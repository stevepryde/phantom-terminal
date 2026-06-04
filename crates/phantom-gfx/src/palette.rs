//! Colour resolution: turn a [`CellColor`] into concrete 8-bit sRGBA using the
//! active theme. Handles the 16 ANSI slots, the xterm 256-colour cube and
//! grayscale ramp, and truecolour passthrough.

use phantom_core::Theme;
use phantom_emu::{CellColor, NamedColor};

/// 8-bit sRGBA, straight alpha.
pub type Rgba = [u8; 4];

#[derive(Debug, Clone)]
pub struct Palette {
    ansi: [Rgba; 16],
    foreground: Rgba,
    background: Rgba,
    cursor: Rgba,
    selection: Rgba,
}

impl Palette {
    /// Build a palette from a validated theme. All theme colours are valid hex
    /// (config validation guarantees it), so parsing never realistically fails.
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            ansi: [
                hex(&theme.black),
                hex(&theme.red),
                hex(&theme.green),
                hex(&theme.yellow),
                hex(&theme.blue),
                hex(&theme.magenta),
                hex(&theme.cyan),
                hex(&theme.white),
                hex(&theme.bright_black),
                hex(&theme.bright_red),
                hex(&theme.bright_green),
                hex(&theme.bright_yellow),
                hex(&theme.bright_blue),
                hex(&theme.bright_magenta),
                hex(&theme.bright_cyan),
                hex(&theme.bright_white),
            ],
            foreground: hex(&theme.foreground),
            background: hex(&theme.background),
            cursor: hex(&theme.cursor),
            selection: hex(&theme.selection),
        }
    }

    /// Selection highlight colour (may carry alpha from `#rrggbbaa`).
    pub fn selection(&self) -> Rgba {
        self.selection
    }

    pub fn foreground(&self) -> Rgba {
        self.foreground
    }

    pub fn background(&self) -> Rgba {
        self.background
    }

    pub fn cursor(&self) -> Rgba {
        self.cursor
    }

    /// One of the 16 ANSI slots (`index` must be 0..=15).
    pub fn ansi(&self, index: usize) -> Rgba {
        self.ansi[index.min(15)]
    }

    /// Resolve a cell colour to sRGBA.
    pub fn resolve(&self, color: CellColor) -> Rgba {
        match color {
            CellColor::Rgb(r, g, b) => [r, g, b, 255],
            CellColor::Named(named) => self.named(named),
            CellColor::Indexed(i) => self.indexed(i),
        }
    }

    fn named(&self, named: NamedColor) -> Rgba {
        use NamedColor::*;
        match named {
            Black => self.ansi[0],
            Red => self.ansi[1],
            Green => self.ansi[2],
            Yellow => self.ansi[3],
            Blue => self.ansi[4],
            Magenta => self.ansi[5],
            Cyan => self.ansi[6],
            White => self.ansi[7],
            BrightBlack => self.ansi[8],
            BrightRed => self.ansi[9],
            BrightGreen => self.ansi[10],
            BrightYellow => self.ansi[11],
            BrightBlue => self.ansi[12],
            BrightMagenta => self.ansi[13],
            BrightCyan => self.ansi[14],
            BrightWhite => self.ansi[15],
            Foreground => self.foreground,
            Background => self.background,
            Cursor => self.cursor,
        }
    }

    fn indexed(&self, i: u8) -> Rgba {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                // 6x6x6 colour cube.
                let j = i - 16;
                [cube(j / 36), cube((j % 36) / 6), cube(j % 6), 255]
            }
            232..=255 => {
                // 24-step grayscale ramp.
                let v = 8 + 10 * (i - 232);
                [v, v, v, 255]
            }
        }
    }
}

/// xterm colour-cube channel value: 0, then 95, 135, 175, 215, 255.
fn cube(v: u8) -> u8 {
    if v == 0 {
        0
    } else {
        55 + 40 * v
    }
}

/// Parse `#rrggbb` or `#rrggbbaa` (leading `#` optional) into sRGBA.
fn hex(s: &str) -> Rgba {
    let s = s.trim_start_matches('#');
    let byte = |i: usize| {
        s.get(i..i + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .unwrap_or(0xff)
    };
    if s.len() >= 8 {
        [byte(0), byte(2), byte(4), byte(6)]
    } else {
        [byte(0), byte(2), byte(4), 255]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::from_theme(&Theme::default())
    }

    #[test]
    fn truecolor_passes_through() {
        assert_eq!(
            palette().resolve(CellColor::Rgb(10, 20, 30)),
            [10, 20, 30, 255]
        );
    }

    #[test]
    fn named_uses_theme_slots() {
        let theme = Theme::default();
        let p = Palette::from_theme(&theme);
        assert_eq!(
            p.resolve(CellColor::Named(NamedColor::Red)),
            hex(&theme.red)
        );
        assert_eq!(
            p.resolve(CellColor::Named(NamedColor::Foreground)),
            hex(&theme.foreground)
        );
        assert_eq!(
            p.resolve(CellColor::Named(NamedColor::BrightWhite)),
            hex(&theme.bright_white)
        );
    }

    #[test]
    fn indexed_low_16_match_ansi() {
        let p = palette();
        assert_eq!(
            p.resolve(CellColor::Indexed(1)),
            p.resolve(CellColor::Named(NamedColor::Red))
        );
    }

    #[test]
    fn indexed_color_cube_endpoints() {
        let p = palette();
        // 16 is the cube origin (black); 231 is the cube max (white).
        assert_eq!(p.resolve(CellColor::Indexed(16)), [0, 0, 0, 255]);
        assert_eq!(p.resolve(CellColor::Indexed(231)), [255, 255, 255, 255]);
        // 196 is pure red (cube 5,0,0).
        assert_eq!(p.resolve(CellColor::Indexed(196)), [255, 0, 0, 255]);
    }

    #[test]
    fn indexed_grayscale_ramp() {
        let p = palette();
        assert_eq!(p.resolve(CellColor::Indexed(232)), [8, 8, 8, 255]);
        assert_eq!(p.resolve(CellColor::Indexed(255)), [238, 238, 238, 255]);
    }

    #[test]
    fn hex_parses_rgb_and_rgba() {
        assert_eq!(hex("#ff8800"), [0xff, 0x88, 0x00, 255]);
        assert_eq!(hex("#11223344"), [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(hex("zzzzzz"), [0xff, 0xff, 0xff, 255]); // malformed -> fallback
    }
}
