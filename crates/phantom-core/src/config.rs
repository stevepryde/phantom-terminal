use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const MIN_FONT_SIZE: u16 = 8;
const MAX_FONT_SIZE: u16 = 48;
const MIN_LINE_HEIGHT: f32 = 1.0;
const MAX_LINE_HEIGHT: f32 = 2.5;
const MAX_SCROLLBACK_LINES: u32 = 1_000_000;
const MAX_PROFILE_COUNT: usize = 64;
const MAX_ID_LEN: usize = 128;
const MAX_NAME_LEN: usize = 128;
const MAX_COMMAND_LEN: usize = 4096;
const MAX_ARG_LEN: usize = 4096;
const MAX_ARGS_PER_PROFILE: usize = 128;
const MAX_CWD_LEN: usize = 4096;
const MAX_KEYBINDINGS: usize = 128;
const MAX_KEYBINDING_FIELD_LEN: usize = 128;
const OLD_DEFAULT_SELECTION: &str = "#33415580";
const DEFAULT_SELECTION: &str = "#ffffff24";
const DEFAULT_UI_THEME: &str = "phantom";
const DEFAULT_TERMINAL_BACKGROUND: &str = "phantom";
const MAX_TERMINAL_BACKGROUND_OPACITY: u8 = 60;
const DEFAULT_TERMINAL_BACKGROUND_OPACITY: u8 = 24;
const DEFAULT_WINDOW_CHROME: &str = "system";
/// Panel opacity is a percentage. Below 100 the egui panels are translucent and
/// the renderer blurs whatever shows through behind them. The floor keeps panels
/// legible; 100 is fully opaque and disables the backdrop blur entirely.
const MIN_PANEL_OPACITY: u8 = 50;
const MAX_PANEL_OPACITY: u8 = 100;
const DEFAULT_PANEL_OPACITY: u8 = 85;

/// Full 16-color ANSI palette plus the special UI colors. Keys are snake_case on
/// the wire; the frontend maps them onto ghostty-web's camelCase Terminal theme.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

impl Theme {
    pub fn validate(&self) -> AppResult<()> {
        validate_color("background", &self.background, false)?;
        validate_color("foreground", &self.foreground, false)?;
        validate_color("cursor", &self.cursor, false)?;
        validate_color("selection", &self.selection, true)?;
        validate_color("black", &self.black, false)?;
        validate_color("red", &self.red, false)?;
        validate_color("green", &self.green, false)?;
        validate_color("yellow", &self.yellow, false)?;
        validate_color("blue", &self.blue, false)?;
        validate_color("magenta", &self.magenta, false)?;
        validate_color("cyan", &self.cyan, false)?;
        validate_color("white", &self.white, false)?;
        validate_color("bright_black", &self.bright_black, false)?;
        validate_color("bright_red", &self.bright_red, false)?;
        validate_color("bright_green", &self.bright_green, false)?;
        validate_color("bright_yellow", &self.bright_yellow, false)?;
        validate_color("bright_blue", &self.bright_blue, false)?;
        validate_color("bright_magenta", &self.bright_magenta, false)?;
        validate_color("bright_cyan", &self.bright_cyan, false)?;
        validate_color("bright_white", &self.bright_white, false)?;
        Ok(())
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: "#0b0b0e".to_string(),
            foreground: "#e6e6e6".to_string(),
            cursor: "#e6e6e6".to_string(),
            selection: DEFAULT_SELECTION.to_string(),
            black: "#1c1c22".to_string(),
            red: "#ff5c57".to_string(),
            green: "#5af78e".to_string(),
            yellow: "#f3f99d".to_string(),
            blue: "#57c7ff".to_string(),
            magenta: "#ff6ac1".to_string(),
            cyan: "#9aedfe".to_string(),
            white: "#d0d0d0".to_string(),
            bright_black: "#686868".to_string(),
            bright_red: "#ff5c57".to_string(),
            bright_green: "#5af78e".to_string(),
            bright_yellow: "#f3f99d".to_string(),
            bright_blue: "#57c7ff".to_string(),
            bright_magenta: "#ff6ac1".to_string(),
            bright_cyan: "#9aedfe".to_string(),
            bright_white: "#f1f1f0".to_string(),
        }
    }
}

/// A named shell launch configuration. `command` is the executable; `args` are
/// passed verbatim. An empty `command` means "use the user's $SHELL".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keybinding {
    pub id: String,
    pub action: String,
    pub keys: String,
}

