use serde::{Deserialize, Serialize};

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

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: "#0b0b0e".to_string(),
            foreground: "#e6e6e6".to_string(),
            cursor: "#e6e6e6".to_string(),
            selection: "#33415580".to_string(),
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
#[serde(default)]
pub struct AppConfig {
    pub font_family: String,
    pub font_size: u16,
    pub theme: Theme,
    pub shell_profiles: Vec<ShellProfile>,
    pub default_shell_profile_id: String,
    pub restore_on_launch: bool,
    /// Lines of scrollback kept per terminal. In-memory only — never written to
    /// disk, so terminal output never persists across relaunch.
    pub scrollback_lines: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            font_family: "monospace".to_string(),
            font_size: 14,
            theme: Theme::default(),
            shell_profiles: vec![ShellProfile {
                id: "default".to_string(),
                name: "Default Shell".to_string(),
                command: String::new(),
                args: Vec::new(),
                cwd: None,
            }],
            default_shell_profile_id: "default".to_string(),
            restore_on_launch: true,
            scrollback_lines: 10_000,
        }
    }
}
