import type { Terminal } from "ghostty-web";

enum CellFlags {
  Bold = 1,
  Italic = 2,
  Underline = 4,
  Strikethrough = 8,
  Inverse = 16,
  Invisible = 32,
  Faint = 128,
}

interface RenderCell {
  codepoint: number;
  fg_r: number;
  fg_g: number;
  fg_b: number;
  bg_r: number;
  bg_g: number;
  bg_b: number;
  flags: number;
  width: number;
  hyperlink_id: number;
  grapheme_len: number;
}

type RenderLine = RenderCell[];

interface RenderBuffer {
  getCursor?: () => { x: number; y: number; visible: boolean };
  getLine?: (row: number) => RenderLine | null;
  getGraphemeString?: (row: number, col: number) => string;
}

interface LigatureRenderer {
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  metrics: { width: number; height: number; baseline: number };
  fontSize: number;
  fontFamily: string;
  theme: {
    background: string;
    selectionBackground: string;
  };
  currentBuffer: RenderBuffer | null;
  devicePixelRatio?: number;
  hoveredHyperlinkId: number;
  previousVisualTrimTopRows?: number;
  visualTrimTopRows?: number;
  render: (
    buffer: RenderBuffer,
    forceAll?: boolean,
    viewportY?: number,
    scrollbackProvider?: unknown,
    scrollbarOpacity?: number,
  ) => void;
  renderCursor: (col: number, row: number) => void;
  renderLine: (line: RenderLine, row: number, cols: number) => void;
  renderCellBackground: (cell: RenderCell, col: number, row: number) => void;
  isInSelection: (col: number, row: number) => boolean;
  rgbToCSS: (r: number, g: number, b: number) => string;
}

interface TerminalInternals {
  renderer?: LigatureRenderer;
}

const patchedRenderers = new WeakSet<LigatureRenderer>();

export function enableTerminalLigatures(term: Terminal) {
  const renderer = (term as unknown as TerminalInternals).renderer;
  if (!renderer || patchedRenderers.has(renderer)) return;
  patchedRenderers.add(renderer);

  const render = renderer.render.bind(renderer);
  renderer.render = function renderWithTopTrim(
    buffer,
    forceAll = false,
    viewportY = 0,
    scrollbackProvider,
    scrollbarOpacity,
  ) {
    const trimRows = shouldTrimTopRow(buffer, viewportY) ? 1 : 0;
    const trimChanged = trimRows !== (this.previousVisualTrimTopRows ?? 0);
    this.visualTrimTopRows = trimRows;
    this.previousVisualTrimTopRows = trimRows;

    if (trimChanged || forceAll) {
      this.ctx.fillStyle = this.theme.background;
      const ratio = this.devicePixelRatio ?? window.devicePixelRatio ?? 1;
      this.ctx.fillRect(0, 0, this.canvas.width / ratio, this.canvas.height / ratio);
    }
    render(buffer, forceAll || trimChanged, viewportY, scrollbackProvider, scrollbarOpacity);
  };

  const renderCursor = renderer.renderCursor.bind(renderer);
  renderer.renderCursor = function renderCursorWithTopTrim(col, row) {
    renderCursor(col, Math.max(0, row - (this.visualTrimTopRows ?? 0)));
  };

  renderer.renderLine = function renderLineWithLigatures(line, row, cols) {
    const visualRow = row - (this.visualTrimTopRows ?? 0);
    if (visualRow < 0) return;

    const y = visualRow * this.metrics.height;
    this.ctx.fillStyle = this.theme.background;
    this.ctx.fillRect(0, y, cols * this.metrics.width, this.metrics.height);

    for (let col = 0; col < line.length; col++) {
      const cell = line[col];
      if (cell.width !== 0) drawCellBackground(this, cell, col, row, visualRow);
    }

    for (let col = 0; col < line.length; ) {
      const run = collectTextRun(this, line, row, col);
      if (run) {
        drawTextRun(this, run, visualRow);
        col = run.endCol;
      } else {
        drawCellText(this, line[col], col, row, visualRow);
        col += 1;
      }
    }
  };
}

function shouldTrimTopRow(buffer: RenderBuffer, viewportY: number): boolean {
  if (viewportY !== 0 || !buffer.getLine) return false;
  const firstLine = buffer.getLine(0);
  const secondLine = buffer.getLine(1);
  if (!firstLine || !secondLine) return false;
  if (lineHasText(firstLine)) return false;
  if (!lineHasText(secondLine)) return false;
  const cursor = buffer.getCursor?.();
  return !cursor || cursor.y > 0;
}

function lineHasText(line: RenderLine): boolean {
  return line.some((cell) => {
    if (!cell || cell.width === 0) return false;
    if (cell.grapheme_len > 0) return true;
    return cell.codepoint !== 0 && cell.codepoint !== 32;
  });
}

interface TextRun {
  startCol: number;
  endCol: number;
  text: string;
  cell: RenderCell;
}