impl Keybinding {
    fn validate(&self) -> AppResult<()> {
        validate_nonempty("keybinding id", &self.id)?;
        validate_nonempty("keybinding action", &self.action)?;
        validate_nonempty("keybinding keys", &self.keys)?;
        validate_len("keybinding id", &self.id, MAX_KEYBINDING_FIELD_LEN)?;
        validate_len("keybinding action", &self.action, MAX_KEYBINDING_FIELD_LEN)?;
        validate_len("keybinding keys", &self.keys, MAX_KEYBINDING_FIELD_LEN)?;
        validate_no_nul("keybinding id", &self.id)?;
        validate_no_nul("keybinding action", &self.action)?;
        validate_no_nul("keybinding keys", &self.keys)?;
        Ok(())
    }
}

impl ShellProfile {
    fn validate(&self) -> AppResult<()> {
        validate_nonempty("shell profile id", &self.id)?;
        validate_len("shell profile id", &self.id, MAX_ID_LEN)?;
        validate_len("shell profile name", &self.name, MAX_NAME_LEN)?;
        validate_len("shell profile command", &self.command, MAX_COMMAND_LEN)?;
        if self.args.len() > MAX_ARGS_PER_PROFILE {
            return Err(AppError::InvalidConfig(format!(
                "shell profile '{}' has too many args",
                self.id
            )));
        }
        for arg in &self.args {
            validate_len("shell profile arg", arg, MAX_ARG_LEN)?;
            validate_no_nul("shell profile arg", arg)?;
        }
        validate_no_nul("shell profile command", &self.command)?;
        if let Some(cwd) = &self.cwd {
            validate_len("shell profile cwd", cwd, MAX_CWD_LEN)?;
            validate_no_nul("shell profile cwd", cwd)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub font_family: String,
    pub font_size: u16,
    pub line_height: f32,
    pub cursor_style: String,
    pub cursor_blink: bool,
    pub ui_theme: String,
    pub terminal_background: String,
    pub terminal_background_opacity: u8,
    /// Opacity (percent) of the egui control panels. Below 100 the panels are
    /// translucent and the backdrop behind them is blurred.
    pub panel_opacity: u8,
    pub theme: Theme,
    pub shell_profiles: Vec<ShellProfile>,
    pub default_shell_profile_id: String,
    pub keybindings: Vec<Keybinding>,
    pub restore_on_launch: bool,
    pub tab_layout: String,
    pub window_chrome: String,
    /// Lines of scrollback kept per terminal. In-memory only — never written to
    /// disk, so terminal output never persists across relaunch.
    pub scrollback_lines: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            font_family: "monospace".to_string(),
            font_size: 14,
            line_height: 1.2,
            cursor_style: "block".to_string(),
            cursor_blink: true,
            ui_theme: DEFAULT_UI_THEME.to_string(),
            terminal_background: DEFAULT_TERMINAL_BACKGROUND.to_string(),
            terminal_background_opacity: DEFAULT_TERMINAL_BACKGROUND_OPACITY,
            panel_opacity: DEFAULT_PANEL_OPACITY,
            theme: Theme::default(),
            shell_profiles: vec![ShellProfile {
                id: "default".to_string(),
                name: "Default Shell".to_string(),
                command: String::new(),
                args: Vec::new(),
                cwd: None,
            }],
            default_shell_profile_id: "default".to_string(),
            keybindings: default_keybindings(),
            restore_on_launch: true,
            tab_layout: "horizontal".to_string(),
            window_chrome: DEFAULT_WINDOW_CHROME.to_string(),
            scrollback_lines: 10_000,
        }
    }
}

