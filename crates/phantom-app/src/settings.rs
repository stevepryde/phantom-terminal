//! Settings panel overlay: a keyboard-driven form over [`AppConfig`]. Every
//! change is applied to a clone and accepted only if `AppConfig::validate`
//! passes, so the live config is always valid. Profile and keybinding *list*
//! management (add/remove/reorder) is a separate editor and not yet here.

use phantom_core::AppConfig;
use phantom_emu::Key;
use phantom_gfx::Renderer;

use crate::chrome::ChromeColors;
use crate::themes;

const CURSOR_STYLES: &[&str] = &["block", "bar", "underline"];
const BACKGROUNDS: &[&str] = &["none", "phantom", "dragon"];
const LAYOUTS: &[&str] = &["horizontal", "vertical"];
const COLOR_KEYS: &[&str] = &[
    "background",
    "foreground",
    "cursor",
    "selection",
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];
const VISIBLE_ROWS: usize = 16;

#[derive(Clone)]
enum FieldKind {
    Enum(&'static [&'static str]),
    Bool,
    Number {
        min: f64,
        max: f64,
        step: f64,
        int: bool,
    },
    Text,
    Hex(&'static str),
}

struct Field {
    label: String,
    kind: FieldKind,
}

pub enum SettingsOutcome {
    None,
    Close,
    /// The config changed and validated; the caller should persist it and
    /// rebuild the renderer (font / theme may have changed).
    Changed,
}

#[derive(Default)]
pub struct SettingsState {
    pub open: bool,
    selected: usize,
    editing: Option<String>,
    fields: Vec<Field>,
}

impl SettingsState {
    pub fn open(&mut self) {
        self.fields = build_fields();
        self.selected = 0;
        self.editing = None;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.editing = None;
    }

