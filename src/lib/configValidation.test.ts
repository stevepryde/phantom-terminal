import { expect, test } from "bun:test";
import { validateAppConfig } from "./configValidation";
import type { AppConfig } from "./ipc";
import { DEFAULT_KEYBINDINGS } from "./keybindings";

test("validates default-shaped config", () => {
  expect(validateAppConfig(config())).toBeNull();
});

test("rejects invalid colors before live-applying config", () => {
  const cfg = config();
  cfg.theme.background = "#fff";

  expect(validateAppConfig(cfg)).toBe("background must be a hex color (#RRGGBB)");
});

test("rejects invalid keybinding fields", () => {
  const cfg = config();
  cfg.keybindings = [{ id: "new-tab", action: "tab.new", keys: "" }];

  expect(validateAppConfig(cfg)).toBe("keybinding keys cannot be empty");
});

test("rejects unknown UI themes before live-applying config", () => {
  const cfg = config();
  cfg.ui_theme = "laser" as AppConfig["ui_theme"];

  expect(validateAppConfig(cfg)).toBe(
    "ui theme must be one of: phantom, aurora, ember, cobalt, verdant, violet, amethyst, ultraviolet, sapphire, glacier, lagoon, emerald, jade, silver",
  );
});

test("rejects unknown terminal backgrounds before live-applying config", () => {
  const cfg = config();
  cfg.terminal_background = "castle" as AppConfig["terminal_background"];

  expect(validateAppConfig(cfg)).toBe("terminal background must be one of: phantom, dragon, none");
});

test("rejects invalid terminal background opacity before live-applying config", () => {
  const cfg = config();
  cfg.terminal_background_opacity = 61;

  expect(validateAppConfig(cfg)).toBe("terminal background opacity must be between 0 and 60");
});

function config(): AppConfig {
  return {
    font_family: "monospace",
    font_size: 14,
    line_height: 1.2,
    cursor_style: "block",
    cursor_blink: true,
    ui_theme: "phantom",
    terminal_background: "phantom",
    terminal_background_opacity: 24,
    theme: {
      background: "#0b0b0e",
      foreground: "#e6e6e6",
      cursor: "#e6e6e6",
      selection: "#ffffff24",
      black: "#1c1c22",
      red: "#ff5c57",
      green: "#5af78e",
      yellow: "#f3f99d",
      blue: "#57c7ff",
      magenta: "#ff6ac1",
      cyan: "#9aedfe",
      white: "#d0d0d0",
      bright_black: "#686868",
      bright_red: "#ff5c57",
      bright_green: "#5af78e",
      bright_yellow: "#f3f99d",
      bright_blue: "#57c7ff",
      bright_magenta: "#ff6ac1",
      bright_cyan: "#9aedfe",
      bright_white: "#f1f1f0",
    },
    shell_profiles: [
      {
        id: "default",
        name: "Default Shell",
        command: "",
        args: [],
        cwd: null,
      },
    ],
    default_shell_profile_id: "default",
    keybindings: DEFAULT_KEYBINDINGS,
    restore_on_launch: true,
    tab_layout: "horizontal",
    window_chrome: "system",
    scrollback_lines: 10_000,
  };
}
