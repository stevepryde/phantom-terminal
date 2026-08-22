//! UI theme accents and terminal washes. The terminal ANSI palette comes from
//! `config.theme`; `ui_theme` only tints chrome and, when no backdrop image is
//! selected, the translucent wash behind terminal content. The name list lives
//! in `phantom-core` so the validated set and the picker can never drift apart.

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

/// Low-opacity sRGBA corner colours for the terminal-pane wash. Phantom stays
/// neutral; every decorative preset uses three restrained hues and four stops.
fn ui_theme_terminal_wash(name: &str) -> Option<[[u8; 4]; 4]> {
    let hues = match name {
        "aurora" => [[34, 211, 238], [139, 92, 246], [20, 184, 166]],
        "ember" => [[239, 68, 68], [249, 115, 22], [139, 92, 246]],
        "cobalt" => [[59, 130, 246], [20, 184, 166], [79, 70, 229]],
        "verdant" => [[34, 197, 94], [6, 182, 212], [202, 138, 4]],
        "violet" => [[139, 92, 246], [217, 70, 239], [79, 70, 229]],
        "amethyst" => [[192, 132, 252], [244, 114, 182], [96, 165, 250]],
        "ultraviolet" => [[168, 85, 247], [59, 130, 246], [217, 70, 239]],
        "sapphire" => [[37, 99, 235], [6, 182, 212], [79, 70, 229]],
        "glacier" => [[125, 211, 252], [186, 230, 253], [96, 165, 250]],
        "lagoon" => [[34, 211, 238], [20, 184, 166], [34, 197, 94]],
        "emerald" => [[16, 185, 129], [13, 148, 136], [132, 204, 22]],
        "jade" => [[52, 211, 153], [20, 184, 166], [234, 179, 8]],
        "silver" => [[148, 163, 184], [203, 213, 225], [96, 165, 250]],
        _ => return None,
    };
    Some([
        with_alpha(hues[0], 12),
        with_alpha(hues[1], 9),
        with_alpha(hues[2], 11),
        with_alpha(hues[0], 7),
    ])
}

fn with_alpha(rgb: [u8; 3], alpha: u8) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], alpha]
}

/// Decorative wash for empty terminal cells. A chosen backdrop image is shown
/// without a theme tint; the wash only applies when the backdrop is `none`.
pub fn terminal_pane_wash(ui_theme: &str, terminal_background: &str) -> Option<[[u8; 4]; 4]> {
    if terminal_background == "none" {
        ui_theme_terminal_wash(ui_theme)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

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

    #[test]
    fn decorative_themes_have_distinct_dim_terminal_washes() {
        assert_eq!(ui_theme_terminal_wash("phantom"), None);

        let washes: Vec<_> = UI_THEMES
            .iter()
            .filter(|name| **name != "phantom")
            .map(|name| {
                let wash = ui_theme_terminal_wash(name)
                    .unwrap_or_else(|| panic!("theme {name} has no terminal wash"));
                assert!(wash.iter().all(|color| color[3] <= 12));
                wash
            })
            .collect();
        assert_eq!(
            washes.iter().copied().collect::<HashSet<_>>().len(),
            washes.len()
        );
    }

    #[test]
    fn decorative_wash_is_not_applied_over_a_backdrop_image() {
        let aurora =
            ui_theme_terminal_wash("aurora").expect("aurora should have a decorative wash");
        assert_eq!(terminal_pane_wash("aurora", "none"), Some(aurora));
        assert_eq!(terminal_pane_wash("aurora", "phantom"), None);
        assert_eq!(terminal_pane_wash("aurora", "dragon"), None);
        assert_eq!(terminal_pane_wash("phantom", "none"), None);
    }
}
