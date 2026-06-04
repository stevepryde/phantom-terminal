//! The winit adapter: translate `winit` keyboard types into the
//! engine-independent [`Key`] and [`Mods`](crate::event::Mods) the app logic
//! uses. This is the only place (besides the event-loop wiring) that touches
//! winit key types.

use phantom_emu::Key;
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

use crate::event::Mods;

/// Translate winit's modifier state into ours (including the Cmd/Super bit).
pub fn winit_mods(state: ModifiersState) -> Mods {
    Mods {
        ctrl: state.control_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
        sup: state.super_key(),
    }
}

/// Translate a winit logical key into a [`Key`] we forward to the PTY, or `None`
/// for keys we don't send (pure modifiers, dead keys, unidentified, …).
pub fn map_key(logical: &WKey) -> Option<Key> {
    match logical {
        WKey::Named(named) => named_key(*named),
        // `Character` already reflects the active layout + Shift. Ctrl handling
        // happens in `encode_key`, which derives the control byte from the char.
        WKey::Character(s) => s.chars().next().map(Key::Char),
        _ => None,
    }
}

fn named_key(named: NamedKey) -> Option<Key> {
    let key = match named {
        NamedKey::Enter => Key::Enter,
        NamedKey::Tab => Key::Tab,
        NamedKey::Backspace => Key::Backspace,
        NamedKey::Escape => Key::Escape,
        NamedKey::Space => Key::Char(' '),
        NamedKey::ArrowUp => Key::Up,
        NamedKey::ArrowDown => Key::Down,
        NamedKey::ArrowLeft => Key::Left,
        NamedKey::ArrowRight => Key::Right,
        NamedKey::Home => Key::Home,
        NamedKey::End => Key::End,
        NamedKey::PageUp => Key::PageUp,
        NamedKey::PageDown => Key::PageDown,
        NamedKey::Insert => Key::Insert,
        NamedKey::Delete => Key::Delete,
        NamedKey::F1 => Key::F(1),
        NamedKey::F2 => Key::F(2),
        NamedKey::F3 => Key::F(3),
        NamedKey::F4 => Key::F(4),
        NamedKey::F5 => Key::F(5),
        NamedKey::F6 => Key::F(6),
        NamedKey::F7 => Key::F(7),
        NamedKey::F8 => Key::F(8),
        NamedKey::F9 => Key::F(9),
        NamedKey::F10 => Key::F(10),
        NamedKey::F11 => Key::F(11),
        NamedKey::F12 => Key::F(12),
        _ => return None,
    };
    Some(key)
}
