//! Native chrome: window layout and the tab bar. Drawn with the phantom-gfx
//! frame API; returns hit regions so the app can route clicks.

use phantom_gfx::Renderer;

const PAD: f32 = 8.0;
const CLOSE_W: f32 = 16.0;
const NEW_TAB_W: f32 = 30.0;
const MAX_TAB_W: f32 = 220.0;
const MIN_TAB_W: f32 = 70.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Window split into the tab bar and the terminal viewport.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub bar: Rect,
    pub viewport: Rect,
    pub horizontal: bool,
}

/// Compute the layout for a window of `w` x `h` physical px. `horizontal` puts
/// the tab bar across the top; otherwise down the left side.
pub fn compute_layout(w: f32, h: f32, cell_h: f32, horizontal: bool) -> Layout {
    if horizontal {
        let bar_h = (cell_h + PAD * 2.0).ceil();
        Layout {
            bar: Rect {
                x: 0.0,
                y: 0.0,
                w,
                h: bar_h,
            },
            viewport: Rect {
                x: 0.0,
                y: bar_h,
                w,
                h: (h - bar_h).max(0.0),
            },
            horizontal,
        }
    } else {
        let bar_w = (w * 0.32).clamp(120.0, 240.0);
        Layout {
            bar: Rect {
                x: 0.0,
                y: 0.0,
                w: bar_w,
                h,
            },
            viewport: Rect {
                x: bar_w,
                y: 0.0,
                w: (w - bar_w).max(0.0),
                h,
            },
            horizontal,
        }
    }
}

pub struct ChromeColors {
    pub bar_bg: [u8; 4],
    pub active_bg: [u8; 4],
    pub text: [u8; 4],
    pub dim_text: [u8; 4],
    pub accent: [u8; 4],
}

impl ChromeColors {
    /// Derive a chrome palette from the renderer's theme colours, using the
    /// UI-theme `accent` for highlights.
    pub fn from_renderer(r: &Renderer, accent: [u8; 4]) -> Self {
        let bg = r.background_color();
        let fg = r.foreground_color();
        Self {
            bar_bg: shade(bg, 0.82),
            active_bg: bg,
            text: fg,
            dim_text: mix(fg, bg, 0.45),
            accent,
        }
    }
}

pub struct TabHit {
    pub index: usize,
    pub rect: Rect,
    pub close: Rect,
}

pub struct TabBarHits {
    pub tabs: Vec<TabHit>,
    pub new_tab: Rect,
}