    pub fn handle_key(
        &mut self,
        key: Key,
        text: Option<&str>,
        config: &mut AppConfig,
    ) -> SettingsOutcome {
        if self.editing.is_some() {
            return self.handle_edit_key(key, text, config);
        }
        match key {
            Key::Escape => SettingsOutcome::Close,
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                SettingsOutcome::None
            }
            Key::Down => {
                self.selected = (self.selected + 1).min(self.fields.len().saturating_sub(1));
                SettingsOutcome::None
            }
            Key::Left => self.adjust(-1, config),
            Key::Right => self.adjust(1, config),
            Key::Enter => match &self.fields[self.selected].kind {
                FieldKind::Enum(_) | FieldKind::Bool => self.adjust(1, config),
                FieldKind::Text | FieldKind::Hex(_) | FieldKind::Number { .. } => {
                    self.editing = Some(self.current_value(config));
                    SettingsOutcome::None
                }
            },
            _ => SettingsOutcome::None,
        }
    }

    fn handle_edit_key(
        &mut self,
        key: Key,
        text: Option<&str>,
        config: &mut AppConfig,
    ) -> SettingsOutcome {
        match key {
            Key::Escape => {
                self.editing = None;
                SettingsOutcome::None
            }
            Key::Enter => {
                let buf = self.editing.take().unwrap_or_default();
                if self.commit_text(&buf, config) {
                    SettingsOutcome::Changed
                } else {
                    SettingsOutcome::None
                }
            }
            Key::Backspace => {
                if let Some(buf) = self.editing.as_mut() {
                    buf.pop();
                }
                SettingsOutcome::None
            }
            _ => {
                if let (Some(buf), Some(text)) = (self.editing.as_mut(), text) {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        buf.push(c);
                    }
                }
                SettingsOutcome::None
            }
        }
    }

    /// Step an enum/bool/number field, validating before committing.
    fn adjust(&mut self, dir: i32, config: &mut AppConfig) -> SettingsOutcome {
        let field = &self.fields[self.selected];
        let mut next = config.clone();
        match &field.kind {
            FieldKind::Enum(options) => {
                let current = get_enum(config, self.selected);
                let idx = options.iter().position(|o| *o == current).unwrap_or(0);
                let len = options.len() as i32;
                let new = options[(((idx as i32 + dir) % len + len) % len) as usize];
                set_enum(&mut next, self.selected, new);
            }
            FieldKind::Bool => {
                let v = get_bool(config, self.selected);
                set_bool(&mut next, self.selected, !v);
            }
            FieldKind::Number {
                min,
                max,
                step,
                int,
            } => {
                let v = (get_number(config, self.selected) + dir as f64 * step).clamp(*min, *max);
                let v = if *int { v.round() } else { v };
                set_number(&mut next, self.selected, v);
            }
            FieldKind::Text | FieldKind::Hex(_) => return SettingsOutcome::None,
        }
        if next.validate().is_ok() {
            *config = next;
            SettingsOutcome::Changed
        } else {
            SettingsOutcome::None
        }
    }

    fn commit_text(&self, buf: &str, config: &mut AppConfig) -> bool {
        let mut next = config.clone();
        match &self.fields[self.selected].kind {
            FieldKind::Text => set_text(&mut next, self.selected, buf.trim()),
            FieldKind::Hex(key) => {
                let mut v = buf.trim().to_string();
                if !v.starts_with('#') {
                    v.insert(0, '#');
                }
                set_color(&mut next, key, &v);
            }
            FieldKind::Number { min, max, int, .. } => match buf.trim().parse::<f64>() {
                Ok(parsed) => {
                    let v = parsed.clamp(*min, *max);
                    let v = if *int { v.round() } else { v };
                    set_number(&mut next, self.selected, v);
                }
                Err(_) => return false,
            },
            FieldKind::Enum(_) | FieldKind::Bool => return false,
        }
        if next.validate().is_ok() {
            *config = next;
            true
        } else {
            false
        }
    }

    fn current_value(&self, config: &AppConfig) -> String {
        value_string(config, &self.fields[self.selected], self.selected)
    }

    pub fn draw(
        &self,
        r: &mut Renderer,
        queue: &wgpu::Queue,
        config: &AppConfig,
        win_w: f32,
        win_h: f32,
        colors: &ChromeColors,
    ) {
        let cell_h = r.cell_size().1;
        let pad = 12.0;
        let row_h = cell_h + 6.0;

        r.fill_rect(0.0, 0.0, win_w, win_h, [0, 0, 0, 170]);

        let panel_w = (win_w * 0.7).clamp(420.0, 860.0);
        let panel_h = pad * 3.0 + row_h * (VISIBLE_ROWS as f32 + 2.0);
        let px = ((win_w - panel_w) / 2.0).max(0.0);
        let py = ((win_h - panel_h) / 2.0).max(20.0);

        r.fill_rect(px, py, panel_w, panel_h, colors.bar_bg);
        r.fill_rect(px, py, panel_w, 2.0, colors.accent);

        r.text(queue, px + pad, py + pad, "Settings", colors.text);
        r.text(
            queue,
            px + pad,
            py + pad + row_h,
            "\u{2191}\u{2193} navigate   \u{2190}\u{2192} change   Enter edit   Esc close",
            colors.dim_text,
        );

        let value_x = px + panel_w * 0.5;
        let highlight = [colors.accent[0], colors.accent[1], colors.accent[2], 70];
        let start = self.selected.saturating_sub(VISIBLE_ROWS - 1);
        let mut y = py + pad * 2.0 + row_h * 2.0;
        for (i, field) in self
            .fields
            .iter()
            .enumerate()
            .skip(start)
            .take(VISIBLE_ROWS)
        {
            let selected = i == self.selected;
            if selected {
                r.fill_rect(px, y, panel_w, row_h, highlight);
            }
            let label_color = if selected {
                colors.text
            } else {
                colors.dim_text
            };
            r.text(queue, px + pad, y + 3.0, &field.label, label_color);

            let value = if selected {
                if let Some(buf) = &self.editing {
                    format!("{buf}\u{258f}")
                } else {
                    value_string(config, field, i)
                }
            } else {
                value_string(config, field, i)
            };
            // Hex fields also get a colour swatch.
            if let FieldKind::Hex(key) = &field.kind {
                let swatch = parse_hex(theme_color(&config.theme, key));
                r.fill_rect(value_x - cell_h - 6.0, y + 3.0, cell_h, cell_h, swatch);
            }
            r.text(queue, value_x, y + 3.0, &value, colors.text);
            y += row_h;
        }
    }
}