function collectTextRun(
  renderer: LigatureRenderer,
  line: RenderLine,
  row: number,
  startCol: number,
): TextRun | null {
  const first = line[startCol];
  if (!isRunCell(first)) return null;

  const selected = renderer.isInSelection(startCol, row);
  const style = styleKey(first, selected);
  let text = cellText(renderer, first, row, startCol);
  let endCol = startCol + 1;

  while (endCol < line.length) {
    const next = line[endCol];
    if (!isRunCell(next)) break;
    const nextSelected = renderer.isInSelection(endCol, row);
    if (nextSelected !== selected || styleKey(next, nextSelected) !== style) break;
    text += cellText(renderer, next, row, endCol);
    endCol += 1;
  }

  return endCol - startCol >= 2 ? { startCol, endCol, text, cell: first } : null;
}

function isRunCell(cell: RenderCell): boolean {
  if (!cell || cell.width !== 1) return false;
  if (cell.flags & CellFlags.Invisible) return false;
  if (cell.flags & CellFlags.Underline) return false;
  if (cell.flags & CellFlags.Strikethrough) return false;
  if (cell.hyperlink_id > 0) return false;
  return true;
}

function styleKey(cell: RenderCell, selected: boolean): string {
  return [
    cell.flags,
    selected,
    cell.fg_r,
    cell.fg_g,
    cell.fg_b,
    cell.bg_r,
    cell.bg_g,
    cell.bg_b,
  ].join(":");
}

function drawTextRun(renderer: LigatureRenderer, run: TextRun, row: number) {
  prepareText(renderer, run.cell);
  const x = run.startCol * renderer.metrics.width;
  const y = row * renderer.metrics.height + renderer.metrics.baseline;
  renderer.ctx.fillText(run.text, x, y);
  if (run.cell.flags & CellFlags.Faint) renderer.ctx.globalAlpha = 1;
}

function drawCellText(
  renderer: LigatureRenderer,
  cell: RenderCell,
  col: number,
  row: number,
  visualRow: number,
) {
  if (cell.width === 0 || cell.flags & CellFlags.Invisible) return;

  prepareText(renderer, cell);

  const x = col * renderer.metrics.width;
  const y = visualRow * renderer.metrics.height;
  const text = cellText(renderer, cell, row, col);
  renderer.ctx.fillText(text, x, y + renderer.metrics.baseline);
  if (cell.flags & CellFlags.Faint) renderer.ctx.globalAlpha = 1;

  const cellWidth = renderer.metrics.width * cell.width;
  if (cell.flags & CellFlags.Underline) {
    drawDecoration(renderer, x, y + renderer.metrics.baseline + 2, cellWidth);
  }
  if (cell.flags & CellFlags.Strikethrough) {
    drawDecoration(renderer, x, y + renderer.metrics.height / 2, cellWidth);
  }
  if (cell.hyperlink_id > 0 && cell.hyperlink_id === renderer.hoveredHyperlinkId) {
    renderer.ctx.strokeStyle = "#4A90E2";
    drawLine(renderer.ctx, x, y + renderer.metrics.baseline + 2, cellWidth);
  }
}

function drawCellBackground(
  renderer: LigatureRenderer,
  cell: RenderCell,
  col: number,
  row: number,
  visualRow: number,
) {
  const x = col * renderer.metrics.width;
  const y = visualRow * renderer.metrics.height;
  const width = renderer.metrics.width * cell.width;

  if (renderer.isInSelection(col, row)) {
    renderer.ctx.fillStyle = renderer.theme.selectionBackground;
    renderer.ctx.fillRect(x, y, width, renderer.metrics.height);
    return;
  }

  let r = cell.bg_r;
  let g = cell.bg_g;
  let b = cell.bg_b;
  if (cell.flags & CellFlags.Inverse) {
    r = cell.fg_r;
    g = cell.fg_g;
    b = cell.fg_b;
  }
  if (r !== 0 || g !== 0 || b !== 0) {
    renderer.ctx.fillStyle = renderer.rgbToCSS(r, g, b);
    renderer.ctx.fillRect(x, y, width, renderer.metrics.height);
  }
}

function prepareText(renderer: LigatureRenderer, cell: RenderCell) {
  let style = "";
  if (cell.flags & CellFlags.Italic) style += "italic ";
  if (cell.flags & CellFlags.Bold) style += "bold ";
  renderer.ctx.font = `${style}${renderer.fontSize}px ${renderer.fontFamily}`;
  renderer.ctx.fontKerning = "normal";

  let r = cell.fg_r;
  let g = cell.fg_g;
  let b = cell.fg_b;
  if (cell.flags & CellFlags.Inverse) {
    r = cell.bg_r;
    g = cell.bg_g;
    b = cell.bg_b;
  }
  renderer.ctx.fillStyle = renderer.rgbToCSS(r, g, b);

  if (cell.flags & CellFlags.Faint) renderer.ctx.globalAlpha = 0.5;
}

function cellText(renderer: LigatureRenderer, cell: RenderCell, row: number, col: number): string {
  if (cell.grapheme_len > 0 && renderer.currentBuffer?.getGraphemeString) {
    return renderer.currentBuffer.getGraphemeString(row, col);
  }
  return String.fromCodePoint(cell.codepoint || 32);
}

function drawDecoration(renderer: LigatureRenderer, x: number, y: number, width: number) {
  renderer.ctx.strokeStyle = renderer.ctx.fillStyle;
  drawLine(renderer.ctx, x, y, width);
}

function drawLine(ctx: CanvasRenderingContext2D, x: number, y: number, width: number) {
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(x, y);
  ctx.lineTo(x + width, y);
  ctx.stroke();
}
