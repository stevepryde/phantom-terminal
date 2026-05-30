import { expect, test } from "bun:test";
import {
  DEFAULT_KEYBINDINGS,
  findKeybindingAction,
  matchesKeybinding,
  resolveShortcut,
} from "./keybindings";

test("matches CmdOrCtrl keybindings on non-mac platforms", () => {
  expect(
    matchesKeybinding(key({ key: "t", code: "KeyT", ctrlKey: true }), "CmdOrCtrl+T", false),
  ).toBe(true);
  expect(
    matchesKeybinding(key({ key: "t", code: "KeyT", metaKey: true }), "CmdOrCtrl+T", false),
  ).toBe(false);
});

test("matches named punctuation keys", () => {
  expect(
    matchesKeybinding(key({ key: ",", code: "Comma", ctrlKey: true }), "CmdOrCtrl+Comma", false),
  ).toBe(true);
});

test("findKeybindingAction ignores unknown actions", () => {
  const action = findKeybindingAction(
    key({ key: "k", code: "KeyK", ctrlKey: true }),
    [
      { id: "custom", action: "unknown.action", keys: "CmdOrCtrl+K" },
      { id: "palette", action: "palette.toggle", keys: "CmdOrCtrl+K" },
    ],
    false,
  );

  expect(action).toBe("palette.toggle");
});

test("resolveShortcut maps configured actions before fixed chords", () => {
  expect(
    resolveShortcut(key({ key: "t", code: "KeyT", ctrlKey: true }), DEFAULT_KEYBINDINGS, false),
  ).toEqual({ kind: "action", action: "tab.new" });
});

test("resolveShortcut handles ctrl+Tab cycling in both directions", () => {
  expect(resolveShortcut(key({ code: "Tab", ctrlKey: true }), [], false)).toEqual({
    kind: "activateRelative",
    delta: 1,
  });
  expect(resolveShortcut(key({ code: "Tab", ctrlKey: true, shiftKey: true }), [], false)).toEqual({
    kind: "activateRelative",
    delta: -1,
  });
});

test("resolveShortcut maps macOS bracket navigation only on mac", () => {
  expect(
    resolveShortcut(key({ code: "BracketRight", metaKey: true, shiftKey: true }), [], true),
  ).toEqual({ kind: "activateRelative", delta: 1 });
  expect(
    resolveShortcut(key({ code: "BracketLeft", metaKey: true, shiftKey: true }), [], true),
  ).toEqual({ kind: "activateRelative", delta: -1 });
  // The same chord does nothing off macOS (meta is not the primary modifier).
  expect(
    resolveShortcut(key({ code: "BracketRight", metaKey: true, shiftKey: true }), [], false),
  ).toBeNull();
});

test("resolveShortcut maps modifier+digit to a zero-based tab index", () => {
  // macOS uses Cmd; other platforms use Alt.
  expect(resolveShortcut(key({ code: "Digit3", metaKey: true }), [], true)).toEqual({
    kind: "activateIndex",
    index: 2,
  });
  expect(resolveShortcut(key({ code: "Digit1", altKey: true }), [], false)).toEqual({
    kind: "activateIndex",
    index: 0,
  });
});

test("resolveShortcut returns null for unmapped keys", () => {
  expect(resolveShortcut(key({ key: "a", code: "KeyA" }), DEFAULT_KEYBINDINGS, false)).toBeNull();
});

function key(patch: Partial<KeyboardEvent>): KeyboardEvent {
  return {
    key: "",
    code: "",
    shiftKey: false,
    altKey: false,
    ctrlKey: false,
    metaKey: false,
    ...patch,
  } as KeyboardEvent;
}
