//! Native chrome: window layout and the tab bar. Drawn with the phantom-gfx
//! frame API; returns hit regions so the app can route clicks.

use std::time::Instant;

use phantom_emu::ScrollState;
use phantom_gfx::Renderer;

const PAD: f32 = 8.0;
const CLOSE_W: f32 = 16.0;
const NEW_TAB_W: f32 = 30.0;
const SETTINGS_W: f32 = 34.0;
const MAX_TAB_W: f32 = 220.0;
const MIN_TAB_W: f32 = 70.0;
const TERMINAL_MARGIN: f32 = 8.0;
const SCROLLBAR_W: f32 = 4.0;
const SCROLLBAR_HOVER_W: f32 = 10.0;
const SCROLLBAR_HIT_W: f32 = 14.0;
const SCROLLBAR_INSET: f32 = 4.0;
const SCROLLBAR_MIN_THUMB_H: f32 = 24.0;
const HOVER_FADE_SECONDS: f32 = 0.14;
const HOVER_EPSILON: f32 = 0.01;

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
                x: TERMINAL_MARGIN,
                y: bar_h + TERMINAL_MARGIN,
                w: (w - TERMINAL_MARGIN * 2.0).max(0.0),
                h: (h - bar_h - TERMINAL_MARGIN * 2.0).max(0.0),
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
                x: bar_w + TERMINAL_MARGIN,
                y: TERMINAL_MARGIN,
                w: (w - bar_w - TERMINAL_MARGIN * 2.0).max(0.0),
                h: (h - TERMINAL_MARGIN * 2.0).max(0.0),
            },
            horizontal,
        }
    }
}

/// Area behind the terminal viewport. This includes the intentional text inset
/// so backdrops can fill the pane while terminal glyphs stay padded.
pub fn terminal_pane(layout: &Layout) -> Rect {
    if layout.horizontal {
        Rect {
            x: layout.bar.x,
            y: layout.bar.y + layout.bar.h,
            w: layout.bar.w,
            h: layout.viewport.h + TERMINAL_MARGIN * 2.0,
        }
    } else {
        Rect {
            x: layout.bar.x + layout.bar.w,
            y: layout.bar.y,
            w: layout.viewport.w + TERMINAL_MARGIN * 2.0,
            h: layout.bar.h,
        }
    }
}

pub fn terminal_scrollbar_track(layout: &Layout) -> Rect {
    let pane = terminal_pane(layout);
    Rect {
        x: pane.x + pane.w - SCROLLBAR_INSET - SCROLLBAR_W,
        y: pane.y + SCROLLBAR_INSET,
        w: SCROLLBAR_W,
        h: (pane.h - SCROLLBAR_INSET * 2.0).max(0.0),
    }
}

pub fn terminal_scrollbar_hit_track(layout: &Layout) -> Rect {
    let track = terminal_scrollbar_track(layout);
    Rect {
        x: track.x + track.w - SCROLLBAR_HIT_W,
        y: track.y,
        w: SCROLLBAR_HIT_W,
        h: track.h,
    }
}

pub fn scrollbar_thumb(track: Rect, scroll: ScrollState) -> Option<Rect> {
    if !scroll.is_scrollable() || track.h <= 0.0 {
        return None;
    }

    let content_rows = scroll.history + scroll.viewport_rows;
    let visible_fraction = scroll.viewport_rows as f32 / content_rows.max(1) as f32;
    let thumb_h = (track.h * visible_fraction).clamp(SCROLLBAR_MIN_THUMB_H.min(track.h), track.h);
    let travel = (track.h - thumb_h).max(0.0);
    let from_top = if scroll.history == 0 {
        0.0
    } else {
        (scroll.history.saturating_sub(scroll.offset) as f32 / scroll.history as f32) * travel
    };

    Some(Rect {
        x: track.x,
        y: track.y + from_top,
        w: track.w,
        h: thumb_h,
    })
}

pub fn terminal_scrollbar_hit_thumb(layout: &Layout, scroll: ScrollState) -> Option<Rect> {
    scrollbar_thumb(terminal_scrollbar_hit_track(layout), scroll)
}

