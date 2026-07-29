//! Key/input encoding: translate a logical key press into the byte sequence a
//! terminal application expects on stdin.
//!
//! This is the input half of the VT contract. It is intentionally
//! engine-independent (it only needs to know the application-cursor-keys mode,
//! which the [`VtCore`](crate::VtCore) reports) so the windowing layer in
//! `phantom-app` can map `winit` key events straight onto [`Key`] + [`Modifiers`].
//!
//! Modifier-combination encoding follows the common xterm convention
//! (`CSI 1 ; <mod> <final>`), which is what shells and editors expect.

/// Active modifier keys at the moment of a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::NONE
        }
    }

    pub fn alt() -> Self {
        Self {
            alt: true,
            ..Self::NONE
        }
    }

    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Self::NONE
        }
    }

    fn any(self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    /// xterm modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4).
    fn xterm_code(self) -> u8 {
        1 + (self.shift as u8) + 2 * (self.alt as u8) + 4 * (self.ctrl as u8)
    }
}

/// A logical key press, independent of any windowing toolkit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character (already resolved for layout/shift by the OS).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// Function key `F1`..=`F12`.
    F(u8),
}

/// Encode `key` (with `mods`) into the bytes to write to the PTY.
/// `app_cursor_keys` is the terminal's DECCKM state, which changes the prefix
/// for cursor keys and keeps Home/End available to full-screen applications.
pub fn encode_key(key: Key, mods: Modifiers, app_cursor_keys: bool) -> Vec<u8> {
    match key {
        Key::Char(c) => {
            if mods.ctrl {
                if let Some(b) = ctrl_byte(c) {
                    return with_alt(vec![b], mods.alt);
                }
            }
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf).as_bytes().to_vec();
            with_alt(encoded, mods.alt)
        }
        Key::Enter => with_alt(vec![b'\r'], mods.alt),
        Key::Tab => {
            if mods.shift {
                vec![0x1b, b'[', b'Z'] // back-tab (CBT)
            } else {
                with_alt(vec![b'\t'], mods.alt)
            }
        }
        Key::Backspace => with_alt(vec![0x7f], mods.alt),
        Key::Escape => vec![0x1b],
        Key::Up => cursor_key(b'A', mods, app_cursor_keys),
        Key::Down => cursor_key(b'B', mods, app_cursor_keys),
        Key::Right => cursor_key(b'C', mods, app_cursor_keys),
        Key::Left => cursor_key(b'D', mods, app_cursor_keys),
        Key::Home => line_key(0x01, b'H', mods, app_cursor_keys),
        Key::End => line_key(0x05, b'F', mods, app_cursor_keys),
        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),
        Key::F(n) => function_key(n, mods),
    }
}

/// Prepend ESC when Alt (meta-sends-escape) is held.
fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(0x1b);
        out.append(&mut bytes);
        out
    } else {
        bytes
    }
}

/// Map a character under Ctrl to its control byte, or `None` if there is no
/// canonical control code (in which case the literal char is sent).
fn ctrl_byte(c: char) -> Option<u8> {
    if !c.is_ascii() {
        return None;
    }
    match c as u8 {
        b' ' | b'@' => Some(0x00),
        b @ b'a'..=b'z' => Some(b - b'a' + 1),
        b @ b'A'..=b'Z' => Some(b - b'A' + 1),
        b'[' => Some(0x1b),
        b'\\' => Some(0x1c),
        b']' => Some(0x1d),
        b'^' => Some(0x1e),
        b'_' => Some(0x1f),
        b'?' => Some(0x7f),
        _ => None,
    }
}

/// Arrow / Home / End. Without modifiers the prefix is `ESC O` in
/// application-cursor-keys mode, else `ESC [`; with modifiers it is always the
/// `CSI 1 ; <mod> <final>` form.
fn cursor_key(final_byte: u8, mods: Modifiers, app_cursor_keys: bool) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[1;{}{}", mods.xterm_code(), final_byte as char).into_bytes()
    } else if app_cursor_keys {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// Home / End. At a normal shell prompt, send the canonical line-editing
/// controls so the keys work without shell-specific escape-sequence bindings.
/// Applications that enable DECCKM still receive their advertised cursor-key
/// sequences, and modified keys retain the xterm CSI form.
fn line_key(line_control: u8, final_byte: u8, mods: Modifiers, app_cursor_keys: bool) -> Vec<u8> {
    if mods.any() || app_cursor_keys {
        cursor_key(final_byte, mods, app_cursor_keys)
    } else {
        vec![line_control]
    }
}

/// `CSI <num> ~` keys (PageUp/Down, Insert, Delete, F5+), with optional modifier.
fn tilde_key(num: u8, mods: Modifiers) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[{};{}~", num, mods.xterm_code()).into_bytes()
    } else {
        format!("\x1b[{}~", num).into_bytes()
    }
}

fn function_key(n: u8, mods: Modifiers) -> Vec<u8> {
    match n {
        1..=4 => {
            let final_byte = [b'P', b'Q', b'R', b'S'][(n - 1) as usize];
            if mods.any() {
                format!("\x1b[1;{}{}", mods.xterm_code(), final_byte as char).into_bytes()
            } else {
                vec![0x1b, b'O', final_byte]
            }
        }
        5 => tilde_key(15, mods),
        6 => tilde_key(17, mods),
        7 => tilde_key(18, mods),
        8 => tilde_key(19, mods),
        9 => tilde_key(20, mods),
        10 => tilde_key(21, mods),
        11 => tilde_key(23, mods),
        12 => tilde_key(24, mods),
        _ => Vec::new(),
    }
}