fn build_fields() -> Vec<Field> {
    let mut fields = vec![
        Field {
            label: "Font family".into(),
            kind: FieldKind::Text,
        },
        Field {
            label: "Font size".into(),
            kind: FieldKind::Number {
                min: 8.0,
                max: 48.0,
                step: 1.0,
                int: true,
            },
        },
        Field {
            label: "Line height".into(),
            kind: FieldKind::Number {
                min: 1.0,
                max: 2.5,
                step: 0.05,
                int: false,
            },
        },
        Field {
            label: "Cursor style".into(),
            kind: FieldKind::Enum(CURSOR_STYLES),
        },
        Field {
            label: "Cursor blink".into(),
            kind: FieldKind::Bool,
        },
        Field {
            label: "UI theme".into(),
            kind: FieldKind::Enum(themes::UI_THEMES),
        },
        Field {
            label: "Terminal background".into(),
            kind: FieldKind::Enum(BACKGROUNDS),
        },
        Field {
            label: "Background opacity".into(),
            kind: FieldKind::Number {
                min: 0.0,
                max: 60.0,
                step: 2.0,
                int: true,
            },
        },
        Field {
            label: "Tab layout".into(),
            kind: FieldKind::Enum(LAYOUTS),
        },
        Field {
            label: "Restore tabs on launch".into(),
            kind: FieldKind::Bool,
        },
        Field {
            label: "Scrollback lines".into(),
            kind: FieldKind::Number {
                min: 0.0,
                max: 1_000_000.0,
                step: 1000.0,
                int: true,
            },
        },
    ];
    for key in COLOR_KEYS {
        fields.push(Field {
            label: format!("Color: {key}"),
            kind: FieldKind::Hex(key),
        });
    }
    fields
}

fn value_string(config: &AppConfig, field: &Field, index: usize) -> String {
    match &field.kind {
        FieldKind::Enum(_) => get_enum(config, index).to_string(),
        FieldKind::Bool => {
            if get_bool(config, index) {
                "on".into()
            } else {
                "off".into()
            }
        }
        FieldKind::Number { int, .. } => {
            let v = get_number(config, index);
            if *int {
                format!("{}", v as i64)
            } else {
                format!("{v:.2}")
            }
        }
        FieldKind::Text => config.font_family.clone(),
        FieldKind::Hex(key) => theme_color(&config.theme, key).to_string(),
    }
}

// --- Field <-> config accessors (indexed by field position for the scalar rows;
// colour rows are keyed by name). ---

fn get_enum(c: &AppConfig, index: usize) -> &str {
    match index {
        3 => &c.cursor_style,
        5 => &c.ui_theme,
        6 => &c.terminal_background,
        8 => &c.tab_layout,
        _ => "",
    }
}

fn set_enum(c: &mut AppConfig, index: usize, value: &str) {
    let v = value.to_string();
    match index {
        3 => c.cursor_style = v,
        5 => c.ui_theme = v,
        6 => c.terminal_background = v,
        8 => c.tab_layout = v,
        _ => {}
    }
}

fn get_bool(c: &AppConfig, index: usize) -> bool {
    match index {
        4 => c.cursor_blink,
        9 => c.restore_on_launch,
        _ => false,
    }
}

fn set_bool(c: &mut AppConfig, index: usize, value: bool) {
    match index {
        4 => c.cursor_blink = value,
        9 => c.restore_on_launch = value,
        _ => {}
    }
}

fn get_number(c: &AppConfig, index: usize) -> f64 {
    match index {
        1 => c.font_size as f64,
        2 => c.line_height as f64,
        7 => c.terminal_background_opacity as f64,
        10 => c.scrollback_lines as f64,
        _ => 0.0,
    }
}

