import type { AppConfig, ShellProfile, Theme } from "./ipc";

const MIN_FONT_SIZE = 8;
const MAX_FONT_SIZE = 48;
const MIN_LINE_HEIGHT = 1;
const MAX_LINE_HEIGHT = 2.5;
const MAX_SCROLLBACK_LINES = 1_000_000;
const MAX_PROFILE_COUNT = 64;
const MAX_ID_LEN = 128;
const MAX_NAME_LEN = 128;
const MAX_COMMAND_LEN = 4096;
const MAX_ARG_LEN = 4096;
const MAX_ARGS_PER_PROFILE = 128;
const MAX_CWD_LEN = 4096;
const MAX_KEYBINDINGS = 128;
const MAX_KEYBINDING_FIELD_LEN = 128;

export function validateAppConfig(config: AppConfig): string | null {
  return (
    validateNonempty("font family", config.font_family) ??
    validateLen("font family", config.font_family, MAX_NAME_LEN) ??
    validateNoNul("font family", config.font_family) ??
    validateIntegerRange("font size", config.font_size, MIN_FONT_SIZE, MAX_FONT_SIZE) ??
    validateRange("line height", config.line_height, MIN_LINE_HEIGHT, MAX_LINE_HEIGHT) ??
    validateCursorStyle(config.cursor_style) ??
    validateScrollback(config.scrollback_lines) ??
    validateTabLayout(config.tab_layout) ??
    validateTheme(config.theme) ??
    validateProfiles(config.shell_profiles, config.default_shell_profile_id) ??
    validateKeybindings(config.keybindings)
  );
}

function validateTheme(theme: Theme): string | null {
  const colorFields: Array<[keyof Theme, boolean]> = [
    ["background", false],
    ["foreground", false],
    ["cursor", false],
    ["selection", true],
    ["black", false],
    ["red", false],
    ["green", false],
    ["yellow", false],
    ["blue", false],
    ["magenta", false],
    ["cyan", false],
    ["white", false],
    ["bright_black", false],
    ["bright_red", false],
    ["bright_green", false],
    ["bright_yellow", false],
    ["bright_blue", false],
    ["bright_magenta", false],
    ["bright_cyan", false],
    ["bright_white", false],
  ];

  for (const [field, allowAlpha] of colorFields) {
    const error = validateColor(field, theme[field], allowAlpha);
    if (error) return error;
  }
  return null;
}

function validateProfiles(profiles: ShellProfile[], defaultId: string): string | null {
  if (profiles.length === 0) return "at least one shell profile is required";
  if (profiles.length > MAX_PROFILE_COUNT) {
    return `no more than ${MAX_PROFILE_COUNT} shell profiles are allowed`;
  }
  const defaultError = validateNonempty("default shell profile id", defaultId);
  if (defaultError) return defaultError;

  const ids = new Set<string>();
  let defaultExists = false;
  for (const profile of profiles) {
    const error = validateProfile(profile);
    if (error) return error;
    if (ids.has(profile.id)) return `duplicate shell profile id '${profile.id}'`;
    ids.add(profile.id);
    defaultExists ||= profile.id === defaultId;
  }
  return defaultExists ? null : "default shell profile must reference an existing profile";
}

function validateProfile(profile: ShellProfile): string | null {
  if (profile.args.length > MAX_ARGS_PER_PROFILE) {
    return `shell profile '${profile.id}' has too many args`;
  }
  const fields: Array<[string, string, number, boolean]> = [
    ["shell profile id", profile.id, MAX_ID_LEN, true],
    ["shell profile name", profile.name, MAX_NAME_LEN, false],
    ["shell profile command", profile.command, MAX_COMMAND_LEN, false],
  ];
  if (profile.cwd != null) fields.push(["shell profile cwd", profile.cwd, MAX_CWD_LEN, false]);

  for (const [name, value, max, nonempty] of fields) {
    const error =
      (nonempty ? validateNonempty(name, value) : null) ??
      validateLen(name, value, max) ??
      validateNoNul(name, value);
    if (error) return error;
  }

  for (const arg of profile.args) {
    const error =
      validateLen("shell profile arg", arg, MAX_ARG_LEN) ?? validateNoNul("shell profile arg", arg);
    if (error) return error;
  }
  return null;
}

function validateKeybindings(keybindings: AppConfig["keybindings"]): string | null {
  if (keybindings.length > MAX_KEYBINDINGS) {
    return `no more than ${MAX_KEYBINDINGS} keybindings are allowed`;
  }

  const ids = new Set<string>();
  for (const keybinding of keybindings) {
    for (const [name, value] of [
      ["keybinding id", keybinding.id],
      ["keybinding action", keybinding.action],
      ["keybinding keys", keybinding.keys],
    ] as const) {
      const error =
        validateNonempty(name, value) ??
        validateLen(name, value, MAX_KEYBINDING_FIELD_LEN) ??
        validateNoNul(name, value);
      if (error) return error;
    }
    if (ids.has(keybinding.id)) return `duplicate keybinding id '${keybinding.id}'`;
    ids.add(keybinding.id);
  }
  return null;
}

function validateColor(name: string, value: string, allowAlpha: boolean): string | null {
  const validLen = value.length === 7 || (allowAlpha && value.length === 9);
  const valid = validLen && value.startsWith("#") && /^[0-9a-f]+$/i.test(value.slice(1));
  if (valid) return null;
  const expected = allowAlpha ? "#RRGGBB or #RRGGBBAA" : "#RRGGBB";
  return `${name} must be a hex color (${expected})`;
}

function validateCursorStyle(value: string): string | null {
  return value === "block" || value === "bar" || value === "underline"
    ? null
    : "cursor style must be block, bar, or underline";
}

function validateTabLayout(value: string): string | null {
  return value === "horizontal" || value === "vertical"
    ? null
    : "tab layout must be horizontal or vertical";
}

function validateScrollback(value: number): string | null {
  return Number.isInteger(value) && value >= 0 && value <= MAX_SCROLLBACK_LINES
    ? null
    : `scrollback_lines must be no more than ${MAX_SCROLLBACK_LINES}`;
}

function validateIntegerRange(
  name: string,
  value: number,
  min: number,
  max: number,
): string | null {
  return Number.isInteger(value) && value >= min && value <= max
    ? null
    : `${name} must be between ${min} and ${max}`;
}

function validateRange(name: string, value: number, min: number, max: number): string | null {
  return Number.isFinite(value) && value >= min && value <= max
    ? null
    : `${name} must be between ${min} and ${max}`;
}

function validateNonempty(name: string, value: string): string | null {
  return value.trim() ? null : `${name} cannot be empty`;
}

function validateLen(name: string, value: string, max: number): string | null {
  return new TextEncoder().encode(value).length <= max
    ? null
    : `${name} must be at most ${max} bytes`;
}

function validateNoNul(name: string, value: string): string | null {
  return value.includes("\0") ? `${name} cannot contain NUL bytes` : null;
}
