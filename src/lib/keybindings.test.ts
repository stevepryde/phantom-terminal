import { expect, test } from "bun:test";
import { findKeybindingAction, matchesKeybinding } from "./keybindings";

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