/// Draw the tab bar and return click regions. `titles` and `active` describe the
/// open tabs.
pub fn draw_tab_bar(
    r: &mut Renderer,
    queue: &wgpu::Queue,
    layout: &Layout,
    titles: &[String],
    active: usize,
    colors: &ChromeColors,
    rename: Option<&str>,
) -> TabBarHits {
    let bar = layout.bar;
    r.fill_rect(bar.x, bar.y, bar.w, bar.h, colors.bar_bg);

    let cell_w = r.text_width("M").max(1.0);
    let n = titles.len().max(1);

    if layout.horizontal {
        let avail = (bar.w - NEW_TAB_W).max(0.0);
        let tab_w = (avail / n as f32).clamp(MIN_TAB_W, MAX_TAB_W);
        let tab_h = bar.h;
        let mut tabs = Vec::with_capacity(titles.len());
        let mut x = bar.x;
        for (i, title) in titles.iter().enumerate() {
            let rect = Rect {
                x,
                y: bar.y,
                w: tab_w,
                h: tab_h,
            };
            let editing = if i == active { rename } else { None };
            draw_tab_label(
                r,
                queue,
                rect,
                title,
                i == active,
                colors,
                cell_w,
                true,
                editing,
            );
            let close = Rect {
                x: rect.x + rect.w - CLOSE_W - PAD * 0.5,
                y: rect.y,
                w: CLOSE_W,
                h: rect.h,
            };
            r.text(queue, close.x, close.y + PAD, "x", colors.dim_text);
            tabs.push(TabHit {
                index: i,
                rect,
                close,
            });
            x += tab_w;
        }
        let new_tab = Rect {
            x: x.min(bar.x + bar.w - NEW_TAB_W),
            y: bar.y,
            w: NEW_TAB_W,
            h: tab_h,
        };
        r.text(queue, new_tab.x + PAD, new_tab.y + PAD, "+", colors.text);
        TabBarHits { tabs, new_tab }
    } else {
        let row_h = (r_cell_h(r) + PAD * 2.0).ceil();
        let mut tabs = Vec::with_capacity(titles.len());
        let mut y = bar.y;
        for (i, title) in titles.iter().enumerate() {
            let rect = Rect {
                x: bar.x,
                y,
                w: bar.w,
                h: row_h,
            };
            let editing = if i == active { rename } else { None };
            draw_tab_label(
                r,
                queue,
                rect,
                title,
                i == active,
                colors,
                cell_w,
                false,
                editing,
            );
            let close = Rect {
                x: rect.x + rect.w - CLOSE_W - PAD,
                y: rect.y,
                w: CLOSE_W,
                h: rect.h,
            };
            r.text(queue, close.x, close.y + PAD, "x", colors.dim_text);
            tabs.push(TabHit {
                index: i,
                rect,
                close,
            });
            y += row_h;
        }
        let new_tab = Rect {
            x: bar.x,
            y,
            w: bar.w,
            h: row_h,
        };
        r.text(
            queue,
            new_tab.x + PAD,
            new_tab.y + PAD,
            "+ new tab",
            colors.dim_text,
        );
        TabBarHits { tabs, new_tab }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tab_label(
    r: &mut Renderer,
    queue: &wgpu::Queue,
    rect: Rect,
    title: &str,
    active: bool,
    colors: &ChromeColors,
    cell_w: f32,
    underline_accent: bool,
    editing: Option<&str>,
) {
    r.fill_rect(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        if active {
            colors.active_bg
        } else {
            colors.bar_bg
        },
    );
    if active && underline_accent {
        r.fill_rect(rect.x, rect.y + rect.h - 2.0, rect.w, 2.0, colors.accent);
    } else if active {
        r.fill_rect(rect.x, rect.y, 2.0, rect.h, colors.accent);
    }
    let max_chars = (((rect.w - PAD * 2.0 - CLOSE_W) / cell_w).floor() as usize).max(1);
    let (label, text_color) = match editing {
        // Show the tail of the edit buffer (what's being typed) plus a caret.
        Some(buf) => (
            truncate_left(&format!("{buf}\u{258f}"), max_chars),
            colors.text,
        ),
        None => (
            truncate(title, max_chars),
            if active { colors.text } else { colors.dim_text },
        ),
    };
    r.text(queue, rect.x + PAD, rect.y + PAD, &label, text_color);
}

fn r_cell_h(r: &Renderer) -> f32 {
    r.cell_size().1
}

fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Keep the rightmost `max` chars (used while typing a rename so the caret stays
/// visible).
fn truncate_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    s.chars().skip(count - max).collect()
}

fn shade(c: [u8; 4], factor: f32) -> [u8; 4] {
    [
        (c[0] as f32 * factor) as u8,
        (c[1] as f32 * factor) as u8,
        (c[2] as f32 * factor) as u8,
        c[3],
    ]
}

fn mix(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let lerp = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t) as u8;
    [lerp(a[0], b[0]), lerp(a[1], b[1]), lerp(a[2], b[2]), 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_layout_splits_off_top_bar() {
        let l = compute_layout(800.0, 600.0, 20.0, true);
        assert_eq!(l.bar.h, 36.0); // 20 + 8*2
        assert_eq!(l.viewport.y, 36.0);
        assert_eq!(l.viewport.h, 564.0);
        assert_eq!(l.viewport.w, 800.0);
    }

    #[test]
    fn vertical_layout_splits_off_left_bar() {
        let l = compute_layout(1000.0, 600.0, 20.0, false);
        assert!(l.bar.w >= 120.0 && l.bar.w <= 240.0);
        assert_eq!(l.viewport.x, l.bar.w);
        assert_eq!(l.viewport.w, 1000.0 - l.bar.w);
    }

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect {
            x: 10.0,
            y: 10.0,
            w: 20.0,
            h: 20.0,
        };
        assert!(r.contains(10.0, 10.0));
        assert!(r.contains(29.9, 29.9));
        assert!(!r.contains(30.0, 20.0));
        assert!(!r.contains(9.9, 20.0));
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "he…");
        assert_eq!(truncate("hi", 2), "hi");
    }
}