impl AppConfig {
    /// Validate user-controlled config before it is stored or used to spawn a
    /// process. This keeps the webview IPC boundary small and explicit.
    pub fn validate(&self) -> AppResult<()> {
        validate_nonempty("font family", &self.font_family)?;
        validate_len("font family", &self.font_family, MAX_NAME_LEN)?;
        validate_no_nul("font family", &self.font_family)?;
        if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&self.font_size) {
            return Err(AppError::InvalidConfig(format!(
                "font size must be between {MIN_FONT_SIZE} and {MAX_FONT_SIZE}"
            )));
        }
        if !self.line_height.is_finite()
            || !(MIN_LINE_HEIGHT..=MAX_LINE_HEIGHT).contains(&self.line_height)
        {
            return Err(AppError::InvalidConfig(format!(
                "line height must be between {MIN_LINE_HEIGHT} and {MAX_LINE_HEIGHT}"
            )));
        }
        match self.cursor_style.as_str() {
            "block" | "bar" | "underline" => {}
            _ => {
                return Err(AppError::InvalidConfig(
                    "cursor style must be block, bar, or underline".to_string(),
                ));
            }
        }
        match self.ui_theme.as_str() {
            "phantom" | "aurora" | "ember" | "cobalt" | "verdant" | "violet" | "amethyst"
            | "ultraviolet" | "sapphire" | "glacier" | "lagoon" | "emerald" | "jade" | "silver" => {
            }
            _ => {
                return Err(AppError::InvalidConfig(
                    "ui theme must be one of: phantom, aurora, ember, cobalt, verdant, violet, amethyst, ultraviolet, sapphire, glacier, lagoon, emerald, jade, silver".to_string(),
                ));
            }
        }
        match self.terminal_background.as_str() {
            "none" | "phantom" | "dragon" => {}
            _ => {
                return Err(AppError::InvalidConfig(
                    "terminal background must be one of: phantom, dragon, none".to_string(),
                ));
            }
        }
        if self.terminal_background_opacity > MAX_TERMINAL_BACKGROUND_OPACITY {
            return Err(AppError::InvalidConfig(format!(
                "terminal background opacity must be between 0 and {MAX_TERMINAL_BACKGROUND_OPACITY}"
            )));
        }
        if !(MIN_PANEL_OPACITY..=MAX_PANEL_OPACITY).contains(&self.panel_opacity) {
            return Err(AppError::InvalidConfig(format!(
                "panel opacity must be between {MIN_PANEL_OPACITY} and {MAX_PANEL_OPACITY}"
            )));
        }
        if self.scrollback_lines > MAX_SCROLLBACK_LINES {
            return Err(AppError::InvalidConfig(format!(
                "scrollback_lines must be no more than {MAX_SCROLLBACK_LINES}"
            )));
        }
        match self.tab_layout.as_str() {
            "horizontal" | "vertical" => {}
            _ => {
                return Err(AppError::InvalidConfig(
                    "tab layout must be horizontal or vertical".to_string(),
                ));
            }
        }
        match self.window_chrome.as_str() {
            "system" | "custom" => {}
            _ => {
                return Err(AppError::InvalidConfig(
                    "window chrome must be system or custom".to_string(),
                ));
            }
        }
        self.theme.validate()?;
        validate_profiles(&self.shell_profiles, &self.default_shell_profile_id)?;
        validate_keybindings(&self.keybindings)?;
        Ok(())
    }

    /// Return a validated config. This method exists to keep call sites concise
    /// and to make later normalization rules a single-code-path change.
    pub fn validated(mut self) -> AppResult<Self> {
        if self
            .theme
            .selection
            .eq_ignore_ascii_case(OLD_DEFAULT_SELECTION)
        {
            self.theme.selection = DEFAULT_SELECTION.to_string();
        }
        self.validate()?;
        Ok(self)
    }

    pub fn profile(&self, id: Option<&str>) -> Option<&ShellProfile> {
        id.and_then(|id| self.shell_profiles.iter().find(|profile| profile.id == id))
            .or_else(|| {
                self.shell_profiles
                    .iter()
                    .find(|profile| profile.id == self.default_shell_profile_id)
            })
            .or_else(|| self.shell_profiles.first())
    }
}

fn default_keybindings() -> Vec<Keybinding> {
    vec![
        Keybinding {
            id: "new-tab".to_string(),
            action: "tab.new".to_string(),
            keys: "CmdOrCtrl+T".to_string(),
        },
        Keybinding {
            id: "close-tab".to_string(),
            action: "tab.close".to_string(),
            keys: "CmdOrCtrl+W".to_string(),
        },
        Keybinding {
            id: "rename-tab".to_string(),
            action: "tab.rename".to_string(),
            keys: "F2".to_string(),
        },
        Keybinding {
            id: "command-palette".to_string(),
            action: "palette.toggle".to_string(),
            keys: "CmdOrCtrl+K".to_string(),
        },
        Keybinding {
            id: "settings".to_string(),
            action: "settings.toggle".to_string(),
            keys: "CmdOrCtrl+Comma".to_string(),
        },
    ]
}

