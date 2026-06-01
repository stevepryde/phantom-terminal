import type { FontMetrics, Terminal } from "ghostty-web";

// The ONLY module allowed to touch ghostty-web's private internals.
//
// ghostty-web is pinned at a pre-release (0.4.0-next.*) and exposes no public
// API for per-line line-height adjustment or for forcing a synchronous
// re-render. We reach into a few internals to do both. Concentrating every cast
// and undocumented field access here (ROADMAP MAINT-2) keeps the rest of the
// terminal code typed against a narrow, documented surface, and gives us one
// place to add runtime guards so a version bump that removes an internal fails
// loudly instead of silently rendering with the wrong metrics.

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
  wasmTerm?: unknown;
  viewportY?: number;
}

interface SelectionRenderManager {
  requestRender?: () => void;
}

interface RenderLoopTerminalInternals extends TerminalInternals {
  animationFrameId?: number;
  cancelRenderLoop?: () => void;
  startRenderLoop?: () => void;
  options?: Terminal["options"];
  scrollbarOpacity?: number;
  selectionManager?: SelectionRenderManager;
  write: Terminal["write"];
  writeln: Terminal["writeln"];
  input: Terminal["input"];
  clear: Terminal["clear"];
  reset: Terminal["reset"];
  scrollLines: Terminal["scrollLines"];
  scrollPages: Terminal["scrollPages"];
  scrollToTop: Terminal["scrollToTop"];
  scrollToBottom: Terminal["scrollToBottom"];
  scrollToLine: Terminal["scrollToLine"];
}

export interface TerminalRenderScheduler {
  schedule: (forceAll?: boolean) => void;
  setActive: (active: boolean) => void;
  syncCursorBlink: () => void;
  dispose: () => void;
}

const adaptiveRenderSchedulers = new WeakMap<Terminal, TerminalRenderScheduler>();
const selectionRenderManagers = new WeakSet<SelectionRenderManager>();

function internals(term: Terminal): TerminalInternals {
  return term as unknown as TerminalInternals;
}

function renderLoopInternals(term: Terminal): RenderLoopTerminalInternals {
  return term as unknown as RenderLoopTerminalInternals;
}

/**
 * Verify the ghostty internals we depend on are present and shaped as expected.
 * Returns the list of missing pieces (empty when the contract holds).
 *
 * `assertGhosttyContract` calls this and fails fast in development — the moment
 * to catch a broken internal is when bumping the pre-release dep, not in a user
 * session — while shipped builds degrade to default rendering rather than brick.
 */
export function checkGhosttyContract(term: Terminal): string[] {
  const missing: string[] = [];
  const renderer = internals(term).renderer;
  if (!renderer) {
    missing.push("renderer");
    return missing;
  }
  for (const method of ["getMetrics", "remeasureFont", "resize", "render"] as const) {
    if (typeof renderer[method] !== "function") missing.push(`renderer.${method}`);
  }
  return missing;
}

/**
 * One-time smoke check, run right after a terminal is opened. Throws in DEV so a
 * version bump that drops an internal fails fast with a clear message; in
 * production it logs once and lets the per-call guards degrade gracefully.
 */
export function assertGhosttyContract(term: Terminal): void {
  const missing = checkGhosttyContract(term);
  if (missing.length === 0) return;

  const message =
    `ghostty-web internals changed — missing ${missing.join(", ")}. ` +
    "Line-height and forced rendering in terminal/ghosttyAdapter.ts need updating " +
    "for this version of ghostty-web.";
  if (import.meta.env.DEV) throw new Error(message);
  console.error(`phantom: ${message}`);
}

/**
 * ghostty-web 0.4 starts a permanent requestAnimationFrame loop for every
 * opened terminal. With cursor blinking enabled, that loop repaints a canvas row
 * every display frame even when the PTY is idle. On Linux/WebKitGTK this can
 * become compositor/GPU pressure that makes other browser-based apps stutter
 * while Phantom's own CPU usage still looks low.
 *
 * Replace that loop with an on-demand scheduler: output, resize, scroll, config
 * changes, and activation render promptly, while an idle blinking cursor renders
 * only at blink cadence.
 */
