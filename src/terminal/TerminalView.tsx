import { FitAddon, Ghostty, Terminal } from "ghostty-web";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { terminalFontFamilyForRendering } from "../lib/fonts";
import { type AppConfig, ghosttyTheme, ptyKill, ptyResize, ptyWrite, spawnPty } from "../lib/ipc";
import {
  assertGhosttyContract,
  forceTerminalRender,
  refreshTerminalDisplay,
} from "./ghosttyAdapter";
import { enableTerminalLigatures } from "./ligatures";

// ghostty-web's WASM is initialised once for the whole app.
let ghosttyPromise: Promise<Ghostty> | null = null;
function ensureGhostty(): Promise<Ghostty> {
  ghosttyPromise ??= Ghostty.load(import.meta.env.DEV ? undefined : "/ghostty-vt.wasm");
  return ghosttyPromise;
}

function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

interface Props {
  tabId: string;
  cwd: string | null;
  active: boolean;
  config: AppConfig;
  shellProfileId: string | null;
  onSpawn: (tabId: string, ptyId: number) => void;
}

export function TerminalView({ tabId, cwd, active, config, shellProfileId, onSpawn }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const ptyIdRef = useRef<number | null>(null);
  const deadRef = useRef(false);
  const [terminalNotice, setTerminalNotice] = useState<string | null>(null);
  const terminalFontFamily = terminalFontFamilyForRendering(config.font_family);
  // Mirror `active` so the async mount can focus the terminal once it's ready
  // if this tab is still the active one (the focus effect below runs before the
  // terminal exists, so a freshly-created active tab needs this).
  const activeRef = useRef(active);
  activeRef.current = active;

  // Mount: create terminal, spawn PTY, wire data flow. Runs once per tab.
  // biome-ignore lint/correctness/useExhaustiveDependencies: intentional mount-once; config/cwd captured at spawn time, live updates handled by the effects below.
  useEffect(() => {
    let disposed = false;
    const disposers: Array<() => void> = [];
    const markDead = (message: string) => {
      if (disposed || deadRef.current) return;
      deadRef.current = true;
      setTerminalNotice(message);
    };

    (async () => {
      const ghostty = await ensureGhostty().catch((err) => {
        markDead(`Terminal failed to start: ${String(err)}`);
        return null;
      });
      if (!ghostty || disposed || !containerRef.current) return;

      const term = new Terminal({
        ghostty,
        fontFamily: terminalFontFamily,
        fontSize: config.font_size,
        theme: ghosttyTheme(config.theme),
        cursorBlink: config.cursor_blink,
        cursorStyle: config.cursor_style,
        scrollback: config.scrollback_lines,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(containerRef.current);
      // Fail fast (in dev) if a ghostty-web bump changed the internals the
      // adapter depends on, rather than silently rendering with wrong metrics.
      assertGhosttyContract(term);
      enableTerminalLigatures(term);
      term.attachCustomWheelEventHandler((event) =>
        handleMouseTrackingWheel(term, event, containerRef.current),
      );
      termRef.current = term;
      fitRef.current = fit;
      fit.observeResize();

      await nextFrame();
      if (disposed || !containerRef.current) return;
      refreshTerminalDisplay(term, config.line_height);
      fit.fit();
      forceTerminalRender(term);
      scheduleFontSettledRefresh(
        term,
        terminalFontFamily,
        config.font_size,
        config.line_height,
        fit,
      );

      const rows = Math.max(24, term.rows);
      const cols = Math.max(80, term.cols);
      const ptyId = await spawnPty(
        {
          shell_profile_id: shellProfileId,
          cwd,
          rows,
          cols,
        },
        (bytes) => {
          if (bytes.length === 0) {
            markDead("Process exited");
            return;
          }
          term.write(bytes);
        },
      ).catch((err) => {
        markDead(`Process failed to start: ${String(err)}`);
        return null;
      });
      if (ptyId == null) return;
      if (disposed) {
        void ptyKill(ptyId);
        term.dispose();
        return;
      }
      ptyIdRef.current = ptyId;
      if (!deadRef.current) setTerminalNotice(null);
      onSpawn(tabId, ptyId);

      // If this tab is the active one, grab keyboard focus now that the terminal
      // exists. The focus effect below already ran (on mount, before this async
      // block created the terminal), so a freshly-created active tab would
      // otherwise be highlighted but not focused.
      if (activeRef.current) term.focus();

      const enc = new TextEncoder();
      const dData = term.onData((s) => {
        if (deadRef.current) return;
        void ptyWrite(ptyId, enc.encode(s)).catch((err) => {
          markDead(`Process is no longer accepting input: ${String(err)}`);
        });
      });
      const dResize = term.onResize(({ cols, rows }) => {
        if (deadRef.current) return;
        void ptyResize(ptyId, rows, cols).catch((err) => {
          markDead(`Process is no longer accepting resize events: ${String(err)}`);
        });
      });

      disposers.push(
        () => dData.dispose(),
        () => dResize.dispose(),
      );
    })();

    return () => {
      disposed = true;
      for (const d of disposers) d();
      if (ptyIdRef.current != null) void ptyKill(ptyIdRef.current);
      termRef.current?.dispose();
      termRef.current = null;
    };
  }, []);

  // Focus the terminal the instant it becomes active, and re-fit it.
  //
  // Focus must happen *synchronously* inside the activating user gesture (tab
  // click / shortcut keydown). WKWebView only honors programmatic focus() while
  // a user-activation token is live; deferring it to requestAnimationFrame (as
  // we used to) drops that token, so WebKit silently ignores the focus move and
  // keystrokes keep going to the previously-focused terminal — i.e. opening a
  // new tab made every other tab unable to type. useLayoutEffect is flushed
  // synchronously for discrete events, so the gesture is still active here.
  // Only the layout-dependent fit() is left in rAF.
  useLayoutEffect(() => {
    if (!active) return;
    termRef.current?.focus();
    const id = requestAnimationFrame(() => {
      const term = termRef.current;
      if (!term) return;
      refreshTerminalDisplay(term, config.line_height);
      fitRef.current?.fit();
      forceTerminalRender(term);
    });
    return () => cancelAnimationFrame(id);
  }, [active, config.line_height]);

  useEffect(() => {
    const onFocusActiveTerminal = () => {
      if (activeRef.current) termRef.current?.focus();
    };
    window.addEventListener("phantom:focus-active-terminal", onFocusActiveTerminal);
    return () => window.removeEventListener("phantom:focus-active-terminal", onFocusActiveTerminal);
  }, []);

  // Display scale changes do not always trigger a useful ResizeObserver event
  // inside WKWebView. Re-apply metrics and fit when the app-level chrome nudge
  // fires after moving between monitors.
  useEffect(() => {
    const onDisplayLayoutChange = () => {
      const term = termRef.current;
      if (!term) return;
      refreshTerminalDisplay(term, config.line_height);
      fitRef.current?.fit();
      forceTerminalRender(term);

      requestAnimationFrame(() => {
        refreshTerminalDisplay(term, config.line_height);
        fitRef.current?.fit();
        forceTerminalRender(term);
      });
    };

    window.addEventListener("phantom:display-layout-change", onDisplayLayoutChange);
    return () => window.removeEventListener("phantom:display-layout-change", onDisplayLayoutChange);
  }, [config.line_height]);

  // Live theme/font updates without respawning the shell.
  useEffect(() => {
    const term = termRef.current;
    if (term) {
      term.options.fontSize = config.font_size;
      term.options.fontFamily = terminalFontFamily;
      term.options.theme = ghosttyTheme(config.theme);
      term.options.cursorBlink = config.cursor_blink;
      term.options.cursorStyle = config.cursor_style;
      term.options.scrollback = config.scrollback_lines;
      refreshTerminalDisplay(term, config.line_height);
      fitRef.current?.fit();
      forceTerminalRender(term);
      scheduleFontSettledRefresh(
        term,
        terminalFontFamily,
        config.font_size,
        config.line_height,
        fitRef.current,
      );
    }
  }, [
    terminalFontFamily,
    config.font_size,
    config.line_height,
    config.theme,
    config.cursor_blink,
    config.cursor_style,
    config.scrollback_lines,
  ]);

  return (
    <div
      className="terminal-view relative h-full w-full"
      style={{ display: active ? "block" : "none" }}
    >
      <div
        ref={containerRef}
        role="application"
        aria-label="Terminal"
        className="terminal-emulator z-10"
        onMouseDown={() => termRef.current?.focus()}
      />
      <div aria-hidden className="terminal-ui-background" />
      {terminalNotice && (
        <div className="pointer-events-none absolute inset-x-3 bottom-3 z-20 flex justify-center">
          <div
            role="status"
            className="rounded border border-white/10 bg-[#111116]/95 px-3 py-2 text-center text-white/70 text-xs shadow-lg"
          >
            {terminalNotice}
          </div>
        </div>
      )}
    </div>
  );
}
export type { Props as TerminalViewProps };

function scheduleFontSettledRefresh(
  term: Terminal,
  fontFamily: string,
  fontSize: number,
  lineHeight: number,
  fit: FitAddon | null,
) {
  const fonts = document.fonts;
  if (!fonts) return;

  void Promise.race([
    fonts.load(`${fontSize}px ${fontFamily}`),
    fonts.ready,
    new Promise((resolve) => setTimeout(resolve, 250)),
  ])
    .catch(() => undefined)
    .then(() => {
      refreshTerminalDisplay(term, lineHeight);
      fit?.fit();
      forceTerminalRender(term);
    });
}

function handleMouseTrackingWheel(
  term: Terminal,
  event: WheelEvent,
  container: HTMLElement | null,
): boolean {
  if (event.deltaY === 0 || !container || !terminalHasMouseTracking(term)) return false;

  const cell = wheelEventCell(term, event, container);
  if (!cell) return false;

  const button = event.deltaY < 0 ? 64 : 65;
  const sequence = encodeMouseWheel(term, button, cell.col, cell.row, mouseModifiers(event));
  term.input(sequence, true);
  return true;
}

function terminalHasMouseTracking(term: Terminal): boolean {
  try {
    return term.hasMouseTracking();
  } catch {
    return false;
  }
}

function wheelEventCell(
  term: Terminal,
  event: WheelEvent,
  container: HTMLElement,
): { col: number; row: number } | null {
  const canvas = container.querySelector("canvas");
  const rect = (canvas ?? container).getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0 || term.cols <= 0 || term.rows <= 0) return null;

  const cellWidth = rect.width / term.cols;
  const cellHeight = rect.height / term.rows;
  return {
    col: clampCell(Math.floor((event.clientX - rect.left) / cellWidth) + 1, term.cols),
    row: clampCell(Math.floor((event.clientY - rect.top) / cellHeight) + 1, term.rows),
  };
}

function clampCell(value: number, max: number): number {
  return Math.min(max, Math.max(1, value));
}

function mouseModifiers(event: WheelEvent): number {
  let modifiers = 0;
  if (event.shiftKey) modifiers += 4;
  if (event.metaKey) modifiers += 8;
  if (event.ctrlKey) modifiers += 16;
  return modifiers;
}

function encodeMouseWheel(
  term: Terminal,
  button: 64 | 65,
  col: number,
  row: number,
  modifiers: number,
): string {
  if (terminalHasSgrMouseMode(term)) {
    return `\x1B[<${button + modifiers};${col};${row}M`;
  }

  const encodedButton = String.fromCharCode(Math.min(button + modifiers + 32, 255));
  const encodedCol = String.fromCharCode(Math.min(col + 32, 255));
  const encodedRow = String.fromCharCode(Math.min(row + 32, 255));
  return `\x1B[M${encodedButton}${encodedCol}${encodedRow}`;
}

function terminalHasSgrMouseMode(term: Terminal): boolean {
  try {
    return term.getMode(1006, false);
  } catch {
    return true;
  }
}
