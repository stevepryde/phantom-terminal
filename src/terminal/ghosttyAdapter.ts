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

function internals(term: Terminal): TerminalInternals {
  return term as unknown as TerminalInternals;
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