fn validate_profiles(profiles: &[ShellProfile], default_id: &str) -> AppResult<()> {
    if profiles.is_empty() {
        return Err(AppError::InvalidConfig(
            "at least one shell profile is required".to_string(),
        ));
    }
    if profiles.len() > MAX_PROFILE_COUNT {
        return Err(AppError::InvalidConfig(format!(
            "no more than {MAX_PROFILE_COUNT} shell profiles are allowed"
        )));
    }
    validate_nonempty("default shell profile id", default_id)?;
    let mut ids = HashSet::new();
    let mut default_exists = false;
    for profile in profiles {
        profile.validate()?;
        if !ids.insert(profile.id.as_str()) {
            return Err(AppError::InvalidConfig(format!(
                "duplicate shell profile id '{}'",
                profile.id
            )));
        }
        if profile.id == default_id {
            default_exists = true;
        }
    }
    if !default_exists {
        return Err(AppError::InvalidConfig(
            "default shell profile must reference an existing profile".to_string(),
        ));
    }
    Ok(())
}

fn validate_keybindings(keybindings: &[Keybinding]) -> AppResult<()> {
    if keybindings.len() > MAX_KEYBINDINGS {
        return Err(AppError::InvalidConfig(format!(
            "no more than {MAX_KEYBINDINGS} keybindings are allowed"
        )));
    }
    let mut ids = HashSet::new();
    for keybinding in keybindings {
        keybinding.validate()?;
        if !ids.insert(keybinding.id.as_str()) {
            return Err(AppError::InvalidConfig(format!(
                "duplicate keybinding id '{}'",
                keybinding.id
            )));
        }
    }
    Ok(())
}

fn validate_color(name: &str, value: &str, allow_alpha: bool) -> AppResult<()> {
    let valid_len = value.len() == 7 || (allow_alpha && value.len() == 9);
    if !valid_len || !value.starts_with('#') || !value[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        let expected = if allow_alpha {
            "#RRGGBB or #RRGGBBAA"
        } else {
            "#RRGGBB"
        };
        return Err(AppError::InvalidConfig(format!(
            "{name} must be a hex color ({expected})"
        )));
    }
    Ok(())
}

fn validate_nonempty(name: &str, value: &str) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::InvalidConfig(format!("{name} cannot be empty")));
    }
    Ok(())
}

fn validate_len(name: &str, value: &str, max: usize) -> AppResult<()> {
    if value.len() > max {
        return Err(AppError::InvalidConfig(format!(
            "{name} must be at most {max} bytes"
        )));
    }
    Ok(())
}

fn validate_no_nul(name: &str, value: &str) -> AppResult<()> {
    if value.contains('\0') {
        return Err(AppError::InvalidConfig(format!(
            "{name} cannot contain NUL bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        AppConfig::default().validate().unwrap();
    }

    #[test]
    fn rejects_empty_profile_set() {
        let config = AppConfig {
            shell_profiles: Vec::new(),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_default_profile_id_that_does_not_exist() {
        let config = AppConfig {
            default_shell_profile_id: "missing".to_string(),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_theme_color() {
        let mut config = AppConfig::default();
        config.theme.background = "black".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_ui_theme() {
        let config = AppConfig {
            ui_theme: "neon-script".to_string(),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_terminal_background() {
        let config = AppConfig {
            terminal_background: "castle".to_string(),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_terminal_background_opacity() {
        let config = AppConfig {
            terminal_background_opacity: MAX_TERMINAL_BACKGROUND_OPACITY + 1,
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_panel_opacity_below_floor() {
        let config = AppConfig {
            panel_opacity: MIN_PANEL_OPACITY - 1,
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_panel_opacity_above_ceiling() {
        let config = AppConfig {
            panel_opacity: MAX_PANEL_OPACITY + 1,
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_tab_layout() {
        let config = AppConfig {
            tab_layout: "diagonal".to_string(),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_window_chrome() {
        let config = AppConfig {
            window_chrome: "floating".to_string(),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validated_migrates_old_default_selection_color() {
        let mut config = AppConfig::default();
        config.theme.selection = OLD_DEFAULT_SELECTION.to_string();

        let validated = config.validated().unwrap();

        assert_eq!(validated.theme.selection, DEFAULT_SELECTION);
    }

    #[test]
    fn resolves_profile_with_default_fallback() {
        let config = AppConfig::default();
        assert_eq!(config.profile(Some("missing")).unwrap().id, "default");
    }
}
