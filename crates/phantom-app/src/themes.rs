//! UI theme accents. The terminal ANSI palette comes from `config.theme`; the
//! `ui_theme` name selects the chrome accent colour (tab underline, highlights).
//! The name list lives in `phantom-core` so the validated set and the picker
//! can never drift apart.

pub use phantom_core::UI_THEMES;

/// Accent colour (sRGBA) for a UI theme name, falling back to the phantom accent.
pub fn ui_theme_accent(name: &str) -> [u8; 4] {
    let rgb = match name {
        "phantom" => [0xb4, 0x78, 0xff],
        "aurora" => [0x5a, 0xf7, 0xc0],
        "ember" => [0xff, 0x7a, 0x45],
        "cobalt" => [0x3b, 0x82, 0xf6],
        "verdant" => [0x4a, 0xde, 0x80],
        "violet" => [0x8b, 0x5c, 0xf6],
        "amethyst" => [0xc0, 0x84, 0xfc],
        "ultraviolet" => [0xa8, 0x55, 0xf7],
        "sapphire" => [0x25, 0x63, 0xeb],
        "glacier" => [0x7d, 0xd3, 0xfc],
        "lagoon" => [0x22, 0xd3, 0xee],
        "emerald" => [0x10, 0xb9, 0x81],
        "jade" => [0x34, 0xd3, 0x99],
        "silver" => [0xcb, 0xd5, 0xe1],
        _ => [0xb4, 0x78, 0xff],
    };
    [rgb[0], rgb[1], rgb[2], 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches adding a theme to `phantom_core::UI_THEMES` without giving it
    /// an accent here (it would silently render with the fallback colour).
    #[test]
    fn every_ui_theme_has_a_dedicated_accent() {
        let fallback = ui_theme_accent("definitely-not-a-theme");
        for name in UI_THEMES.iter().filter(|name| **name != "phantom") {
            assert_ne!(
                ui_theme_accent(name),
                fallback,
                "theme {name} falls back to the default accent"
            );
        }
    }
}