/// Encode a mouse event in SGR form: `CSI < b ; col ; row M` (press) or `m`
/// (release). `button` already includes any modifier/motion bits; `col`/`row`
/// are 0-based and emitted 1-based.
pub fn encode_mouse_sgr(button: u8, col: u16, row: u16, pressed: bool) -> Vec<u8> {
    let terminator = if pressed { 'M' } else { 'm' };
    format!(
        "\x1b[<{};{};{}{}",
        button,
        col as u32 + 1,
        row as u32 + 1,
        terminator
    )
    .into_bytes()
}

/// Encode a mouse event in legacy X10 form: `CSI M Cb Cx Cy`, each value biased
/// by 32. Coordinates beyond the protocol's 223 limit are clamped.
pub fn encode_mouse_legacy(button: u8, col: u16, row: u16) -> Vec<u8> {
    let coord = |v: u16| (33 + v.min(222)) as u8;
    vec![
        0x1b,
        b'[',
        b'M',
        32u8.saturating_add(button),
        coord(col),
        coord(row),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_char_is_its_utf8() {
        assert_eq!(encode_key(Key::Char('a'), Modifiers::NONE, false), b"a");
        assert_eq!(
            encode_key(Key::Char('é'), Modifiers::NONE, false),
            "é".as_bytes()
        );
    }

    #[test]
    fn ctrl_letters_map_to_control_codes() {
        assert_eq!(
            encode_key(Key::Char('c'), Modifiers::ctrl(), false),
            vec![3]
        );
        assert_eq!(
            encode_key(Key::Char('a'), Modifiers::ctrl(), false),
            vec![1]
        );
        assert_eq!(
            encode_key(Key::Char(' '), Modifiers::ctrl(), false),
            vec![0]
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            encode_key(Key::Char('b'), Modifiers::alt(), false),
            b"\x1bb"
        );
    }

    #[test]
    fn control_keys_have_canonical_bytes() {
        assert_eq!(encode_key(Key::Enter, Modifiers::NONE, false), vec![b'\r']);
        assert_eq!(
            encode_key(Key::Backspace, Modifiers::NONE, false),
            vec![0x7f]
        );
        assert_eq!(encode_key(Key::Escape, Modifiers::NONE, false), vec![0x1b]);
        assert_eq!(encode_key(Key::Tab, Modifiers::NONE, false), vec![b'\t']);
        assert_eq!(encode_key(Key::Tab, Modifiers::shift(), false), b"\x1b[Z");
    }

    #[test]
    fn arrows_respect_application_cursor_keys() {
        assert_eq!(encode_key(Key::Up, Modifiers::NONE, false), b"\x1b[A");
        assert_eq!(encode_key(Key::Up, Modifiers::NONE, true), b"\x1bOA");
        assert_eq!(encode_key(Key::Left, Modifiers::NONE, false), b"\x1b[D");
    }

    #[test]
    fn home_and_end_edit_the_shell_line() {
        assert_eq!(encode_key(Key::Home, Modifiers::NONE, false), b"\x01");
        assert_eq!(encode_key(Key::End, Modifiers::NONE, false), b"\x05");
    }

    #[test]
    fn home_and_end_respect_application_cursor_keys() {
        assert_eq!(encode_key(Key::Home, Modifiers::NONE, true), b"\x1bOH");
        assert_eq!(encode_key(Key::End, Modifiers::NONE, true), b"\x1bOF");
    }

    #[test]
    fn modifiers_use_xterm_csi_form() {
        // Ctrl is bit 4 -> code 5.
        assert_eq!(encode_key(Key::Up, Modifiers::ctrl(), false), b"\x1b[1;5A");
        // Even in app-cursor-keys mode, modifiers force the CSI form.
        assert_eq!(encode_key(Key::Up, Modifiers::ctrl(), true), b"\x1b[1;5A");
        assert_eq!(
            encode_key(Key::End, Modifiers::shift(), false),
            b"\x1b[1;2F"
        );
    }

    #[test]
    fn navigation_and_function_keys() {
        assert_eq!(encode_key(Key::PageUp, Modifiers::NONE, false), b"\x1b[5~");
        assert_eq!(encode_key(Key::Delete, Modifiers::NONE, false), b"\x1b[3~");
        assert_eq!(encode_key(Key::F(1), Modifiers::NONE, false), b"\x1bOP");
        assert_eq!(encode_key(Key::F(5), Modifiers::NONE, false), b"\x1b[15~");
        assert_eq!(encode_key(Key::F(12), Modifiers::NONE, false), b"\x1b[24~");
    }

    #[test]
    fn sgr_mouse_encoding() {
        assert_eq!(encode_mouse_sgr(0, 0, 0, true), b"\x1b[<0;1;1M");
        assert_eq!(encode_mouse_sgr(0, 5, 10, false), b"\x1b[<0;6;11m");
        // Wheel-up (button 64).
        assert_eq!(encode_mouse_sgr(64, 2, 3, true), b"\x1b[<64;3;4M");
    }

    #[test]
    fn legacy_mouse_encoding_biases_and_clamps() {
        assert_eq!(
            encode_mouse_legacy(0, 0, 0),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
        // Column past the 223 ceiling is clamped.
        assert_eq!(encode_mouse_legacy(0, 5000, 0)[4], 255);
    }
}