export function installAdaptiveRenderLoop(
  term: Terminal,
  initialActive: boolean,
): TerminalRenderScheduler {
  const existing = adaptiveRenderSchedulers.get(term);
  if (existing) {
    existing.setActive(initialActive);
    return existing;
  }

  const terminal = renderLoopInternals(term);
  let active = initialActive;
  let disposed = false;
  let scheduledFrame: number | null = null;
  let scheduledForceAll = false;
  let blinkTimer: ReturnType<typeof setInterval> | null = null;

  const original = {
    write: terminal.write.bind(term),
    writeln: terminal.writeln.bind(term),
    input: terminal.input.bind(term),
    clear: terminal.clear.bind(term),
    reset: terminal.reset.bind(term),
    scrollLines: terminal.scrollLines.bind(term),
    scrollPages: terminal.scrollPages.bind(term),
    scrollToTop: terminal.scrollToTop.bind(term),
    scrollToBottom: terminal.scrollToBottom.bind(term),
    scrollToLine: terminal.scrollToLine.bind(term),
  };

  const clearScheduledFrame = () => {
    if (scheduledFrame == null) return;
    cancelAnimationFrame(scheduledFrame);
    scheduledFrame = null;
  };

  const renderNow = (forceAll = false) => {
    if (disposed || !active) return;
    const { renderer, wasmTerm, viewportY, scrollbarOpacity } = renderLoopInternals(term);
    if (!renderer || !wasmTerm) return;
    renderer.render(wasmTerm, forceAll, viewportY, term, scrollbarOpacity);
  };

  const schedule = (forceAll = false) => {
    scheduledForceAll ||= forceAll;
    if (disposed || !active || scheduledFrame != null) return;
    scheduledFrame = requestAnimationFrame(() => {
      scheduledFrame = null;
      const shouldForceAll = scheduledForceAll;
      scheduledForceAll = false;
      renderNow(shouldForceAll);
    });
  };

  const syncCursorBlink = () => {
    if (blinkTimer != null) {
      clearInterval(blinkTimer);
      blinkTimer = null;
    }
    if (disposed || !active || !renderLoopInternals(term).options?.cursorBlink) return;
    blinkTimer = setInterval(() => renderNow(false), 530);
  };

  terminal.cancelRenderLoop = () => {
    const frame = renderLoopInternals(term).animationFrameId;
    if (frame != null) {
      cancelAnimationFrame(frame);
      renderLoopInternals(term).animationFrameId = undefined;
    }
    clearScheduledFrame();
  };
  terminal.startRenderLoop = () => {
    installSelectionRenderHook(term, schedule);
    schedule(false);
    syncCursorBlink();
  };

  terminal.write = ((data, callback) => {
    original.write(data, callback);
    schedule(false);
  }) as Terminal["write"];
  terminal.writeln = ((data, callback) => {
    original.writeln(data, callback);
    schedule(false);
  }) as Terminal["writeln"];
  terminal.input = ((data, wasUserInput) => {
    original.input(data, wasUserInput);
    schedule(false);
  }) as Terminal["input"];
  terminal.clear = (() => {
    original.clear();
    schedule(true);
  }) as Terminal["clear"];
  terminal.reset = (() => {
    original.reset();
    schedule(true);
  }) as Terminal["reset"];
  terminal.scrollLines = ((amount) => {
    original.scrollLines(amount);
    schedule(false);
  }) as Terminal["scrollLines"];
  terminal.scrollPages = ((amount) => {
    original.scrollPages(amount);
    schedule(false);
  }) as Terminal["scrollPages"];
  terminal.scrollToTop = (() => {
    original.scrollToTop();
    schedule(false);
  }) as Terminal["scrollToTop"];
  terminal.scrollToBottom = (() => {
    original.scrollToBottom();
    schedule(false);
  }) as Terminal["scrollToBottom"];
  terminal.scrollToLine = ((line) => {
    original.scrollToLine(line);
    schedule(false);
  }) as Terminal["scrollToLine"];

  const scheduler: TerminalRenderScheduler = {
    schedule,
    setActive(nextActive) {
      if (active === nextActive) return;
      active = nextActive;
      if (active) {
        schedule(true);
      } else {
        clearScheduledFrame();
      }
      syncCursorBlink();
    },
    syncCursorBlink,
    dispose() {
      disposed = true;
      clearScheduledFrame();
      if (blinkTimer != null) {
        clearInterval(blinkTimer);
        blinkTimer = null;
      }
      adaptiveRenderSchedulers.delete(term);
    },
  };

  adaptiveRenderSchedulers.set(term, scheduler);
  return scheduler;
}

function installSelectionRenderHook(
  term: Terminal,
  schedule: TerminalRenderScheduler["schedule"],
): void {
  const selectionManager = renderLoopInternals(term).selectionManager;
  if (!selectionManager || selectionRenderManagers.has(selectionManager)) return;

  const requestRender = selectionManager.requestRender?.bind(selectionManager);
  selectionManager.requestRender = () => {
    requestRender?.();
    schedule(false);
  };
  selectionRenderManagers.add(selectionManager);
}

/**
 * Re-measure the font, apply the configured line-height by padding metric
 * height (centering the baseline), and re-render. No-op if the renderer internal
 * is absent.
 */
export function refreshTerminalDisplay(term: Terminal, lineHeight: number): void {
  const renderer = internals(term).renderer;
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

/** Force a synchronous full re-render. No-op if the internals are absent. */
export function forceTerminalRender(term: Terminal): void {
  const { renderer, wasmTerm, viewportY } = internals(term);
  if (!renderer || !wasmTerm) return;
  renderer.render(wasmTerm, true, viewportY, term);
}
