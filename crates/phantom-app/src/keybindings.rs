//! Keybinding parsing and matching.
//!
//! Parses the `CmdOrCtrl+Shift+T` style strings stored in `AppConfig` into
//! comparable [`Combo`]s, and matches engine-independent key presses against
//! them. `CmdOrCtrl` resolves to Cmd (Super) on macOS and Ctrl elsewhere.

use phantom_core::Keybinding;
use phantom_emu::Key;

use crate::event::Mods;

/// A bound action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NewTab,
    CloseTab,
    RenameTab,
    TogglePalette,
    ToggleSettings,
    /// Switch to the 1-based tab index (built-in Cmd/Ctrl+1..9).
    SwitchTab(u8),
}

fn action_from_id(action: &str) -> Option<Action> {
    match action {
        "tab.new" => Some(Action::NewTab),
        "tab.close" => Some(Action::CloseTab),
        "tab.rename" => Some(Action::RenameTab),
        "palette.toggle" => Some(Action::TogglePalette),
        "settings.toggle" => Some(Action::ToggleSettings),
        _ => None,
    }
}

/// The key part of a combo, normalized so config strings and winit events meet
/// in the middle (e.g. config `Comma` and the `,` character both become
/// `Char(',')`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComboKey {
    Char(char),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combo {
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: ComboKey,
}

/// Parse a `CmdOrCtrl+Shift+T` style string. Returns `None` if there is no
/// non-modifier key.
pub fn parse_combo(s: &str) -> Option<Combo> {
    let mut primary = false;
    let mut shift = false;
    let mut alt = false;
    let mut key = None;

    for token in s.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            "cmdorctrl" | "cmd" | "command" | "ctrl" | "control" | "super" | "meta" => {
                primary = true
            }
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            other => key = Some(normalize_key(other)),
        }
    }

    Some(Combo {
        primary,
        shift,
        alt,
        key: key?,
    })
}

fn normalize_key(token: &str) -> ComboKey {
    // Punctuation names that arrive as characters from winit.
    match token {
        "comma" => return ComboKey::Char(','),
        "period" | "dot" => return ComboKey::Char('.'),
        "space" => return ComboKey::Char(' '),
        "minus" => return ComboKey::Char('-'),
        "equal" | "plus" => return ComboKey::Char('='),
        "slash" => return ComboKey::Char('/'),
        "backslash" => return ComboKey::Char('\\'),
        "semicolon" => return ComboKey::Char(';'),
        _ => {}
    }
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => ComboKey::Char(c.to_ascii_lowercase()),
        _ => ComboKey::Named(token.to_string()),
    }
}

/// Build a combo from a logical key press, or `None` for keys we can't bind.
pub fn combo_from_key(key: Key, mods: Mods) -> Option<Combo> {
    let combo_key = match key {
        Key::Char(c) => ComboKey::Char(c.to_ascii_lowercase()),
        Key::Enter => ComboKey::Named("enter".into()),
        Key::Tab => ComboKey::Named("tab".into()),
        Key::Escape => ComboKey::Named("escape".into()),
        Key::Backspace => ComboKey::Named("backspace".into()),
        Key::F(n) => ComboKey::Named(format!("f{n}")),
        // Arrow / navigation keys aren't bound via config strings here.
        _ => return None,
    };
    Some(Combo {
        primary: mods.primary(),
        shift: mods.shift,
        alt: mods.alt,
        key: combo_key,
    })
}

pub struct Keymap {
    bindings: Vec<(Combo, Action)>,
}

impl Keymap {
    pub fn from_config(keybindings: &[Keybinding]) -> Self {
        let mut bindings = Vec::new();
        for kb in keybindings {
            if let (Some(combo), Some(action)) = (parse_combo(&kb.keys), action_from_id(&kb.action))
            {
                bindings.push((combo, action));
            }
        }
        Self { bindings }
    }

    fn action_for(&self, combo: &Combo) -> Option<Action> {
        // Built-in Cmd/Ctrl+1..9 tab switching (not in the stored config).
        if combo.primary && !combo.shift && !combo.alt {
            if let ComboKey::Char(c) = combo.key {
                if let Some(d) = c.to_digit(10) {
                    if (1..=9).contains(&d) {
                        return Some(Action::SwitchTab(d as u8));
                    }
                }
            }
        }
        self.bindings
            .iter()
            .find(|(c, _)| c == combo)
            .map(|(_, a)| *a)
    }

    /// Resolve a logical key press to an action, if bound.
    pub fn lookup(&self, key: Key, mods: Mods) -> Option<Action> {
        self.action_for(&combo_from_key(key, mods)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifier_and_letter() {
        let c = parse_combo("CmdOrCtrl+T").unwrap();
        assert!(c.primary && !c.shift && !c.alt);
        assert_eq!(c.key, ComboKey::Char('t'));
    }

    #[test]
    fn parses_shift_and_comma_and_fkey() {
        let shifted = parse_combo("CmdOrCtrl+Shift+W").unwrap();
        assert!(shifted.primary && shifted.shift);
        assert_eq!(shifted.key, ComboKey::Char('w'));

        assert_eq!(
            parse_combo("CmdOrCtrl+Comma").unwrap().key,
            ComboKey::Char(',')
        );
        assert_eq!(parse_combo("F2").unwrap().key, ComboKey::Named("f2".into()));
        assert!(!parse_combo("F2").unwrap().primary);
    }

    #[test]
    fn parse_requires_a_key() {
        assert!(parse_combo("CmdOrCtrl+Shift").is_none());
    }

    #[test]
    fn keymap_resolves_default_actions() {
        let map = Keymap::from_config(&phantom_core::AppConfig::default().keybindings);
        let combo = |s: &str| parse_combo(s).unwrap();
        assert_eq!(map.action_for(&combo("CmdOrCtrl+T")), Some(Action::NewTab));
        assert_eq!(
            map.action_for(&combo("CmdOrCtrl+W")),
            Some(Action::CloseTab)
        );
        assert_eq!(map.action_for(&combo("F2")), Some(Action::RenameTab));
        assert_eq!(
            map.action_for(&combo("CmdOrCtrl+K")),
            Some(Action::TogglePalette)
        );
        assert_eq!(
            map.action_for(&combo("CmdOrCtrl+Comma")),
            Some(Action::ToggleSettings)
        );
    }

    #[test]
    fn builtin_tab_switching() {
        let map = Keymap::from_config(&[]);
        assert_eq!(
            map.action_for(&parse_combo("CmdOrCtrl+1").unwrap()),
            Some(Action::SwitchTab(1))
        );
        assert_eq!(
            map.action_for(&parse_combo("CmdOrCtrl+9").unwrap()),
            Some(Action::SwitchTab(9))
        );
        // No modifier -> just a digit keystroke, not a switch.
        assert_eq!(map.action_for(&parse_combo("5").unwrap()), None);
    }
}
