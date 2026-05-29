import { FitAddon, type FontMetrics, Ghostty, Terminal } from "ghostty-web";
import { useEffect, useLayoutEffect, useRef } from "react";
import { type AppConfig, ghosttyTheme, ptyKill, ptyResize, ptyWrite, spawnPty } from "../lib/ipc";

// ghostty-web's WASM is initialised once for the whole app.
let ghosttyPromise: Promise<Ghostty> | null = null;
function ensureGhostty(): Promise<Ghostty> {
  ghosttyPromise ??= Ghostty.load(import.meta.env.DEV ? undefined : "/ghostty-vt.wasm");
  return ghosttyPromise;
}

function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

interface LineHeightRenderer {
  devicePixelRatio?: number;
  metrics?: FontMetrics;
  getMetrics: () => FontMetrics;
  render: (
    buffer: unknown,
    forceAll?: boolean,
    viewportY?: number,
    scrollbackProvider?: unknown,
    scrollbarOpacity?: number,
  ) => void;
  remeasureFont: () => void;
  resize: (cols: number, rows: number) => void;
}

interface TerminalInternals {
  renderer?: LineHeightRenderer;
}

interface Props {
  tabId: string;
  cwd: string;
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

    (async () => {
      const ghostty = await ensureGhostty();
      if (disposed || !containerRef.current) return;

      const term = new Terminal({
        ghostty,
        fontFamily: config.font_family,
        fontSize: config.font_size,
        theme: ghosttyTheme(config.theme),
        cursorBlink: config.cursor_blink,
        cursorStyle: config.cursor_style,
        scrollback: config.scrollback_lines,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(containerRef.current);
      termRef.current = term;
      fitRef.current = fit;
      fit.observeResize();

      await nextFrame();
      if (disposed || !containerRef.current) return;
      refreshTerminalDisplay(term, config.line_height);
      fit.fit();
      forceTerminalRender(term);

      const ptyId = await spawnPty(
        {
          shell_profile_id: shellProfileId,
          cwd: cwd || null,
          rows: Math.max(24, term.rows),
          cols: Math.max(80, term.cols),
        },
        (bytes) => term.write(bytes),
      );
      if (disposed) {
        ptyKill(ptyId);
        term.dispose();
        return;
      }
      ptyIdRef.current = ptyId;
      onSpawn(tabId, ptyId);

      // If this tab is the active one, grab keyboard focus now that the terminal
      // exists. The focus effect below already ran (on mount, before this async
      // block created the terminal), so a freshly-created active tab would
      // otherwise be highlighted but not focused.
      if (activeRef.current) term.focus();

      const enc = new TextEncoder();
      const dData = term.onData((s) => {
        ptyWrite(ptyId, enc.encode(s));
      });
      const dResize = term.onResize(({ cols, rows }) => {
        ptyResize(ptyId, rows, cols);
      });

      disposers.push(
        () => dData.dispose(),
        () => dResize.dispose(),
      );
    })();

    return () => {
      disposed = true;
      for (const d of disposers) d();
      if (ptyIdRef.current != null) ptyKill(ptyIdRef.current);
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
      term.options.fontFamily = config.font_family;
      term.options.theme = ghosttyTheme(config.theme);
      term.options.cursorBlink = config.cursor_blink;
      term.options.cursorStyle = config.cursor_style;
      term.options.scrollback = config.scrollback_lines;
      refreshTerminalDisplay(term, config.line_height);
      fitRef.current?.fit();
      forceTerminalRender(term);
    }
  }, [
    config.font_family,
    config.font_size,
    config.line_height,
    config.theme,
    config.cursor_blink,
    config.cursor_style,
    config.scrollback_lines,
  ]);

  return (
    <div
      ref={containerRef}
      role="application"
      aria-label="Terminal"
      className="h-full w-full"
      style={{ display: active ? "block" : "none" }}
      onMouseDown={() => termRef.current?.focus()}
    />
  );
}

// Keep onExit referenced for future PTY-exit handling.
export type { Props as TerminalViewProps };

function refreshTerminalDisplay(term: Terminal, lineHeight: number) {
  const renderer = (term as unknown as TerminalInternals).renderer;
  if (!renderer) return;

  renderer.devicePixelRatio = window.devicePixelRatio || 1;
  renderer.remeasureFont();
  const metrics = renderer.getMetrics();
  const height = Math.max(metrics.height, Math.ceil(metrics.height * lineHeight));
  const extra = height - metrics.height;
  renderer.metrics = {
    ...metrics,
    height,
    baseline: metrics.baseline + Math.floor(extra / 2),
  };
  renderer.resize(term.cols, term.rows);
  forceTerminalRender(term);
}

function forceTerminalRender(term: Terminal) {
  const renderer = (term as unknown as TerminalInternals).renderer;
  if (!renderer || !term.wasmTerm) return;
  renderer.render(term.wasmTerm, true, term.viewportY, term);
}
