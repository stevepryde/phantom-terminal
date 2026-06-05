//! Render a representative terminal frame to a PNG, headlessly — no window.
//!
//!   cargo run -p phantom-gfx --features headless --example snapshot [out.png]
//!
//! Useful for eyeballing the renderer's real output (colours, bold/underline/
//! inverse, cursor) without launching the app, and for CI visual diffs.

use std::fs::File;
use std::io::BufWriter;

use phantom_core::AppConfig;
use phantom_emu::{AlacrittyCore, CursorShape, VtCore};
use phantom_gfx::headless::Harness;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "phantom-snapshot.png".into());
    let (width, height) = (760u32, 320u32);

    let config = AppConfig::default();
    let Some(mut harness) = Harness::new(&config, width, height) else {
        eprintln!("no GPU adapter available; cannot render a snapshot here");
        std::process::exit(1);
    };

    let (rows, cols) = harness.grid();
    let mut term = AlacrittyCore::new(rows, cols, 1000, CursorShape::Block);

    // A representative shell session exercising colour, bold, underline, inverse.
    term.advance(b"\x1b[32muser@host\x1b[0m:\x1b[34m~/projects/phantom\x1b[0m$ ls\r\n");
    term.advance(b"\x1b[1;36mCargo.toml\x1b[0m  \x1b[1;32msrc\x1b[0m  README.md  \x1b[1;34mcrates\x1b[0m\r\n");
    term.advance(
        b"\x1b[33mwarning\x1b[0m: \x1b[4munderlined\x1b[0m text and \x1b[7minverse\x1b[0m too\r\n",
    );
    term.advance(b"\x1b[31m\xe2\x9c\x97 error\x1b[0m: 256-colour \x1b[38;5;208morange\x1b[0m \x1b[38;5;45mcyan\x1b[0m\r\n");
    term.advance(b"$ ");

    let image = harness.render_snapshot(&term.snapshot(), true);

    let file = File::create(&path).expect("create png");
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(image.rgba())
        .expect("png data");

    println!(
        "wrote {path} ({}x{}, {} cells)",
        image.width,
        image.height,
        rows as u32 * cols as u32
    );
}
