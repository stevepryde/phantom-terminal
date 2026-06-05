//! Pixel-level tests of the real renderer, via the headless offscreen harness.
//! Run with: `cargo test -p phantom-gfx --features headless`.
//! Tests skip (rather than fail) when no GPU adapter is available.
#![cfg(feature = "headless")]

use phantom_core::AppConfig;
use phantom_emu::{AlacrittyCore, CursorShape, SelSide, VtCore};
use phantom_gfx::headless::Harness;

fn core(rows: u16, cols: u16) -> AlacrittyCore {
    AlacrittyCore::new(rows, cols, 1000, CursorShape::Block)
}

/// Centre sample of a cell, inset to avoid edges/anti-aliasing.
fn cell_sample(h: &Harness, col: usize, row: usize) -> (u32, u32, u32, u32) {
    let (cw, ch) = h.cell_size();
    let x = (col as f32 * cw) as u32 + 1;
    let y = (row as f32 * ch) as u32 + 1;
    let w = (cw as u32).saturating_sub(2).max(1);
    let hh = (ch as u32).saturating_sub(2).max(1);
    (x, y, w, hh)
}

macro_rules! harness_or_skip {
    ($config:expr, $w:expr, $h:expr) => {
        match Harness::new($config, $w, $h) {
            Some(h) => h,
            None => {
                eprintln!("SKIP: no GPU adapter");
                return;
            }
        }
    };
}

#[test]
fn empty_grid_is_theme_background() {
    let config = AppConfig::default();
    let mut h = harness_or_skip!(&config, 200, 120);
    let (rows, cols) = h.grid();
    let term = core(rows, cols);

    let img = h.render_snapshot(&term.snapshot(), false);
    // Theme background is #0b0b0e -> ~[11, 11, 14].
    let [r, g, b] = img.avg(80, 50, 20, 20);
    assert!(
        r < 40 && g < 40 && b < 40,
        "background too bright: {r},{g},{b}"
    );
}

#[test]
fn glyph_draws_in_its_cell_and_not_elsewhere() {
    let config = AppConfig::default();
    let mut h = harness_or_skip!(&config, 240, 120);
    let (rows, cols) = h.grid();
    let mut term = core(rows, cols);
    term.advance(b"X"); // single glyph at (0,0)

    let img = h.render_snapshot(&term.snapshot(), false);
    let bg = img.avg(0, 0, 1, 1)[0] as i32;

    let (x, y, w, hh) = cell_sample(&h, 0, 0);
    let drawn = img.avg(x, y, w, hh)[0] as i32;
    let (ex, ey, ew, eh) = cell_sample(&h, 6, 0);
    let empty = img.avg(ex, ey, ew, eh)[0] as i32;

    assert!(
        drawn > bg + 30,
        "glyph cell not brighter than bg ({drawn} vs {bg})"
    );
    assert!(empty < bg + 15, "empty cell unexpectedly bright ({empty})");
}

#[test]
fn starship_prompt_symbol_draws_when_font_stack_covers_it() {
    let config = AppConfig::default();
    let mut h = harness_or_skip!(&config, 240, 120);
    let (rows, cols) = h.grid();
    let mut term = core(rows, cols);
    term.advance("\u{276f}".as_bytes());

    let img = h.render_snapshot(&term.snapshot(), false);
    let bg = img.avg(0, 0, 1, 1);
    let (x, y, w, hh) = cell_sample(&h, 0, 0);
    let drawn = img.avg(x, y, w, hh);

    let bg_sum: u32 = bg.iter().sum();
    let drawn_sum: u32 = drawn.iter().sum();
    assert!(
        drawn_sum > bg_sum + 20,
        "prompt symbol did not render ({drawn_sum} vs {bg_sum})"
    );
}

#[test]
fn foreground_color_reaches_the_pixels() {
    let config = AppConfig::default();
    let mut h = harness_or_skip!(&config, 240, 120);
    let (rows, cols) = h.grid();
    let mut term = core(rows, cols);
    term.advance(b"\x1b[31mW\x1b[0m"); // red glyph

    let img = h.render_snapshot(&term.snapshot(), false);
    let (x, y, w, hh) = cell_sample(&h, 0, 0);
    let [r, g, b] = img.avg(x, y, w, hh);
    assert!(
        r > g + 20 && r > b + 20,
        "expected reddish cell, got {r},{g},{b}"
    );
}

#[test]
fn selection_highlights_the_cells() {
    let config = AppConfig::default();
    let mut h = harness_or_skip!(&config, 240, 120);
    let (rows, cols) = h.grid();
    let mut term = core(rows, cols);
    term.advance(b"hello");

    let (x, y, w, hh) = cell_sample(&h, 0, 0);
    let before = h.render_snapshot(&term.snapshot(), false).avg(x, y, w, hh);

    term.selection_start(0, 0, SelSide::Left);
    term.selection_update(0, 4, SelSide::Right);
    let after = h.render_snapshot(&term.snapshot(), false).avg(x, y, w, hh);

    // Selection (#ffffff24, translucent white) lightens the cell.
    let sum_before: u32 = before.iter().sum();
    let sum_after: u32 = after.iter().sum();
    assert!(
        sum_after > sum_before + 10,
        "selection did not lighten the cell ({sum_before} -> {sum_after})"
    );
}

#[test]
fn block_cursor_fills_its_cell_when_on() {
    let config = AppConfig::default();
    let mut h = harness_or_skip!(&config, 240, 120);
    let (rows, cols) = h.grid();
    let term = core(rows, cols); // cursor at (0,0), empty cell

    let (x, y, w, hh) = cell_sample(&h, 0, 0);
    let off = h.render_snapshot(&term.snapshot(), false).avg(x, y, w, hh);
    let on = h.render_snapshot(&term.snapshot(), true).avg(x, y, w, hh);

    // The block cursor (theme cursor #e6e6e6) is far brighter than the bg.
    let sum_off: u32 = off.iter().sum();
    let sum_on: u32 = on.iter().sum();
    assert!(
        sum_on > sum_off + 200,
        "cursor-on cell not brighter ({sum_off} -> {sum_on})"
    );
}