fn set_number(c: &mut AppConfig, index: usize, value: f64) {
    match index {
        1 => c.font_size = value as u16,
        2 => c.line_height = value as f32,
        7 => c.terminal_background_opacity = value as u8,
        10 => c.scrollback_lines = value as u32,
        _ => {}
    }
}

fn set_text(c: &mut AppConfig, index: usize, value: &str) {
    if index == 0 {
        c.font_family = value.to_string();
    }
}

fn theme_color<'a>(theme: &'a phantom_core::Theme, key: &str) -> &'a str {
    match key {
        "background" => &theme.background,
        "foreground" => &theme.foreground,
        "cursor" => &theme.cursor,
        "selection" => &theme.selection,
        "black" => &theme.black,
        "red" => &theme.red,
        "green" => &theme.green,
        "yellow" => &theme.yellow,
        "blue" => &theme.blue,
        "magenta" => &theme.magenta,
        "cyan" => &theme.cyan,
        "white" => &theme.white,
        "bright_black" => &theme.bright_black,
        "bright_red" => &theme.bright_red,
        "bright_green" => &theme.bright_green,
        "bright_yellow" => &theme.bright_yellow,
        "bright_blue" => &theme.bright_blue,
        "bright_magenta" => &theme.bright_magenta,
        "bright_cyan" => &theme.bright_cyan,
        "bright_white" => &theme.bright_white,
        _ => "",
    }
}

fn set_color(c: &mut AppConfig, key: &str, value: &str) {
    let t = &mut c.theme;
    let v = value.to_string();
    match key {
        "background" => t.background = v,
        "foreground" => t.foreground = v,
        "cursor" => t.cursor = v,
        "selection" => t.selection = v,
        "black" => t.black = v,
        "red" => t.red = v,
        "green" => t.green = v,
        "yellow" => t.yellow = v,
        "blue" => t.blue = v,
        "magenta" => t.magenta = v,
        "cyan" => t.cyan = v,
        "white" => t.white = v,
        "bright_black" => t.bright_black = v,
        "bright_red" => t.bright_red = v,
        "bright_green" => t.bright_green = v,
        "bright_yellow" => t.bright_yellow = v,
        "bright_blue" => t.bright_blue = v,
        "bright_magenta" => t.bright_magenta = v,
        "bright_cyan" => t.bright_cyan = v,
        "bright_white" => t.bright_white = v,
        _ => {}
    }
}

fn parse_hex(s: &str) -> [u8; 4] {
    let s = s.trim_start_matches('#');
    let byte = |i: usize| {
        s.get(i..i + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .unwrap_or(0)
    };
    [byte(0), byte(2), byte(4), 255]
}

#[cfg(test)]
mod tests {
    // KeyEvent can't be constructed outside winit, so we test the pure
    // apply/validate helpers (what the key handlers call).
    use super::*;

    #[test]
    fn enum_indices_map_to_config_fields() {
        let mut c = AppConfig::default();
        set_enum(&mut c, 3, "bar");
        assert_eq!(c.cursor_style, "bar");
        set_enum(&mut c, 8, "vertical");
        assert_eq!(c.tab_layout, "vertical");
        assert_eq!(get_enum(&c, 5), c.ui_theme);
    }

    #[test]
    fn number_round_trip_and_clamping_via_config_validate() {
        let mut c = AppConfig::default();
        set_number(&mut c, 1, 20.0);
        assert_eq!(c.font_size, 20);
        assert!(c.validate().is_ok());
        // Out-of-range would be rejected by validate (the panel clamps first).
        set_number(&mut c, 1, 99.0);
        assert!(c.validate().is_err());
    }

    #[test]
    fn color_set_and_get_round_trip() {
        let mut c = AppConfig::default();
        set_color(&mut c, "red", "#abcdef");
        assert_eq!(theme_color(&c.theme, "red"), "#abcdef");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn build_fields_covers_scalars_and_colors() {
        let fields = build_fields();
        assert_eq!(fields.len(), 11 + COLOR_KEYS.len());
    }
}
