//! Native chrome: window layout and the tab bar. Drawn with the phantom-gfx
//! frame API; returns hit regions so the app can route clicks.

use std::time::Instant;

use phantom_emu::ScrollState;
use phantom_gfx::Renderer;

const PAD: f32 = 8.0;
const CLOSE_W: f32 = 16.0;
const NEW_TAB_W: f32 = 30.0;
const SETTINGS_W: f32 = 34.0;
const EPHEMERAL_W: f32 = 24.0;
const TAB_W: f32 = 220.0;
const MIN_TAB_W: f32 = 70.0;
const VERTICAL_BAR_W: f32 = 220.0;
const MIN_VERTICAL_VIEWPORT_W: f32 = 80.0;
const TERMINAL_MARGIN: f32 = 8.0;
const CUSTOM_EDGE_INSET: f32 = 1.0;
const CUSTOM_TITLEBAR_H: f32 = 32.0;
const MAC_TRAFFIC_LIGHT_SPACE: f32 = 76.0;
const LINUX_WINDOW_BUTTON_W: f32 = 42.0;
const WINDOW_BORDER_W: f32 = 1.0;
const CUSTOM_TAB_DRAG_HANDLE_W: f32 = 12.0;
const DRAG_REGION_SHADE_ALPHA: u8 = 80;
const SCROLLBAR_W: f32 = 4.0;
const SCROLLBAR_HOVER_W: f32 = 10.0;
const SCROLLBAR_HIT_W: f32 = 14.0;
const SCROLLBAR_INSET: f32 = 4.0;
const SCROLLBAR_MIN_THUMB_H: f32 = 24.0;
#[cfg(any(test, target_os = "linux"))]
const WINDOW_RESIZE_EDGE_W: f32 = 5.0;
#[cfg(any(test, target_os = "linux"))]
const WINDOW_RESIZE_CORNER_LEN: f32 = 12.0;
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
    pub const EMPTY: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// Overlap of two rects; `None` when they don't intersect.
    pub fn intersect(&self, other: Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        (x1 > x0 && y1 > y0).then_some(Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// Clamp a strip scroll offset to its content, optionally adjusting it so the
/// `ensure` span (content coordinates) is fully visible. Returns the adjusted
/// scroll and the maximum scroll.
fn strip_scroll(
    content: f32,
    viewport: f32,
    scroll: f32,
    ensure: Option<(f32, f32)>,
) -> (f32, f32) {
    let max_scroll = (content - viewport).max(0.0);
    let mut scroll = scroll.clamp(0.0, max_scroll);
    if let Some((lo, hi)) = ensure {
        if lo < scroll {
            scroll = lo;
        } else if hi > scroll + viewport {
            scroll = hi - viewport;
        }
        scroll = scroll.clamp(0.0, max_scroll);
    }
    (scroll, max_scroll)
}

fn px(layout: &Layout, value: f32) -> f32 {
    value * layout.scale_factor
}

/// Window split into the tab bar and the terminal viewport.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub bar: Rect,
    pub titlebar: Option<Rect>,
    pub viewport: Rect,
    pub horizontal: bool,
    pub custom_chrome: bool,
    scale_factor: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowChrome {
    System,
    Custom,
}

/// Edge or corner owned by an invisible custom-chrome resize handle.
#[cfg(any(test, target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowResizeHandle {
    East,
    North,
    NorthEast,
    NorthWest,
    South,
    SouthEast,
    SouthWest,
    West,
}

/// Return the thin, DPI-scaled custom-chrome resize handle under a physical
/// window pixel. `excluded` regions retain priority over window resizing.
#[cfg(any(test, target_os = "linux"))]
pub fn window_resize_handle_at(
    width: f32,
    height: f32,
    scale_factor: f32,
    window_chrome: WindowChrome,
    excluded: Option<Rect>,
    px: f32,
    py: f32,
) -> Option<WindowResizeHandle> {
    if !window_chrome.is_custom()
        || width <= 0.0
        || height <= 0.0
        || px < 0.0
        || py < 0.0
        || px >= width
        || py >= height
        || excluded.is_some_and(|rect| rect.contains(px, py))
    {
        return None;
    }

    let scale_factor = scale_factor.max(1.0);
    let edge = WINDOW_RESIZE_EDGE_W * scale_factor;
    let corner = WINDOW_RESIZE_CORNER_LEN * scale_factor;
    let left = px < edge;
    let right = px >= width - edge;
    let top = py < edge;
    let bottom = py >= height - edge;
    let near_left_corner = px < corner;
    let near_right_corner = px >= width - corner;
    let near_top_corner = py < corner;
    let near_bottom_corner = py >= height - corner;

    match () {
        _ if (left && near_top_corner) || (top && near_left_corner) => {
            Some(WindowResizeHandle::NorthWest)
        }
        _ if (right && near_top_corner) || (top && near_right_corner) => {
            Some(WindowResizeHandle::NorthEast)
        }
        _ if (left && near_bottom_corner) || (bottom && near_left_corner) => {
            Some(WindowResizeHandle::SouthWest)
        }
        _ if (right && near_bottom_corner) || (bottom && near_right_corner) => {
            Some(WindowResizeHandle::SouthEast)
        }
        _ if top => Some(WindowResizeHandle::North),
        _ if bottom => Some(WindowResizeHandle::South),
        _ if left => Some(WindowResizeHandle::West),
        _ if right => Some(WindowResizeHandle::East),
        _ => None,
    }
}

impl WindowChrome {
    pub fn from_config(value: &str) -> Self {
        if value == "custom" {
            Self::Custom
        } else {
            Self::System
        }
    }

    pub fn is_custom(self) -> bool {
        matches!(self, Self::Custom)
    }
}

/// Compute the layout for a window of `w` x `h` physical px. `horizontal` puts
/// the tab bar across the top; otherwise down the left side.
#[cfg(test)]
fn compute_layout(
    w: f32,
    h: f32,
    cell_h: f32,
    horizontal: bool,
    window_chrome: WindowChrome,
) -> Layout {
    compute_layout_scaled(w, h, cell_h, horizontal, window_chrome, 1.0)
}

/// Compute layout in physical px while preserving chrome dimensions specified
/// in logical desktop points.
pub fn compute_layout_scaled(
    w: f32,
    h: f32,
    cell_h: f32,
    horizontal: bool,
    window_chrome: WindowChrome,
    scale_factor: f32,
) -> Layout {
    let scale_factor = scale_factor.max(1.0);
    let custom_chrome = window_chrome.is_custom();
    let pad = PAD * scale_factor;
    let terminal_margin = TERMINAL_MARGIN * scale_factor;
    let edge = if custom_chrome {
        CUSTOM_EDGE_INSET * scale_factor
    } else {
        0.0
    };
    let titlebar_h = if custom_chrome {
        (cell_h + pad * 2.0)
            .ceil()
            .max(CUSTOM_TITLEBAR_H * scale_factor)
    } else {
        0.0
    };
    if horizontal {
        let bar_h = if custom_chrome {
            titlebar_h
        } else {
            (cell_h + pad * 2.0).ceil()
        };
        Layout {
            bar: Rect {
                x: edge,
                y: edge,
                w: (w - edge * 2.0).max(0.0),
                h: bar_h,
            },
            titlebar: custom_chrome.then_some(Rect {
                x: edge,
                y: edge,
                w: (w - edge * 2.0).max(0.0),
                h: titlebar_h,
            }),
            viewport: Rect {
                x: edge + terminal_margin,
                y: edge + bar_h + terminal_margin,
                w: (w - edge * 2.0 - terminal_margin * 2.0).max(0.0),
                h: (h - edge * 2.0 - bar_h - terminal_margin * 2.0).max(0.0),
            },
            horizontal,
            custom_chrome,
            scale_factor,
        }
    } else {
        let max_bar_w =
            (w - terminal_margin * 2.0 - MIN_VERTICAL_VIEWPORT_W * scale_factor).max(0.0);
        let bar_w = max_bar_w.min(VERTICAL_BAR_W * scale_factor);
        Layout {
            titlebar: custom_chrome.then_some(Rect {
                x: edge,
                y: edge,
                w: (w - edge * 2.0).max(0.0),
                h: titlebar_h,
            }),
            bar: Rect {
                x: edge,
                y: edge + titlebar_h,
                w: bar_w,
                h: (h - edge * 2.0 - titlebar_h).max(0.0),
            },
            viewport: Rect {
                x: edge + bar_w + terminal_margin,
                y: edge + titlebar_h + terminal_margin,
                w: (w - edge * 2.0 - bar_w - terminal_margin * 2.0).max(0.0),
                h: (h - edge * 2.0 - titlebar_h - terminal_margin * 2.0).max(0.0),
            },
            horizontal,
            custom_chrome,
            scale_factor,
        }
    }
}

/// Reserve an edge-attached right sidebar from terminal content while leaving
/// titlebar/tab chrome full-width. The sidebar starts below chrome, and callers
/// retain the original layout for full-window backdrop drawing.
pub fn reserve_right_sidebar(mut layout: Layout, width: f32) -> Layout {
    let width = width.max(0.0).min(layout.viewport.w);
    layout.viewport.w = (layout.viewport.w - width).max(0.0);
    layout
}

/// Area behind the terminal viewport. This includes the intentional text inset
/// so backdrops can fill the pane while terminal glyphs stay padded.
pub fn terminal_pane(layout: &Layout) -> Rect {
    let terminal_margin = px(layout, TERMINAL_MARGIN);
    Rect {
        x: layout.viewport.x - terminal_margin,
        y: layout.viewport.y - terminal_margin,
        w: layout.viewport.w + terminal_margin * 2.0,
        h: layout.viewport.h + terminal_margin * 2.0,
    }
}

pub fn terminal_scrollbar_track(layout: &Layout) -> Rect {
    let pane = terminal_pane(layout);
    let inset = px(layout, SCROLLBAR_INSET);
    let scrollbar_w = px(layout, SCROLLBAR_W);
    Rect {
        x: pane.x + pane.w - inset - scrollbar_w,
        y: pane.y + inset,
        w: scrollbar_w,
        h: (pane.h - inset * 2.0).max(0.0),
    }
}

pub fn terminal_scrollbar_hit_track(layout: &Layout) -> Rect {
    let track = terminal_scrollbar_track(layout);
    let hit_w = px(layout, SCROLLBAR_HIT_W);
    Rect {
        x: track.x + track.w - hit_w,
        y: track.y,
        w: hit_w,
        h: track.h,
    }
}

pub fn scrollbar_thumb(track: Rect, scroll: ScrollState, scale_factor: f32) -> Option<Rect> {
    if !scroll.is_scrollable() || track.h <= 0.0 {
        return None;
    }

    let content_rows = scroll.history + scroll.viewport_rows;
    let visible_fraction = scroll.viewport_rows as f32 / content_rows.max(1) as f32;
    // The minimum grab size is a logical dimension like every other chrome
    // metric, so scale it to physical px before clamping.
    let min_thumb_h = (SCROLLBAR_MIN_THUMB_H * scale_factor.max(0.1)).min(track.h);
    let thumb_h = (track.h * visible_fraction).clamp(min_thumb_h, track.h);
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
    scrollbar_thumb(
        terminal_scrollbar_hit_track(layout),
        scroll,
        layout.scale_factor,
    )
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
    let Some(thumb) = scrollbar_thumb(track, scroll, layout.scale_factor) else {
        return;
    };
    let scrollbar_w = px(layout, SCROLLBAR_W);
    let hover_w = px(layout, SCROLLBAR_HOVER_W);
    let visual_w = if active { hover_w } else { scrollbar_w };
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControl {
    Close,
    Minimize,
    Maximize,
}

pub struct WindowControlHit {
    pub control: WindowControl,
    pub rect: Rect,
}

pub struct TabBarHits {
    pub tabs: Vec<TabHit>,
    pub new_tab: Rect,
    pub settings: Rect,
    pub titlebar: Option<Rect>,
    pub window_controls: Vec<WindowControlHit>,
    /// Visible region of the (scrollable) tab strip.
    pub tab_strip: Rect,
    /// The scroll offset actually applied this frame (clamped, and adjusted
    /// when a tab was scrolled into view). The app stores this back.
    pub tab_scroll: f32,
    /// Maximum scroll offset; zero when every tab fits.
    pub max_tab_scroll: f32,
}

struct HorizontalTabLayout {
    rects: Vec<Rect>,
    new_tab: Rect,
    strip: Rect,
    scroll: f32,
    max_scroll: f32,
}

#[derive(Debug, Clone, Copy)]
struct TitlebarControlState {
    settings_open: bool,
    ephemeral: bool,
    settings_hover: f32,
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
    NewTab,
    Settings,
    WindowControl(WindowControl),
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
    new_tab_hover: f32,
    settings_hover: f32,
    window_control_hover: [f32; 3],
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
        let new_tab_target = if self.hover == ChromeHoverTarget::NewTab {
            1.0
        } else {
            0.0
        };
        animating |= ease_value(&mut self.new_tab_hover, new_tab_target, amount);
        let settings_target = if self.hover == ChromeHoverTarget::Settings {
            1.0
        } else {
            0.0
        };
        animating |= ease_value(&mut self.settings_hover, settings_target, amount);
        for control in [
            WindowControl::Minimize,
            WindowControl::Maximize,
            WindowControl::Close,
        ] {
            let target = if self.hover == ChromeHoverTarget::WindowControl(control) {
                1.0
            } else {
                0.0
            };
            animating |= ease_value(
                &mut self.window_control_hover[window_control_index(control)],
                target,
                amount,
            );
        }
        animating
    }

    fn tab(&self, index: usize) -> f32 {
        self.tab_hover.get(index).copied().unwrap_or_default()
    }

    fn close(&self, index: usize) -> f32 {
        self.close_hover.get(index).copied().unwrap_or_default()
    }

    fn new_tab(&self) -> f32 {
        self.new_tab_hover
    }

    fn settings(&self) -> f32 {
        self.settings_hover
    }

    fn window_control(&self, control: WindowControl) -> f32 {
        self.window_control_hover[window_control_index(control)]
    }
}

fn window_control_index(control: WindowControl) -> usize {
    match control {
        WindowControl::Minimize => 0,
        WindowControl::Maximize => 1,
        WindowControl::Close => 2,
    }
}

impl TabBarHits {
    pub fn hover_target(&self, px: f32, py: f32) -> ChromeHoverTarget {
        for hit in &self.window_controls {
            if hit.rect.contains(px, py) {
                return ChromeHoverTarget::WindowControl(hit.control);
            }
        }
        if self.settings.contains(px, py) {
            return ChromeHoverTarget::Settings;
        }
        if self.new_tab.contains(px, py) {
            return ChromeHoverTarget::NewTab;
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

    pub fn window_control_at(&self, px: f32, py: f32) -> Option<WindowControl> {
        self.window_controls
            .iter()
            .find(|hit| hit.rect.contains(px, py))
            .map(|hit| hit.control)
    }

    pub fn titlebar_drag_region_contains(&self, px: f32, py: f32) -> bool {
        let Some(titlebar) = self.titlebar else {
            return false;
        };
        if !titlebar.contains(px, py) {
            return false;
        }
        if self.new_tab.contains(px, py) || self.settings.contains(px, py) {
            return false;
        }
        if self
            .window_controls
            .iter()
            .any(|hit| hit.rect.contains(px, py))
        {
            return false;
        }
        !self
            .tabs
            .iter()
            .any(|tab| tab.rect.contains(px, py) || tab.close.contains(px, py))
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
        // Past every visible tab: drop after the last visible one. With a
        // scrolled strip the hits hold real indices, so `tabs.len()` would be
        // wrong (it counts only visible tabs).
        self.tabs.last().map(|tab| tab.index + 1).unwrap_or(0)
    }
}

/// Draw the tab bar and return click regions. `titles` and `active` describe the
/// open tabs. `tab_scroll` is the strip scroll offset from the previous frame
/// (clamped here); `scroll_into_view` scrolls that tab fully into the strip.
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
    ephemeral: bool,
    animations: &ChromeAnimationState,
    drag_indicator: Option<DragIndicator>,
    tab_scroll: f32,
    scroll_into_view: Option<usize>,
) -> TabBarHits {
    let bar = layout.bar;
    draw_chrome_surfaces(r, layout, colors);

    let cell_w = r.text_width("M").max(1.0);
    let titlebar = layout.titlebar;
    let window_controls = window_control_hits(layout);

    if layout.horizontal {
        let right_reserved = custom_right_reserved(layout);
        let settings_w = px(layout, SETTINGS_W);
        let settings = Rect {
            x: bar.x + (bar.w - right_reserved - settings_w).max(0.0),
            y: bar.y,
            w: settings_w,
            h: bar.h,
        };
        let ephemeral_indicator =
            ephemeral.then_some(ephemeral_indicator_rect_for_layout(layout, settings));
        let HorizontalTabLayout {
            rects,
            new_tab,
            strip,
            scroll,
            max_scroll,
        } = horizontal_tab_layout(
            layout,
            settings,
            titles.len(),
            ephemeral,
            tab_scroll,
            scroll_into_view,
        );
        let mut tabs = Vec::with_capacity(rects.len());
        // Scrolled tabs are clipped to the strip: partially visible tabs draw
        // their visible part only, and fully hidden tabs draw (and hit) nothing.
        r.set_clip(strip.x, strip.y, strip.w, strip.h);
        for (i, rect) in rects.iter().copied().enumerate() {
            let Some(visible) = rect.intersect(strip) else {
                continue;
            };
            let title = &titles[i];
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
                layout.scale_factor,
                true,
                hover,
                dragging,
                editing,
            );
            let close = Rect {
                x: rect.x + rect.w - px(layout, CLOSE_W) - px(layout, PAD) * 0.5,
                y: rect.y,
                w: px(layout, CLOSE_W),
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
                rect: visible,
                close: close.intersect(strip).unwrap_or(Rect::EMPTY),
            });
        }
        r.clear_clip();
        if let Some(rect) = horizontal_drag_region_rect(layout, new_tab, settings) {
            r.fill_rect(
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                [0, 0, 0, DRAG_REGION_SHADE_ALPHA],
            );
        }
        draw_new_tab_button(r, queue, new_tab, colors, animations.new_tab());
        if let Some(rect) = ephemeral_indicator {
            draw_ephemeral_indicator(r, rect, colors);
        }
        draw_settings_button(r, settings, colors, settings_open, animations.settings());
        draw_window_controls(r, queue, window_controls.as_slice(), colors, animations);
        let hits = TabBarHits {
            tabs,
            new_tab,
            settings,
            titlebar,
            window_controls,
            tab_strip: strip,
            tab_scroll: scroll,
            max_tab_scroll: max_scroll,
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
        let pad = px(layout, PAD);
        let settings_w = px(layout, SETTINGS_W);
        let row_h = (r_cell_h(r) + pad * 2.0).ceil();
        let sidebar_settings = Rect {
            x: bar.x + (bar.w - settings_w).max(0.0),
            y: bar.y,
            w: settings_w,
            h: row_h,
        };
        let settings = if layout.custom_chrome {
            titlebar
                .map(|rect| titlebar_settings_rect(rect, layout))
                .unwrap_or(sidebar_settings)
        } else {
            sidebar_settings
        };
        let ephemeral_indicator =
            ephemeral.then_some(ephemeral_indicator_rect_for_layout(layout, settings));
        if let Some(titlebar) = titlebar {
            draw_titlebar_controls(
                r,
                queue,
                titlebar,
                layout,
                colors,
                TitlebarControlState {
                    settings_open,
                    ephemeral,
                    settings_hover: animations.settings(),
                },
            );
            draw_window_controls(r, queue, window_controls.as_slice(), colors, animations);
        }
        // Rows live in a vertically scrollable strip below the header row; the
        // "+ new tab" row scrolls with them.
        let strip = Rect {
            x: bar.x,
            y: bar.y + row_h,
            w: bar.w,
            h: (bar.h - row_h).max(0.0),
        };
        let content_h = row_h * (titles.len() + 1) as f32;
        let ensure = scroll_into_view
            .filter(|i| *i < titles.len())
            .map(|i| (i as f32 * row_h, (i + 1) as f32 * row_h));
        let (scroll, max_scroll) = strip_scroll(content_h, strip.h, tab_scroll, ensure);
        let mut tabs = Vec::with_capacity(titles.len());
        let mut y = strip.y - scroll;
        r.set_clip(strip.x, strip.y, strip.w, strip.h);
        for (i, title) in titles.iter().enumerate() {
            let rect = Rect {
                x: bar.x,
                y,
                w: bar.w,
                h: row_h,
            };
            y += row_h;
            let Some(visible) = rect.intersect(strip) else {
                continue;
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
                layout.scale_factor,
                false,
                hover,
                dragging,
                editing,
            );
            let close = Rect {
                x: rect.x + rect.w - px(layout, CLOSE_W) - pad,
                y: rect.y,
                w: px(layout, CLOSE_W),
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
                rect: visible,
                close: close.intersect(strip).unwrap_or(Rect::EMPTY),
            });
        }
        let new_tab = Rect {
            x: bar.x,
            y,
            w: bar.w,
            h: row_h,
        };
        draw_chrome_hover_fill(r, new_tab, colors, animations.new_tab());
        r.text(
            queue,
            new_tab.x + pad,
            new_tab.y + pad,
            "+ new tab",
            colors.dim_text,
        );
        r.clear_clip();
        if !layout.custom_chrome {
            if let Some(rect) = ephemeral_indicator {
                draw_ephemeral_indicator(r, rect, colors);
            }
            draw_settings_button(r, settings, colors, settings_open, animations.settings());
        }
        let hits = TabBarHits {
            tabs,
            new_tab: new_tab.intersect(strip).unwrap_or(Rect::EMPTY),
            settings,
            titlebar,
            window_controls,
            tab_strip: strip,
            tab_scroll: scroll,
            max_tab_scroll: max_scroll,
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

#[cfg(test)]
fn horizontal_tab_rects(
    tab_count: usize,
    tab_start: f32,
    tab_limit: f32,
    y: f32,
    h: f32,
) -> Vec<Rect> {
    horizontal_tab_rects_with_widths(tab_count, tab_start, tab_limit, y, h, TAB_W, MIN_TAB_W, 0.0)
}

/// One tab's width when `tab_count` tabs share `available` px: the default
/// width while there is room, shrinking down to `min_tab_w` when crowded.
/// Below that the strip scrolls instead of shrinking further.
fn horizontal_tab_width(
    tab_count: usize,
    available: f32,
    default_tab_w: f32,
    min_tab_w: f32,
) -> f32 {
    if tab_count == 0 || default_tab_w * tab_count as f32 <= available {
        return default_tab_w;
    }
    (available / tab_count as f32).clamp(min_tab_w, default_tab_w)
}

#[allow(clippy::too_many_arguments)]
fn horizontal_tab_rects_with_widths(
    tab_count: usize,
    tab_start: f32,
    tab_limit: f32,
    y: f32,
    h: f32,
    default_tab_w: f32,
    min_tab_w: f32,
    scroll: f32,
) -> Vec<Rect> {
    let available = (tab_limit - tab_start).max(0.0);
    let tab_w = horizontal_tab_width(tab_count, available, default_tab_w, min_tab_w);
    // Every tab gets a rect; tabs scrolled out of the strip are culled (and
    // partially visible ones clipped) by the caller.
    (0..tab_count)
        .map(|i| Rect {
            x: tab_start + i as f32 * tab_w - scroll,
            y,
            w: tab_w,
            h,
        })
        .collect()
}

fn horizontal_tab_layout(
    layout: &Layout,
    settings: Rect,
    tab_count: usize,
    ephemeral: bool,
    scroll: f32,
    scroll_into_view: Option<usize>,
) -> HorizontalTabLayout {
    let bar = layout.bar;
    let drag_handle_w = custom_tab_drag_handle_w(layout);
    let tab_start = bar.x + custom_left_reserved(layout) + drag_handle_w;
    let status_w = horizontal_status_slot_w(layout, ephemeral);
    let new_tab_w = px(layout, NEW_TAB_W);
    let new_tab_limit = (settings.x - status_w - new_tab_w).max(tab_start);
    let tab_limit = new_tab_limit;
    let available = (tab_limit - tab_start).max(0.0);
    let tab_w = horizontal_tab_width(
        tab_count,
        available,
        px(layout, TAB_W),
        px(layout, MIN_TAB_W),
    );
    let ensure = scroll_into_view
        .filter(|i| *i < tab_count)
        .map(|i| (i as f32 * tab_w, (i + 1) as f32 * tab_w));
    let (scroll, max_scroll) = strip_scroll(tab_w * tab_count as f32, available, scroll, ensure);
    let rects = horizontal_tab_rects_with_widths(
        tab_count,
        tab_start,
        tab_limit,
        bar.y,
        bar.h,
        px(layout, TAB_W),
        px(layout, MIN_TAB_W),
        scroll,
    );
    let tab_end = rects
        .last()
        .map(|rect| rect.x + rect.w)
        .unwrap_or(tab_start);
    let new_tab = Rect {
        x: tab_end.min(new_tab_limit).max(bar.x),
        y: bar.y,
        w: new_tab_w,
        h: bar.h,
    };
    let strip = Rect {
        x: tab_start,
        y: bar.y,
        w: available,
        h: bar.h,
    };

    HorizontalTabLayout {
        rects,
        new_tab,
        strip,
        scroll,
        max_scroll,
    }
}

fn custom_tab_drag_handle_w(layout: &Layout) -> f32 {
    if layout.custom_chrome {
        px(layout, CUSTOM_TAB_DRAG_HANDLE_W)
    } else {
        0.0
    }
}

fn horizontal_status_slot_w(layout: &Layout, ephemeral: bool) -> f32 {
    if layout.custom_chrome || ephemeral {
        px(layout, EPHEMERAL_W)
    } else {
        0.0
    }
}

fn horizontal_drag_region_rect(layout: &Layout, new_tab: Rect, settings: Rect) -> Option<Rect> {
    layout.titlebar?;
    let x = new_tab.x + new_tab.w;
    let w = settings.x - x;
    (w > 0.0).then_some(Rect {
        x,
        y: new_tab.y,
        w,
        h: new_tab.h,
    })
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

pub fn draw_window_backfill(
    r: &mut Renderer,
    layout: &Layout,
    colors: &ChromeColors,
    w: f32,
    h: f32,
) {
    if !layout.custom_chrome {
        return;
    }

    let pane = terminal_pane(layout);
    let edge = px(layout, CUSTOM_EDGE_INSET);
    let right = (w - edge).max(edge);
    let bottom = (h - edge).max(edge);
    let fill = colors.active_bg;

    r.fill_rect(
        edge,
        edge,
        (right - edge).max(0.0),
        (pane.y - edge).max(0.0),
        fill,
    );
    r.fill_rect(
        edge,
        pane.y,
        (pane.x - edge).max(0.0),
        pane.h.max(0.0),
        fill,
    );
    r.fill_rect(
        pane.x + pane.w,
        pane.y,
        (right - pane.x - pane.w).max(0.0),
        pane.h.max(0.0),
        fill,
    );
    r.fill_rect(
        edge,
        pane.y + pane.h,
        (right - edge).max(0.0),
        (bottom - pane.y - pane.h).max(0.0),
        fill,
    );

    if cfg!(target_os = "linux") {
        let radius = 14.0 * layout.scale_factor;
        draw_rounded_corner_fill(r, edge, edge, radius, fill, Corner::TopLeft);
        draw_rounded_corner_fill(r, right - radius, edge, radius, fill, Corner::TopRight);
        draw_rounded_corner_fill(r, edge, bottom - radius, radius, fill, Corner::BottomLeft);
        draw_rounded_corner_fill(
            r,
            right - radius,
            bottom - radius,
            radius,
            fill,
            Corner::BottomRight,
        );
    }
}

fn draw_chrome_surfaces(r: &mut Renderer, layout: &Layout, colors: &ChromeColors) {
    if let Some(titlebar) = layout.titlebar {
        draw_titlebar_gradient(r, titlebar, colors);
        if cfg!(target_os = "linux") {
            draw_linux_border(r, layout, colors);
        }
    }

    if layout.horizontal {
        if layout.titlebar.is_none() {
            r.fill_rect(
                layout.bar.x,
                layout.bar.y,
                layout.bar.w,
                layout.bar.h,
                colors.bar_bg,
            );
        }
    } else {
        r.fill_rect(
            layout.bar.x,
            layout.bar.y,
            layout.bar.w,
            layout.bar.h,
            colors.bar_bg,
        );
    }
}

fn draw_linux_border(r: &mut Renderer, layout: &Layout, colors: &ChromeColors) {
    let Some(titlebar) = layout.titlebar else {
        return;
    };
    let x = px(layout, CUSTOM_EDGE_INSET);
    let y = px(layout, CUSTOM_EDGE_INSET);
    let w = titlebar.w;
    let border_w = px(layout, WINDOW_BORDER_W);
    let h = if layout.horizontal {
        layout.bar.h + layout.viewport.h + px(layout, TERMINAL_MARGIN) * 2.0
    } else {
        titlebar.h + layout.bar.h
    };
    let border = with_alpha(mix(colors.text, colors.accent, 0.5), 90);
    r.fill_rect(x, y, w, border_w, border);
    r.fill_rect(x, y, border_w, h.max(0.0), border);
    r.fill_rect(x + w - border_w, y, border_w, h.max(0.0), border);
    r.fill_rect(x, y + h - border_w, w, border_w, with_alpha(border, 54));
}

fn draw_titlebar_gradient(r: &mut Renderer, rect: Rect, colors: &ChromeColors) {
    let glow = mix(colors.bar_bg, colors.accent, 0.42);
    let top_left = mix(colors.bar_bg, colors.accent, 0.14);
    let top_right = mix(glow, colors.text, 0.05);
    let bottom_left = shade(mix(colors.bar_bg, colors.accent, 0.08), 0.9);
    let bottom_right = shade(mix(colors.bar_bg, colors.accent, 0.28), 0.92);
    r.fill_rect_gradient(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        [top_left, top_right, bottom_left, bottom_right],
    );
}

#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

fn draw_rounded_corner_fill(
    r: &mut Renderer,
    x: f32,
    y: f32,
    radius: f32,
    color: [u8; 4],
    corner: Corner,
) {
    let rows = radius.ceil() as usize;
    for row in 0..rows {
        let dy = row as f32 + 0.5;
        let chord = (radius * radius - (radius - dy).powi(2)).sqrt();
        let inset = radius - chord;
        let row_y = match corner {
            Corner::TopLeft | Corner::TopRight => y + row as f32,
            Corner::BottomLeft | Corner::BottomRight => y + radius - row as f32 - 1.0,
        };
        match corner {
            Corner::TopLeft | Corner::BottomLeft => {
                r.fill_rect(x + inset, row_y, radius - inset, 1.0, color);
            }
            Corner::TopRight | Corner::BottomRight => {
                r.fill_rect(x, row_y, radius - inset, 1.0, color);
            }
        }
    }
}

fn custom_left_reserved(layout: &Layout) -> f32 {
    if layout.custom_chrome && cfg!(target_os = "macos") {
        px(layout, MAC_TRAFFIC_LIGHT_SPACE)
    } else {
        0.0
    }
}

fn custom_right_reserved(layout: &Layout) -> f32 {
    if layout.custom_chrome && !cfg!(target_os = "macos") {
        px(layout, LINUX_WINDOW_BUTTON_W) * 3.0
    } else {
        0.0
    }
}

fn titlebar_settings_rect(titlebar: Rect, layout: &Layout) -> Rect {
    let settings_w = px(layout, SETTINGS_W);
    Rect {
        x: titlebar.x + (titlebar.w - custom_right_reserved(layout) - settings_w).max(0.0),
        y: titlebar.y,
        w: settings_w,
        h: titlebar.h,
    }
}

fn ephemeral_indicator_rect_for_layout(layout: &Layout, settings: Rect) -> Rect {
    let ephemeral_w = px(layout, EPHEMERAL_W);
    Rect {
        x: (settings.x - ephemeral_w).max(0.0),
        y: settings.y,
        w: ephemeral_w,
        h: settings.h,
    }
}

fn window_control_hits(layout: &Layout) -> Vec<WindowControlHit> {
    if !layout.custom_chrome || cfg!(target_os = "macos") {
        return Vec::new();
    }
    let Some(titlebar) = layout.titlebar else {
        return Vec::new();
    };
    let button_w = px(layout, LINUX_WINDOW_BUTTON_W);
    let mut x = titlebar.x + titlebar.w - button_w * 3.0;
    [
        WindowControl::Minimize,
        WindowControl::Maximize,
        WindowControl::Close,
    ]
    .into_iter()
    .map(|control| {
        let rect = Rect {
            x,
            y: titlebar.y,
            w: button_w,
            h: titlebar.h,
        };
        x += button_w;
        WindowControlHit { control, rect }
    })
    .collect()
}

fn draw_titlebar_controls(
    r: &mut Renderer,
    queue: &wgpu::Queue,
    titlebar: Rect,
    layout: &Layout,
    colors: &ChromeColors,
    state: TitlebarControlState,
) {
    let settings = titlebar_settings_rect(titlebar, layout);
    if state.ephemeral {
        draw_ephemeral_indicator(
            r,
            ephemeral_indicator_rect_for_layout(layout, settings),
            colors,
        );
    }
    draw_settings_button(
        r,
        settings,
        colors,
        state.settings_open,
        state.settings_hover,
    );
    if !cfg!(target_os = "macos") {
        return;
    }
    let text = "Phantom Terminal";
    let x = titlebar.x + px(layout, MAC_TRAFFIC_LIGHT_SPACE);
    let y = titlebar.y + (titlebar.h - r.cell_size().1) * 0.5;
    r.text(queue, x, y, text, with_alpha(colors.dim_text, 160));
}

fn draw_new_tab_button(
    r: &mut Renderer,
    queue: &wgpu::Queue,
    rect: Rect,
    colors: &ChromeColors,
    hover: f32,
) {
    draw_chrome_hover_fill(r, rect, colors, hover);
    r.text(
        queue,
        rect.x + (rect.w - r.cell_size().0) * 0.5,
        rect.y + (rect.h - r.cell_size().1) * 0.5,
        "+",
        colors.text,
    );
}

fn draw_window_controls(
    r: &mut Renderer,
    queue: &wgpu::Queue,
    hits: &[WindowControlHit],
    colors: &ChromeColors,
    animations: &ChromeAnimationState,
) {
    for hit in hits {
        let hover = animations.window_control(hit.control).clamp(0.0, 1.0);
        if hover > 0.0 {
            let fill = match hit.control {
                WindowControl::Close => [239, 68, 68, (76.0 * hover) as u8],
                WindowControl::Minimize | WindowControl::Maximize => {
                    with_alpha(colors.text, (22.0 * hover) as u8)
                }
            };
            r.fill_rect(hit.rect.x, hit.rect.y, hit.rect.w, hit.rect.h, fill);
        }
        let (glyph, color) = match hit.control {
            WindowControl::Close => (
                "\u{00d7}",
                mix(with_alpha(colors.text, 210), colors.text, hover),
            ),
            WindowControl::Minimize => ("\u{2212}", with_alpha(colors.dim_text, 210)),
            WindowControl::Maximize => ("\u{25a1}", with_alpha(colors.dim_text, 210)),
        };
        r.text(
            queue,
            hit.rect.x + (hit.rect.w - r.cell_size().0) * 0.5,
            hit.rect.y + (hit.rect.h - r.cell_size().1) * 0.5,
            glyph,
            color,
        );
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
    draw_chrome_hover_fill(r, rect, colors, hover);
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
    let icon_size = (rect.w.min(rect.h) * 0.58).max(16.0);
    let base_icon = if active { colors.text } else { colors.dim_text };
    let color = mix(base_icon, colors.accent, hover);
    draw_settings_icon(
        r,
        rect.x + (rect.w - icon_size) * 0.5,
        rect.y + (rect.h - icon_size) * 0.5,
        icon_size,
        icon_size,
        color,
    );
}

fn draw_chrome_hover_fill(r: &mut Renderer, rect: Rect, colors: &ChromeColors, hover: f32) {
    let hover = hover.clamp(0.0, 1.0);
    if hover > 0.0 {
        r.fill_rect(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            with_alpha(colors.text, (22.0 * hover) as u8),
        );
    }
}

fn draw_ephemeral_indicator(r: &mut Renderer, rect: Rect, colors: &ChromeColors) {
    let accent = [251, 191, 36, 255];
    let scale = (rect.h / CUSTOM_TITLEBAR_H).max(1.0);
    let size = 10.0 * scale;
    let line = 2.0 * scale;
    let x = rect.x + (rect.w - size) * 0.5;
    let y = rect.y + (rect.h - size) * 0.5;
    r.fill_rect(
        x - 3.0 * scale,
        y - 3.0 * scale,
        size + 6.0 * scale,
        size + 6.0 * scale,
        with_alpha(accent, 28),
    );
    r.fill_rect(x, y, size, line, accent);
    r.fill_rect(x, y + size - line, size, line, accent);
    r.fill_rect(x, y, line, size, accent);
    r.fill_rect(x + size - line, y, line, size, accent);
    r.fill_rect(
        x + 4.0 * scale,
        y + 4.0 * scale,
        line,
        line,
        mix(accent, colors.text, 0.35),
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
    scale_factor: f32,
    underline_accent: bool,
    hover: f32,
    dragging: bool,
    editing: Option<&str>,
) {
    let hover = hover.clamp(0.0, 1.0);
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
        let underline_h = 2.0 * scale_factor;
        r.fill_rect(
            rect.x,
            rect.y + rect.h - underline_h,
            rect.w,
            underline_h,
            colors.accent,
        );
    } else if active {
        r.fill_rect(rect.x, rect.y, 2.0 * scale_factor, rect.h, colors.accent);
    }
    let pad = PAD * scale_factor;
    let close_w = CLOSE_W * scale_factor;
    let max_chars = (((rect.w - pad * 2.0 - close_w) / cell_w).floor() as usize).max(1);
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
    r.text(queue, rect.x + pad, rect.y + pad, &label, text_color);
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
            titlebar: None,
            window_controls: Vec::new(),
            tab_strip: Rect::EMPTY,
            tab_scroll: 0.0,
            max_tab_scroll: 0.0,
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
            titlebar: None,
            window_controls: Vec::new(),
            tab_strip: Rect::EMPTY,
            tab_scroll: 0.0,
            max_tab_scroll: 0.0,
        }
    }

    #[test]
    fn horizontal_layout_splits_off_top_bar() {
        let l = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::System);
        assert_eq!(l.bar.h, 36.0); // 20 + 8*2
        assert_eq!(l.viewport.x, 8.0);
        assert_eq!(l.viewport.y, 44.0);
        assert_eq!(l.viewport.h, 548.0);
        assert_eq!(l.viewport.w, 784.0);
    }

    #[test]
    fn vertical_layout_splits_off_left_bar() {
        let l = compute_layout(1000.0, 600.0, 20.0, false, WindowChrome::System);
        assert!(l.bar.w >= 120.0 && l.bar.w <= 240.0);
        assert_eq!(l.viewport.x, l.bar.w + 8.0);
        assert_eq!(l.viewport.y, 8.0);
        assert_eq!(l.viewport.w, 1000.0 - l.bar.w - 16.0);
        assert_eq!(l.viewport.h, 584.0);
    }

    #[test]
    fn vertical_tab_bar_keeps_default_width_when_there_is_room() {
        let wide = compute_layout(1000.0, 600.0, 20.0, false, WindowChrome::System);
        let wider = compute_layout(1200.0, 600.0, 20.0, false, WindowChrome::System);

        assert_eq!(wide.bar.w, VERTICAL_BAR_W);
        assert_eq!(wider.bar.w, VERTICAL_BAR_W);
    }

    #[test]
    fn vertical_tab_bar_shrinks_only_when_crowded() {
        let l = compute_layout(280.0, 600.0, 20.0, false, WindowChrome::System);

        assert!(l.bar.w < VERTICAL_BAR_W);
        assert_eq!(l.viewport.w, MIN_VERTICAL_VIEWPORT_W);
    }

    #[test]
    fn custom_window_resize_handles_cover_edges_and_corners() {
        use WindowResizeHandle::*;

        let hit =
            |x, y| window_resize_handle_at(800.0, 600.0, 1.0, WindowChrome::Custom, None, x, y);
        assert_eq!(hit(400.0, 2.0), Some(North));
        assert_eq!(hit(797.0, 300.0), Some(East));
        assert_eq!(hit(400.0, 597.0), Some(South));
        assert_eq!(hit(2.0, 300.0), Some(West));
        assert_eq!(hit(2.0, 8.0), Some(NorthWest));
        assert_eq!(hit(792.0, 2.0), Some(NorthEast));
        assert_eq!(hit(2.0, 592.0), Some(SouthWest));
        assert_eq!(hit(797.0, 592.0), Some(SouthEast));
        assert_eq!(hit(400.0, 300.0), None);
    }

    #[test]
    fn window_resize_handles_are_custom_only_and_dpi_scaled() {
        assert_eq!(
            window_resize_handle_at(800.0, 600.0, 1.0, WindowChrome::System, None, 2.0, 300.0,),
            None
        );
        assert_eq!(
            window_resize_handle_at(
                1600.0,
                1200.0,
                2.0,
                WindowChrome::Custom,
                None,
                1591.0,
                600.0,
            ),
            Some(WindowResizeHandle::East)
        );
        assert_eq!(
            window_resize_handle_at(
                1600.0,
                1200.0,
                2.0,
                WindowChrome::Custom,
                None,
                1589.0,
                600.0,
            ),
            None
        );
    }

    #[test]
    fn terminal_scrollbar_corridor_keeps_priority_over_right_resize_handle() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::Custom);
        let scrollbar = terminal_scrollbar_hit_track(&layout);
        let scrollbar_x = scrollbar.x + scrollbar.w - 1.0;
        let scrollbar_y = scrollbar.y + scrollbar.h / 2.0;

        assert!(scrollbar.contains(scrollbar_x, scrollbar_y));
        assert_eq!(
            window_resize_handle_at(
                800.0,
                600.0,
                1.0,
                WindowChrome::Custom,
                Some(scrollbar),
                scrollbar_x,
                scrollbar_y,
            ),
            None
        );
        assert_eq!(
            window_resize_handle_at(
                800.0,
                600.0,
                1.0,
                WindowChrome::Custom,
                Some(scrollbar),
                797.0,
                scrollbar_y,
            ),
            Some(WindowResizeHandle::East)
        );
    }

    #[test]
    fn horizontal_terminal_pane_wraps_viewport_inset_below_tab_bar() {
        let l = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::System);
        let pane = terminal_pane(&l);

        assert_eq!(pane.x, 0.0);
        assert_eq!(pane.y, l.bar.h);
        assert_eq!(pane.w, 800.0);
        assert_eq!(pane.h, 600.0 - l.bar.h);
    }

    #[test]
    fn right_sidebar_reserves_only_terminal_content_width() {
        let base = compute_layout(1000.0, 600.0, 20.0, true, WindowChrome::System);
        let reserved = reserve_right_sidebar(base, 260.0);
        let base_pane = terminal_pane(&base);
        let reserved_pane = terminal_pane(&reserved);

        assert_eq!(reserved.viewport.w, base.viewport.w - 260.0);
        assert_eq!(reserved.bar, base.bar);
        assert_eq!(reserved.titlebar, base.titlebar);
        assert_eq!(reserved.viewport.x, base.viewport.x);
        assert_eq!(base_pane.w, base.bar.w);
        assert_eq!(reserved_pane.w, base_pane.w - 260.0);
        assert_eq!(reserved_pane.x, base_pane.x);
        assert_eq!(
            reserved_pane.x + reserved_pane.w,
            base_pane.x + base_pane.w - 260.0
        );
        let scrollbar = terminal_scrollbar_track(&reserved);
        assert_eq!(
            scrollbar.x + scrollbar.w,
            reserved_pane.x + reserved_pane.w - px(&reserved, SCROLLBAR_INSET)
        );
    }

    #[test]
    fn vertical_terminal_pane_wraps_viewport_inset_beside_tab_bar() {
        let l = compute_layout(1000.0, 600.0, 20.0, false, WindowChrome::System);
        let pane = terminal_pane(&l);

        assert_eq!(pane.x, l.bar.w);
        assert_eq!(pane.y, 0.0);
        assert_eq!(pane.w, 1000.0 - l.bar.w);
        assert_eq!(pane.h, 600.0);
    }

    #[test]
    fn custom_horizontal_layout_uses_titlebar_for_tabs() {
        let l = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::Custom);

        assert!(l.custom_chrome);
        assert_eq!(l.titlebar, Some(l.bar));
        assert!(l.bar.h >= CUSTOM_TITLEBAR_H);
        assert!(l.viewport.y > l.bar.y + l.bar.h);
    }

    #[test]
    fn custom_horizontal_layout_scales_titlebar_controls() {
        let scale = 2.0;
        let layout = compute_layout_scaled(1600.0, 1200.0, 40.0, true, WindowChrome::Custom, scale);
        let titlebar = layout.titlebar.expect("custom titlebar");
        let settings = titlebar_settings_rect(titlebar, &layout);

        assert!(layout.bar.h >= CUSTOM_TITLEBAR_H * scale);
        assert_eq!(settings.w, SETTINGS_W * scale);
        assert_eq!(settings.h, titlebar.h);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scaled_custom_horizontal_layout_reserves_macos_traffic_light_space() {
        let scale = 2.0;
        let layout = compute_layout_scaled(1600.0, 1200.0, 40.0, true, WindowChrome::Custom, scale);
        let settings = titlebar_settings_rect(layout.titlebar.unwrap(), &layout);
        let strip = horizontal_tab_layout(&layout, settings, 1, false, 0.0, None);

        assert!(strip.rects[0].x >= MAC_TRAFFIC_LIGHT_SPACE * scale);
    }

    #[test]
    fn custom_vertical_layout_keeps_tabs_below_titlebar() {
        let l = compute_layout(1000.0, 600.0, 20.0, false, WindowChrome::Custom);
        let titlebar = l.titlebar.expect("custom titlebar");

        assert!(l.custom_chrome);
        assert!(l.bar.y >= titlebar.y + titlebar.h);
        assert!(l.viewport.y >= titlebar.y + titlebar.h);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn custom_titlebar_exposes_window_controls_before_drag_region() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::Custom);
        let controls = window_control_hits(&layout);

        assert_eq!(controls.len(), 3);
        let close = controls
            .iter()
            .find(|hit| hit.control == WindowControl::Close)
            .expect("close hit");
        let close_probe_x = close.rect.x + 1.0;
        let close_probe_y = close.rect.y + 1.0;
        let hits = TabBarHits {
            tabs: Vec::new(),
            new_tab: Rect {
                x: 120.0,
                y: 1.0,
                w: 30.0,
                h: layout.bar.h,
            },
            settings: titlebar_settings_rect(layout.titlebar.unwrap(), &layout),
            titlebar: layout.titlebar,
            window_controls: controls,
            tab_strip: Rect::EMPTY,
            tab_scroll: 0.0,
            max_tab_scroll: 0.0,
        };

        assert_eq!(
            hits.window_control_at(close_probe_x, close_probe_y),
            Some(WindowControl::Close)
        );
        assert!(!hits.titlebar_drag_region_contains(close_probe_x, close_probe_y));
    }

    #[test]
    fn ephemeral_indicator_sits_next_to_settings_and_is_drag_region() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::Custom);
        let settings = titlebar_settings_rect(layout.titlebar.unwrap(), &layout);
        let indicator = ephemeral_indicator_rect_for_layout(&layout, settings);
        let hits = TabBarHits {
            tabs: Vec::new(),
            new_tab: Rect {
                x: 120.0,
                y: layout.bar.y,
                w: 30.0,
                h: layout.bar.h,
            },
            settings,
            titlebar: layout.titlebar,
            window_controls: window_control_hits(&layout),
            tab_strip: Rect::EMPTY,
            tab_scroll: 0.0,
            max_tab_scroll: 0.0,
        };

        assert_eq!(indicator.x + indicator.w, settings.x);
        assert!(hits.titlebar_drag_region_contains(indicator.x + 1.0, indicator.y + 1.0));
    }

    #[test]
    fn horizontal_drag_hint_fills_only_gap_between_new_tab_and_settings() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::Custom);
        let settings = titlebar_settings_rect(layout.titlebar.unwrap(), &layout);
        let strip = horizontal_tab_layout(&layout, settings, 1, false, 0.0, None);
        let hint = horizontal_drag_region_rect(&layout, strip.new_tab, settings)
            .expect("custom titlebar has a drag hint");

        assert_eq!(hint.x, strip.new_tab.x + strip.new_tab.w);
        assert_eq!(hint.x + hint.w, settings.x);
        assert_eq!(hint.y, layout.bar.y);
        assert_eq!(hint.h, layout.bar.h);

        let system = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::System);
        assert!(horizontal_drag_region_rect(&system, strip.new_tab, settings).is_none());
    }

    #[test]
    fn custom_titlebar_excludes_tab_body_from_drag_region() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::Custom);
        let tab = tab_hit(0, 96.0, layout.bar.y);
        let tab_body_x = tab.rect.x + 12.0;
        let tab_body_y = tab.rect.y + 12.0;
        let hits = TabBarHits {
            tabs: vec![tab],
            new_tab: Rect {
                x: 220.0,
                y: layout.bar.y,
                w: 30.0,
                h: layout.bar.h,
            },
            settings: titlebar_settings_rect(layout.titlebar.unwrap(), &layout),
            titlebar: layout.titlebar,
            window_controls: window_control_hits(&layout),
            tab_strip: Rect::EMPTY,
            tab_scroll: 0.0,
            max_tab_scroll: 0.0,
        };

        assert_eq!(hits.tab_body_at(tab_body_x, tab_body_y), Some(0));
        assert!(!hits.titlebar_drag_region_contains(tab_body_x, tab_body_y));
    }

    #[test]
    fn custom_horizontal_tabs_use_status_slot_drag_handle_when_crowded() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::Custom);
        let settings = titlebar_settings_rect(layout.titlebar.unwrap(), &layout);
        let normal_strip = horizontal_tab_layout(&layout, settings, 50, false, 0.0, None);
        let ephemeral_strip = horizontal_tab_layout(&layout, settings, 50, true, 0.0, None);
        assert_eq!(normal_strip.new_tab, ephemeral_strip.new_tab);

        let strip = normal_strip;
        let first_tab = strip.rects.first().copied().expect("visible tab");
        // Mirror production: only tabs intersecting the strip become hits.
        let tabs = strip
            .rects
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, rect)| {
                rect.intersect(strip.strip).map(|visible| TabHit {
                    index,
                    rect: visible,
                    close: Rect {
                        x: rect.x + rect.w - CLOSE_W - PAD * 0.5,
                        y: rect.y,
                        w: CLOSE_W,
                        h: rect.h,
                    }
                    .intersect(strip.strip)
                    .unwrap_or(Rect::EMPTY),
                })
            })
            .collect();
        let hits = TabBarHits {
            tabs,
            new_tab: strip.new_tab,
            settings,
            titlebar: layout.titlebar,
            window_controls: window_control_hits(&layout),
            tab_strip: strip.strip,
            tab_scroll: strip.scroll,
            max_tab_scroll: strip.max_scroll,
        };
        let y = layout.bar.y + layout.bar.h * 0.5;
        let left_handle_x = first_tab.x - CUSTOM_TAB_DRAG_HANDLE_W * 0.5;
        let status_slot_x = strip.new_tab.x + strip.new_tab.w + EPHEMERAL_W * 0.5;

        assert!(
            first_tab.x - layout.bar.x >= custom_left_reserved(&layout) + CUSTOM_TAB_DRAG_HANDLE_W
        );
        // 50 tabs overflow: the strip scrolls, tabs fill it edge to edge, and
        // the new-tab button is pinned at the strip's right end.
        assert!(strip.max_scroll > 0.0);
        assert_eq!(strip.new_tab.x, strip.strip.x + strip.strip.w);
        assert!(settings.x - (strip.new_tab.x + strip.new_tab.w) >= EPHEMERAL_W);
        assert!(hits.titlebar_drag_region_contains(left_handle_x, y));
        assert!(hits.titlebar_drag_region_contains(status_slot_x, y));
        assert!(!hits.titlebar_drag_region_contains(first_tab.x + 1.0, y));
        assert!(!hits.titlebar_drag_region_contains(strip.new_tab.x + 1.0, y));
    }

    #[test]
    fn crowded_horizontal_tabs_scroll_instead_of_disappearing() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::System);
        let settings = Rect {
            x: 760.0,
            y: layout.bar.y,
            w: 34.0,
            h: layout.bar.h,
        };

        let at_start = horizontal_tab_layout(&layout, settings, 40, false, 0.0, None);
        assert_eq!(at_start.rects.len(), 40, "every tab gets a rect");
        assert!(at_start.max_scroll > 0.0);
        assert_eq!(at_start.scroll, 0.0);
        assert_eq!(
            at_start.rects[0].w, MIN_TAB_W,
            "crowded tabs stop at min width"
        );

        // Scrolling shifts every rect left; an absurd offset clamps to max.
        let scrolled = horizontal_tab_layout(&layout, settings, 40, false, 1e9, None);
        assert_eq!(scrolled.scroll, scrolled.max_scroll);
        assert_eq!(
            scrolled.rects[0].x,
            at_start.rects[0].x - scrolled.max_scroll
        );

        // Scrolling the last tab into view places its right edge at the strip's.
        let into_view = horizontal_tab_layout(&layout, settings, 40, false, 0.0, Some(39));
        let last = into_view.rects[39];
        assert!((last.x + last.w - (into_view.strip.x + into_view.strip.w)).abs() < 0.5);
        assert_eq!(into_view.scroll, into_view.max_scroll);

        // And scrolling the first tab back pins the strip to the start.
        let back = horizontal_tab_layout(&layout, settings, 40, false, 1e9, Some(0));
        assert_eq!(back.scroll, 0.0);
    }

    #[test]
    fn strip_scroll_clamps_and_ensures_visibility() {
        // Content fits: never scrolls.
        assert_eq!(strip_scroll(100.0, 200.0, 50.0, None), (0.0, 0.0));
        // Clamped to max.
        assert_eq!(strip_scroll(300.0, 200.0, 500.0, None), (100.0, 100.0));
        // Ensure below the viewport scrolls forward just enough.
        assert_eq!(
            strip_scroll(300.0, 200.0, 0.0, Some((250.0, 280.0))),
            (80.0, 100.0)
        );
        // Ensure above the viewport scrolls back to the span start.
        assert_eq!(
            strip_scroll(300.0, 200.0, 90.0, Some((10.0, 40.0))),
            (10.0, 100.0)
        );
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
            1.0,
        )
        .unwrap();
        let at_top = scrollbar_thumb(
            track,
            ScrollState {
                offset: 100,
                history: 100,
                viewport_rows: 25,
            },
            1.0,
        )
        .unwrap();

        assert_eq!(at_bottom.y + at_bottom.h, track.y + track.h);
        assert_eq!(at_top.y, track.y);
        assert_eq!(at_bottom.h, at_top.h);
    }

    #[test]
    fn scrollbar_min_thumb_scales_with_dpi() {
        let track = Rect {
            x: 90.0,
            y: 0.0,
            w: 4.0,
            h: 1000.0,
        };
        // A tiny visible fraction forces the minimum-height clamp.
        let scroll = ScrollState {
            offset: 0,
            history: 1_000_000,
            viewport_rows: 1,
        };
        let at_1x = scrollbar_thumb(track, scroll, 1.0).unwrap();
        let at_2x = scrollbar_thumb(track, scroll, 2.0).unwrap();
        assert_eq!(at_1x.h, SCROLLBAR_MIN_THUMB_H);
        assert_eq!(at_2x.h, SCROLLBAR_MIN_THUMB_H * 2.0);
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
            1.0,
        )
        .is_none());
    }

    #[test]
    fn scrollbar_hit_track_is_wider_but_right_aligned() {
        let layout = compute_layout(800.0, 600.0, 20.0, true, WindowChrome::System);
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
            titlebar: None,
            window_controls: Vec::new(),
            tab_strip: Rect::EMPTY,
            tab_scroll: 0.0,
            max_tab_scroll: 0.0,
        };

        assert_eq!(hits.hover_target(10.0, 10.0), ChromeHoverTarget::Tab(0));
        assert_eq!(hits.hover_target(90.0, 10.0), ChromeHoverTarget::Close(0));
        assert_eq!(hits.hover_target(110.0, 10.0), ChromeHoverTarget::NewTab);
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
    fn horizontal_tabs_keep_default_width_when_there_is_room() {
        let tabs = horizontal_tab_rects(3, 80.0, 80.0 + TAB_W * 3.0 + 100.0, 0.0, 32.0);

        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[0].x, 80.0);
        assert_eq!(tabs[0].w, TAB_W);
        assert_eq!(tabs[1].x, 80.0 + TAB_W);
        assert_eq!(tabs[1].w, TAB_W);
        assert_eq!(tabs[2].x, 80.0 + TAB_W * 2.0);
        assert_eq!(tabs[2].w, TAB_W);
    }

    #[test]
    fn horizontal_tabs_shrink_only_when_crowded() {
        let tabs = horizontal_tab_rects(3, 80.0, 80.0 + 300.0, 0.0, 32.0);

        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[0].w, 100.0);
        assert_eq!(tabs[1].x, 180.0);
        assert_eq!(tabs[1].w, 100.0);
        assert_eq!(tabs[2].x, 280.0);
        assert_eq!(tabs[2].w, 100.0);
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
        assert!(state.set_hover(ChromeHoverTarget::NewTab));
        assert!(state.advance(now, 1));
        assert!(state.new_tab() > 0.0);

        assert!(state.set_hover(ChromeHoverTarget::Settings));
        assert!(state.advance(now + std::time::Duration::from_millis(16), 1));
        assert!(state.settings() > 0.0);

        assert!(state.set_hover(ChromeHoverTarget::WindowControl(WindowControl::Close)));
        assert!(state.advance(now + std::time::Duration::from_millis(32), 1));
        assert!(state.window_control(WindowControl::Close) > 0.0);

        assert!(state.set_hover(ChromeHoverTarget::None));
        assert!(!state.advance(now + std::time::Duration::from_millis(500), 1));
        assert_eq!(state.settings(), 0.0);
        assert_eq!(state.window_control(WindowControl::Close), 0.0);
    }

    #[test]
    fn chrome_hover_targets_report_clickability() {
        assert!(!ChromeHoverTarget::None.is_clickable());
        assert!(ChromeHoverTarget::Tab(0).is_clickable());
        assert!(ChromeHoverTarget::Close(0).is_clickable());
        assert!(ChromeHoverTarget::NewTab.is_clickable());
        assert!(ChromeHoverTarget::Settings.is_clickable());
        assert!(ChromeHoverTarget::WindowControl(WindowControl::Close).is_clickable());
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
