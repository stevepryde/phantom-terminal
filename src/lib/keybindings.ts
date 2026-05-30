import type { Keybinding } from "./ipc";

export type KeybindingAction =
  | "tab.new"
  | "tab.close"
  | "tab.rename"
  | "palette.toggle"
  | "settings.toggle";

export const KEYBINDING_ACTION_LABELS: Record<KeybindingAction, string> = {
  "tab.new": "New tab",
  "tab.close": "Close tab",
  "tab.rename": "Rename tab",
  "palette.toggle": "Command palette",
  "settings.toggle": "Settings",
};

export const DEFAULT_KEYBINDINGS: Keybinding[] = [
  { id: "new-tab", action: "tab.new", keys: "CmdOrCtrl+T" },
  { id: "close-tab", action: "tab.close", keys: "CmdOrCtrl+W" },
  { id: "rename-tab", action: "tab.rename", keys: "F2" },
  { id: "command-palette", action: "palette.toggle", keys: "CmdOrCtrl+K" },
  { id: "settings", action: "settings.toggle", keys: "CmdOrCtrl+Comma" },
];

const ACTIONS = new Set<string>(Object.keys(KEYBINDING_ACTION_LABELS));
const isMac =
  typeof navigator !== "undefined" && /mac/i.test(navigator.platform || navigator.userAgent);

export function isKnownKeybindingAction(action: string): action is KeybindingAction {
  return ACTIONS.has(action);
}

export function findKeybindingAction(
  event: KeyboardEvent,
  keybindings: Keybinding[],
  platformIsMac = isMac,
): KeybindingAction | null {
  for (const keybinding of keybindings) {
    if (!isKnownKeybindingAction(keybinding.action)) continue;
    if (matchesKeybinding(event, keybinding.keys, platformIsMac)) return keybinding.action;
  }
  return null;
}

/**
 * A resolved keyboard command. Configurable `action`s come from
 * `config.keybindings`; the navigation commands below are fixed shortcuts that
 * are intentionally not user-rebindable (ctrl+Tab cycling, ⌘⇧[ / ⌘⇧] on macOS,
 * and the numeric "jump to tab N" modifier+digit chords).
 */
export type ShortcutCommand =
  | { kind: "action"; action: KeybindingAction }
  | { kind: "activateRelative"; delta: number }
  | { kind: "activateIndex"; index: number };

/**
 * Map a keydown to the command it should run, or null when nothing matches.
 * Pure and platform-parameterized so the keyboard layer can be tested without a
 * DOM. Configurable actions take precedence over the fixed navigation chords.
 */
export function resolveShortcut(
  event: KeyboardEvent,
  keybindings: Keybinding[],
  platformIsMac = isMac,
): ShortcutCommand | null {
  const action = findKeybindingAction(event, keybindings, platformIsMac);
  if (action) return { kind: "action", action };

  if (event.ctrlKey && event.code === "Tab") {
    return { kind: "activateRelative", delta: event.shiftKey ? -1 : 1 };
  }
  if (platformIsMac && event.metaKey && event.shiftKey && event.code === "BracketLeft") {
    return { kind: "activateRelative", delta: -1 };
  }
  if (platformIsMac && event.metaKey && event.shiftKey && event.code === "BracketRight") {
    return { kind: "activateRelative", delta: 1 };
  }
  const numMod = platformIsMac ? event.metaKey : event.altKey;
  if (numMod && /^Digit[1-9]$/.test(event.code)) {
    return { kind: "activateIndex", index: Number(event.code.slice(5)) - 1 };
  }
  return null;
}

export function matchesKeybinding(
  event: KeyboardEvent,
  keys: string,
  platformIsMac = isMac,
): boolean {
  const parsed = parseKeybinding(keys);
  if (!parsed) return false;

  return (
    event.shiftKey === parsed.shift &&
    event.altKey === parsed.alt &&
    event.ctrlKey === (parsed.ctrl || (parsed.primary && !platformIsMac)) &&
    event.metaKey === (parsed.meta || (parsed.primary && platformIsMac)) &&
    eventKeyToken(event) === parsed.key
  );
}

interface ParsedKeybinding {
  primary: boolean;
  meta: boolean;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  key: string;
}

function parseKeybinding(keys: string): ParsedKeybinding | null {
  const parts = keys
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean);
  if (parts.length === 0) return null;

  const parsed: ParsedKeybinding = {
    primary: false,
    meta: false,
    ctrl: false,
    alt: false,
    shift: false,
    key: "",
  };

  for (const part of parts) {
    const token = normalizeToken(part);
    if (token === "cmdorctrl" || token === "mod") {
      parsed.primary = true;
    } else if (token === "cmd" || token === "command" || token === "meta" || token === "super") {
      parsed.meta = true;
    } else if (token === "ctrl" || token === "control") {
      parsed.ctrl = true;
    } else if (token === "alt" || token === "option") {
      parsed.alt = true;
    } else if (token === "shift") {
      parsed.shift = true;
    } else if (!parsed.key) {
      parsed.key = normalizeKeyToken(part);
    } else {
      return null;
    }
  }

  return parsed.key ? parsed : null;
}

function eventKeyToken(event: KeyboardEvent): string {
  if (/^Digit[0-9]$/.test(event.code)) return event.code.slice(5);
  if (/^Key[A-Z]$/.test(event.code)) return event.code.slice(3).toLowerCase();
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(event.key)) return event.key.toLowerCase();
  if (event.code === "BracketLeft") return "[";
  if (event.code === "BracketRight") return "]";
  return normalizeKeyToken(event.key);
}

function normalizeKeyToken(token: string): string {
  const normalized = normalizeToken(token);
  if (normalized === "comma") return ",";
  if (normalized === "period") return ".";
  if (normalized === "space") return " ";
  if (normalized === "tab") return "tab";
  if (normalized === "escape" || normalized === "esc") return "escape";
  if (normalized === "bracketleft") return "[";
  if (normalized === "bracketright") return "]";
  if (/^f([1-9]|1[0-9]|2[0-4])$/.test(normalized)) return normalized;
  return normalized;
}

function normalizeToken(token: string): string {
  return token.trim().toLowerCase().replace(/\s+/g, "");
}
