//! UI theme accents. The terminal ANSI palette comes from `config.theme`; the
//! `ui_theme` name selects the chrome accent colour (tab underline, highlights).
//! These names mirror the validated set in `AppConfig::validate`.

/// All selectable UI theme names, in display order.
pub const UI_THEMES: &[&str] = &[
    "phantom",
    "aurora",
    "ember",
    "cobalt",
    "verdant",
    "violet",
    "amethyst",
    "ultraviolet",
    "sapphire",
    "glacier",
    "lagoon",
    "emerald",
    "jade",
    "silver",
];

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
