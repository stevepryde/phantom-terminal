import { afterEach, expect, test } from "bun:test";
import type { Terminal } from "ghostty-web";
import { installAdaptiveRenderLoop } from "./ghosttyAdapter";

interface FakeSelectionManager {
  requestRender: () => void;
}

interface FakeTerminal {
  renderer: {
    render: (
      buffer: unknown,
      forceAll?: boolean,
      viewportY?: number,
      scrollbackProvider?: unknown,
      scrollbarOpacity?: number,
    ) => void;
  };
  wasmTerm: unknown;
  viewportY: number;
  scrollbarOpacity: number;
  options: { cursorBlink: boolean };
  selectionManager: FakeSelectionManager;
  write: () => void;
  writeln: () => void;
  input: () => void;
  clear: () => void;
  reset: () => void;
  scrollLines: () => void;
  scrollPages: () => void;
  scrollToTop: () => void;
  scrollToBottom: () => void;
  scrollToLine: () => void;
}

const originalRequestAnimationFrame = globalThis.requestAnimationFrame;
const originalCancelAnimationFrame = globalThis.cancelAnimationFrame;

afterEach(() => {
  globalThis.requestAnimationFrame = originalRequestAnimationFrame;
  globalThis.cancelAnimationFrame = originalCancelAnimationFrame;
});

test("selection render requests schedule an adaptive render frame", () => {
  const frameCallbacks: FrameRequestCallback[] = [];
  globalThis.requestAnimationFrame = (callback: FrameRequestCallback) => {
    frameCallbacks.push(callback);
    return frameCallbacks.length;
  };
  globalThis.cancelAnimationFrame = () => undefined;

  const renderForceModes: boolean[] = [];
  let selectionRequestCount = 0;
  const fakeTerminal: FakeTerminal = {
    renderer: {
      render: (_buffer, forceAll = false) => {
        renderForceModes.push(forceAll);
      },
    },
    wasmTerm: {},
    viewportY: 0,
    scrollbarOpacity: 1,
    options: { cursorBlink: false },
    selectionManager: {
      requestRender: () => {
        selectionRequestCount += 1;
      },
    },
    write: () => undefined,
    writeln: () => undefined,
    input: () => undefined,
    clear: () => undefined,
    reset: () => undefined,
    scrollLines: () => undefined,
    scrollPages: () => undefined,
    scrollToTop: () => undefined,
    scrollToBottom: () => undefined,
    scrollToLine: () => undefined,
  };

  const terminal = fakeTerminal as unknown as Terminal;
  const scheduler = installAdaptiveRenderLoop(terminal, true);
  const internals = terminal as unknown as { startRenderLoop: () => void };

  internals.startRenderLoop();
  const initialFrame = frameCallbacks.shift();
  expect(initialFrame).toBeDefined();
  initialFrame?.(0);
  renderForceModes.length = 0;

  fakeTerminal.selectionManager.requestRender();

  expect(selectionRequestCount).toBe(1);
  expect(renderForceModes).toEqual([]);
  const selectionFrame = frameCallbacks.shift();
  expect(selectionFrame).toBeDefined();

  if (!selectionFrame) throw new Error("selection render frame was not scheduled");
  selectionFrame(0);

  expect(renderForceModes).toEqual([false]);
  scheduler.dispose();
});