pub fn draw_terminal_scrollbar(
    r: &mut Renderer,
    layout: &Layout,
    scroll: ScrollState,
    colors: &ChromeColors,
    active: bool,
) {
    let track = if active {
        terminal_scrollbar_hit_track(layout)
    } else {
        terminal_scrollbar_track(layout)
    };
    let Some(thumb) = scrollbar_thumb(track, scroll) else {
        return;
    };
    let visual_w = if active {
        SCROLLBAR_HOVER_W
    } else {
        SCROLLBAR_W
    };
    let track = Rect {
        x: track.x + track.w - visual_w,
        w: visual_w,
        ..track
    };
    let thumb = Rect {
        x: thumb.x + thumb.w - visual_w,
        w: visual_w,
        ..thumb
    };
    r.fill_rect(
        track.x,
        track.y,
        track.w,
        track.h,
        with_alpha(colors.text, if active { 28 } else { 18 }),
    );
    r.fill_rect(
        thumb.x,
        thumb.y,
        thumb.w,
        thumb.h,
        with_alpha(colors.text, if active { 132 } else { 96 }),
    );
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
    pub settings: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragIndicator {
    pub source: usize,
    pub drop_index: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChromeHoverTarget {
    #[default]
    None,
    Tab(usize),
    Close(usize),
    Settings,
}

impl ChromeHoverTarget {
    pub fn is_clickable(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Default)]
pub struct ChromeAnimationState {
    hover: ChromeHoverTarget,
    tab_hover: Vec<f32>,
    close_hover: Vec<f32>,
    settings_hover: f32,
    last_tick: Option<Instant>,
}

impl ChromeAnimationState {
    pub fn set_hover(&mut self, hover: ChromeHoverTarget) -> bool {
        if self.hover == hover {
            return false;
        }
        self.hover = hover;
        true
    }

    pub fn advance(&mut self, now: Instant, tab_count: usize) -> bool {
        self.tab_hover.resize(tab_count, 0.0);
        self.close_hover.resize(tab_count, 0.0);

        let dt = self.last_tick.map_or(1.0 / 60.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        self.last_tick = Some(now);
        let amount = (dt / HOVER_FADE_SECONDS).clamp(0.0, 1.0);

        let mut animating = false;
        for index in 0..tab_count {
            let tab_target = match self.hover {
                ChromeHoverTarget::Tab(i) | ChromeHoverTarget::Close(i) if i == index => 1.0,
                _ => 0.0,
            };
            let close_target = if self.hover == ChromeHoverTarget::Close(index) {
                1.0
            } else {
                0.0
            };
            animating |= ease_value(&mut self.tab_hover[index], tab_target, amount);
            animating |= ease_value(&mut self.close_hover[index], close_target, amount);
        }
        let settings_target = if self.hover == ChromeHoverTarget::Settings {
            1.0
        } else {
            0.0
        };
        animating |= ease_value(&mut self.settings_hover, settings_target, amount);
        animating
    }

    fn tab(&self, index: usize) -> f32 {
        self.tab_hover.get(index).copied().unwrap_or_default()
    }

    fn close(&self, index: usize) -> f32 {
        self.close_hover.get(index).copied().unwrap_or_default()
    }

    fn settings(&self) -> f32 {
        self.settings_hover
    }
}

impl TabBarHits {
    pub fn hover_target(&self, px: f32, py: f32) -> ChromeHoverTarget {
        if self.settings.contains(px, py) {
            return ChromeHoverTarget::Settings;
        }
        for tab in &self.tabs {
            if tab.close.contains(px, py) {
                return ChromeHoverTarget::Close(tab.index);
            }
        }
        for tab in &self.tabs {
            if tab.rect.contains(px, py) {
                return ChromeHoverTarget::Tab(tab.index);
            }
        }
        ChromeHoverTarget::None
    }

    pub fn tab_body_at(&self, px: f32, py: f32) -> Option<usize> {
        for tab in &self.tabs {
            if tab.rect.contains(px, py) && !tab.close.contains(px, py) {
                return Some(tab.index);
            }
        }
        None
    }

    pub fn drop_index(&self, px: f32, py: f32, horizontal: bool) -> usize {
        for tab in &self.tabs {
            let midpoint = if horizontal {
                tab.rect.x + tab.rect.w * 0.5
            } else {
                tab.rect.y + tab.rect.h * 0.5
            };
            let pointer = if horizontal { px } else { py };
            if pointer < midpoint {
                return tab.index;
            }
        }
        self.tabs.len()
    }
}

/// Draw the tab bar and return click regions. `titles` and `active` describe the
/// open tabs.
#[allow(clippy::too_many_arguments)]
pub fn draw_tab_bar(
    r: &mut Renderer,
    queue: &wgpu::Queue,
    layout: &Layout,
    titles: &[String],
    active: usize,
    colors: &ChromeColors,
    rename: Option<&str>,
    settings_open: bool,
    animations: &ChromeAnimationState,
    drag_indicator: Option<DragIndicator>,
) -> TabBarHits {
    let bar = layout.bar;
    r.fill_rect(bar.x, bar.y, bar.w, bar.h, colors.bar_bg);

    let cell_w = r.text_width("M").max(1.0);
    let n = titles.len().max(1);

    if layout.horizontal {
        let settings = Rect {
            x: bar.x + (bar.w - SETTINGS_W).max(0.0),
            y: bar.y,
            w: SETTINGS_W,
            h: bar.h,
        };
        let avail = (bar.w - NEW_TAB_W - SETTINGS_W).max(0.0);
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
            let dragging = drag_indicator.is_some_and(|drag| drag.source == i);
            let hover = if drag_indicator.is_some() {
                0.0
            } else {
                animations.tab(i)
            };
            draw_tab_label(
                r,
                queue,
                rect,
                title,
                i == active,
                colors,
                cell_w,
                true,
                hover,
                dragging,
                editing,
            );
            let close = Rect {
                x: rect.x + rect.w - CLOSE_W - PAD * 0.5,
                y: rect.y,
                w: CLOSE_W,
                h: rect.h,
            };
            let close_hover = if drag_indicator.is_some() {
                0.0
            } else {
                animations.close(i)
            };
            draw_close_button(r, queue, close, colors, close_hover);
            tabs.push(TabHit {
                index: i,
                rect,
                close,
            });
            x += tab_w;
        }
        let new_tab = Rect {
            x: x.min(settings.x - NEW_TAB_W).max(bar.x),
            y: bar.y,
            w: NEW_TAB_W,
            h: tab_h,
        };
        r.text(queue, new_tab.x + PAD, new_tab.y + PAD, "+", colors.text);
        draw_settings_button(r, settings, colors, settings_open, animations.settings());
        let hits = TabBarHits {
            tabs,
            new_tab,
            settings,
        };
        draw_drop_indicator(
            r,
            &hits,
            colors,
            layout.horizontal,
            drag_indicator.and_then(|drag| drag.drop_index),
        );
        hits
    } else {
        let row_h = (r_cell_h(r) + PAD * 2.0).ceil();
        let settings = Rect {
            x: bar.x + (bar.w - SETTINGS_W).max(0.0),
            y: bar.y,
            w: SETTINGS_W,
            h: row_h,
        };
        let mut tabs = Vec::with_capacity(titles.len());
        let mut y = bar.y + row_h;
        for (i, title) in titles.iter().enumerate() {
            let rect = Rect {
                x: bar.x,
                y,
                w: bar.w,
                h: row_h,
            };
            let editing = if i == active { rename } else { None };
            let dragging = drag_indicator.is_some_and(|drag| drag.source == i);
            let hover = if drag_indicator.is_some() {
                0.0
            } else {
                animations.tab(i)
            };
            draw_tab_label(
                r,
                queue,
                rect,
                title,
                i == active,
                colors,
                cell_w,
                false,
                hover,
                dragging,
                editing,
            );
            let close = Rect {
                x: rect.x + rect.w - CLOSE_W - PAD,
                y: rect.y,
                w: CLOSE_W,
                h: rect.h,
            };
            let close_hover = if drag_indicator.is_some() {
                0.0
            } else {
                animations.close(i)
            };
            draw_close_button(r, queue, close, colors, close_hover);
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
        draw_settings_button(r, settings, colors, settings_open, animations.settings());
        let hits = TabBarHits {
            tabs,
            new_tab,
            settings,
        };
        draw_drop_indicator(
            r,
            &hits,
            colors,
            layout.horizontal,
            drag_indicator.and_then(|drag| drag.drop_index),
        );
        hits
    }
}

fn draw_drop_indicator(
    r: &mut Renderer,
    hits: &TabBarHits,
    colors: &ChromeColors,
    horizontal: bool,
    indicator: Option<usize>,
) {
    let Some(index) = indicator else {
        return;
    };
    if hits.tabs.is_empty() {
        return;
    }
    let index = index.min(hits.tabs.len());
    let rect = if index == hits.tabs.len() {
        hits.tabs[hits.tabs.len() - 1].rect
    } else {
        hits.tabs[index].rect
    };
    if horizontal {
        let x = if index == hits.tabs.len() {
            rect.x + rect.w
        } else {
            rect.x
        };
        r.fill_rect(x - 1.5, rect.y + 5.0, 3.0, rect.h - 10.0, colors.accent);
    } else {
        let y = if index == hits.tabs.len() {
            rect.y + rect.h
        } else {
            rect.y
        };
        r.fill_rect(rect.x + 6.0, y - 1.5, rect.w - 12.0, 3.0, colors.accent);
    }
}

fn draw_settings_button(
    r: &mut Renderer,
    rect: Rect,
    colors: &ChromeColors,
    active: bool,
    hover: f32,
) {
    let hover = hover.clamp(0.0, 1.0);
    if active {
        r.fill_rect(
            rect.x + 4.0,
            rect.y + 4.0,
            rect.w - 8.0,
            rect.h - 8.0,
            colors.active_bg,
        );
        r.fill_rect(
            rect.x + rect.w - 2.0,
            rect.y + 6.0,
            2.0,
            rect.h - 12.0,
            colors.accent,
        );
    }
    let (cell_w, cell_h) = r.cell_size();
    let base_icon = if active { colors.text } else { colors.dim_text };
    let color = mix(base_icon, colors.accent, hover);
    draw_settings_icon(
        r,
        rect.x + (rect.w - cell_w) * 0.5,
        rect.y + (rect.h - cell_h) * 0.5,
        cell_w,
        cell_h,
        color,
    );
}

fn draw_settings_icon(r: &mut Renderer, x: f32, y: f32, w: f32, h: f32, color: [u8; 4]) {
    let line_w = (w * 0.9).max(12.0);
    let line_h = 2.0;
    let knob = 4.0;
    let left = x + (w - line_w) * 0.5;
    let top = y + h * 0.28;
    let gap = h * 0.22;
    for (row, knob_offset) in [(0.0, 0.22), (1.0, 0.68), (2.0, 0.42)] {
        let line_y = top + row * gap;
        r.fill_rect(left, line_y, line_w, line_h, color);
        r.fill_rect(
            left + line_w * knob_offset - knob * 0.5,
            line_y - 1.0,
            knob,
            knob,
            color,
        );
    }
}

fn draw_close_button(
    r: &mut Renderer,
    queue: &wgpu::Queue,
    rect: Rect,
    colors: &ChromeColors,
    hover: f32,
) {
    let hover = hover.clamp(0.0, 1.0);
    if hover > 0.0 {
        let size = rect.w.min(rect.h) - 4.0;
        r.fill_rect(
            rect.x + (rect.w - size) * 0.5,
            rect.y + (rect.h - size) * 0.5,
            size,
            size,
            with_alpha(colors.accent, (32.0 * hover) as u8),
        );
    }
    let (cell_w, cell_h) = r.cell_size();
    r.text(
        queue,
        rect.x + (rect.w - cell_w) * 0.5,
        rect.y + (rect.h - cell_h) * 0.5,
        "\u{00d7}",
        mix(colors.dim_text, colors.accent, hover),
    );
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
    hover: f32,
    dragging: bool,
    editing: Option<&str>,
) {
    let hover = hover.clamp(0.0, 1.0);
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
    if hover > 0.0 {
        r.fill_rect(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            with_alpha(colors.text, (22.0 * hover) as u8),
        );
    }
    if dragging {
        draw_drag_source(r, rect, colors);
    }
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
            if dragging || active {
                colors.text
            } else {
                colors.dim_text
            },
        ),
    };
    r.text(queue, rect.x + PAD, rect.y + PAD, &label, text_color);
}

fn draw_drag_source(r: &mut Renderer, rect: Rect, colors: &ChromeColors) {
    r.fill_rect(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        with_alpha(colors.accent, 28),
    );
    r.fill_rect(rect.x, rect.y, rect.w, 2.0, with_alpha(colors.accent, 170));
    r.fill_rect(
        rect.x,
        rect.y + rect.h - 2.0,
        rect.w,
        2.0,
        with_alpha(colors.accent, 170),
    );
    r.fill_rect(rect.x, rect.y, 2.0, rect.h, with_alpha(colors.accent, 130));
    r.fill_rect(
        rect.x + rect.w - 2.0,
        rect.y,
        2.0,
        rect.h,
        with_alpha(colors.accent, 130),
    );
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

fn ease_value(value: &mut f32, target: f32, amount: f32) -> bool {
    *value += (target - *value) * amount;
    if (*value - target).abs() <= HOVER_EPSILON {
        *value = target;
        false
    } else {
        true
    }
}

fn with_alpha(mut color: [u8; 4], alpha: u8) -> [u8; 4] {
    color[3] = alpha;
    color
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

    fn tab_hit(index: usize, x: f32, y: f32) -> TabHit {
        TabHit {
            index,
            rect: Rect {
                x,
                y,
                w: 100.0,
                h: 30.0,
            },
            close: Rect {
                x: x + 80.0,
                y,
                w: 20.0,
                h: 30.0,
            },
        }
    }

    fn horizontal_hits() -> TabBarHits {
        TabBarHits {
            tabs: vec![
                tab_hit(0, 0.0, 0.0),
                tab_hit(1, 100.0, 0.0),
                tab_hit(2, 200.0, 0.0),
            ],
            new_tab: Rect {
                x: 300.0,
                y: 0.0,
                w: 30.0,
                h: 30.0,
            },
            settings: Rect {
                x: 330.0,
                y: 0.0,
                w: 30.0,
                h: 30.0,
            },
        }
    }

    fn vertical_hits() -> TabBarHits {
        TabBarHits {
            tabs: vec![
                tab_hit(0, 0.0, 0.0),
                tab_hit(1, 0.0, 30.0),
                tab_hit(2, 0.0, 60.0),
            ],
            new_tab: Rect {
                x: 0.0,
                y: 90.0,
                w: 100.0,
                h: 30.0,
            },
            settings: Rect {
                x: 0.0,
                y: 120.0,
                w: 100.0,
                h: 30.0,
            },
        }
    }

    #[test]
    fn horizontal_layout_splits_off_top_bar() {
        let l = compute_layout(800.0, 600.0, 20.0, true);
        assert_eq!(l.bar.h, 36.0); // 20 + 8*2
        assert_eq!(l.viewport.x, 8.0);
        assert_eq!(l.viewport.y, 44.0);
        assert_eq!(l.viewport.h, 548.0);
        assert_eq!(l.viewport.w, 784.0);
    }

    #[test]
    fn vertical_layout_splits_off_left_bar() {
        let l = compute_layout(1000.0, 600.0, 20.0, false);
        assert!(l.bar.w >= 120.0 && l.bar.w <= 240.0);
        assert_eq!(l.viewport.x, l.bar.w + 8.0);
        assert_eq!(l.viewport.y, 8.0);
        assert_eq!(l.viewport.w, 1000.0 - l.bar.w - 16.0);
        assert_eq!(l.viewport.h, 584.0);
    }

    #[test]
    fn horizontal_terminal_pane_wraps_viewport_inset_below_tab_bar() {
        let l = compute_layout(800.0, 600.0, 20.0, true);
        let pane = terminal_pane(&l);

        assert_eq!(pane.x, 0.0);
        assert_eq!(pane.y, l.bar.h);
        assert_eq!(pane.w, 800.0);
        assert_eq!(pane.h, 600.0 - l.bar.h);
    }

    #[test]
    fn vertical_terminal_pane_wraps_viewport_inset_beside_tab_bar() {
        let l = compute_layout(1000.0, 600.0, 20.0, false);
        let pane = terminal_pane(&l);

        assert_eq!(pane.x, l.bar.w);
        assert_eq!(pane.y, 0.0);
        assert_eq!(pane.w, 1000.0 - l.bar.w);
        assert_eq!(pane.h, 600.0);
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
    fn scrollbar_thumb_tracks_scroll_offset() {
        let track = Rect {
            x: 90.0,
            y: 10.0,
            w: 4.0,
            h: 100.0,
        };
        let at_bottom = scrollbar_thumb(
            track,
            ScrollState {
                offset: 0,
                history: 100,
                viewport_rows: 25,
            },
        )
        .unwrap();
        let at_top = scrollbar_thumb(
            track,
            ScrollState {
                offset: 100,
                history: 100,
                viewport_rows: 25,
            },
        )
        .unwrap();

        assert_eq!(at_bottom.y + at_bottom.h, track.y + track.h);
        assert_eq!(at_top.y, track.y);
        assert_eq!(at_bottom.h, at_top.h);
    }

    #[test]
    fn scrollbar_thumb_is_hidden_without_history() {
        let track = Rect {
            x: 90.0,
            y: 10.0,
            w: 4.0,
            h: 100.0,
        };
        assert!(scrollbar_thumb(
            track,
            ScrollState {
                offset: 0,
                history: 0,
                viewport_rows: 25,
            },
        )
        .is_none());
    }

    #[test]
    fn scrollbar_hit_track_is_wider_but_right_aligned() {
        let layout = compute_layout(800.0, 600.0, 20.0, true);
        let visual = terminal_scrollbar_track(&layout);
        let hit = terminal_scrollbar_hit_track(&layout);

        assert!(hit.w > visual.w);
        assert_eq!(hit.x + hit.w, visual.x + visual.w);
        assert_eq!(hit.y, visual.y);
        assert_eq!(hit.h, visual.h);
    }

    #[test]
    fn hover_target_prefers_close_over_tab() {
        let hits = TabBarHits {
            tabs: vec![tab_hit(0, 0.0, 0.0)],
            new_tab: Rect {
                x: 100.0,
                y: 0.0,
                w: 30.0,
                h: 30.0,
            },
            settings: Rect {
                x: 130.0,
                y: 0.0,
                w: 30.0,
                h: 30.0,
            },
        };

        assert_eq!(hits.hover_target(10.0, 10.0), ChromeHoverTarget::Tab(0));
        assert_eq!(hits.hover_target(90.0, 10.0), ChromeHoverTarget::Close(0));
        assert_eq!(hits.hover_target(140.0, 10.0), ChromeHoverTarget::Settings);
    }

    #[test]
    fn tab_body_hit_excludes_close_button() {
        let hits = horizontal_hits();

        assert_eq!(hits.tab_body_at(10.0, 10.0), Some(0));
        assert_eq!(hits.tab_body_at(90.0, 10.0), None);
        assert_eq!(hits.tab_body_at(310.0, 10.0), None);
    }

    #[test]
    fn horizontal_drop_index_uses_tab_midpoints() {
        let hits = horizontal_hits();

        assert_eq!(hits.drop_index(49.0, 10.0, true), 0);
        assert_eq!(hits.drop_index(51.0, 10.0, true), 1);
        assert_eq!(hits.drop_index(151.0, 10.0, true), 2);
        assert_eq!(hits.drop_index(299.0, 10.0, true), 3);
    }

    #[test]
    fn vertical_drop_index_uses_tab_midpoints() {
        let hits = vertical_hits();

        assert_eq!(hits.drop_index(10.0, 14.0, false), 0);
        assert_eq!(hits.drop_index(10.0, 16.0, false), 1);
        assert_eq!(hits.drop_index(10.0, 46.0, false), 2);
        assert_eq!(hits.drop_index(10.0, 89.0, false), 3);
    }

    #[test]
    fn chrome_animation_eases_toward_hover_target() {
        let now = Instant::now();
        let mut state = ChromeAnimationState::default();
        assert!(state.set_hover(ChromeHoverTarget::Settings));
        assert!(state.advance(now, 1));
        assert!(state.settings() > 0.0);

        assert!(state.set_hover(ChromeHoverTarget::None));
        assert!(!state.advance(now + std::time::Duration::from_millis(500), 1));
        assert_eq!(state.settings(), 0.0);
    }

    #[test]
    fn chrome_hover_targets_report_clickability() {
        assert!(!ChromeHoverTarget::None.is_clickable());
        assert!(ChromeHoverTarget::Tab(0).is_clickable());
        assert!(ChromeHoverTarget::Close(0).is_clickable());
        assert!(ChromeHoverTarget::Settings.is_clickable());
    }

    #[test]
    fn close_hover_keeps_parent_tab_hovered() {
        let now = Instant::now();
        let mut state = ChromeAnimationState::default();
        assert!(state.set_hover(ChromeHoverTarget::Close(0)));
        assert!(state.advance(now, 1));

        assert!(state.tab(0) > 0.0);
        assert!(state.close(0) > 0.0);
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "he…");
        assert_eq!(truncate("hi", 2), "hi");
    }
}
