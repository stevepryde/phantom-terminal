//! Ephemeral state and egui presentation for scrollback find.
//!
//! Matching and terminal-coordinate mapping stay outside this module. The app
//! feeds result summaries back after evaluating the current query and options.

use egui::scroll_area::ScrollBarVisibility;
use egui::{Area, Button, Color32, Frame, Id, Key, Margin, Order, ScrollArea, TextEdit, Ui};

use crate::ui_components::{with_alpha, DIVIDER, ELEVATED_SURFACE, FOCUS_ACCENT};

const BAR_INSET: f32 = 8.0;
const BAR_HEIGHT: f32 = 36.0;
const CONTROL_SIZE: f32 = 28.0;
const CONTROL_GAP: f32 = 4.0;
const IDEAL_QUERY_WIDTH: f32 = 220.0;
const MIN_QUERY_WIDTH: f32 = 72.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub selection_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FindError {
    InvalidRegex(String),
}

impl FindError {
    fn tooltip(&self) -> &str {
        match self {
            Self::InvalidRegex(message) => message,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FindResultSummary {
    pub match_count: usize,
    /// True when the search stopped at the emulator's match cap; the real
    /// total is "`match_count` or more" (a counter would show "10000+").
    pub capped: bool,
    pub active_match: Option<usize>,
    pub error: Option<FindError>,
}

impl FindResultSummary {
    fn navigation_available(&self, query: &str) -> bool {
        !query.is_empty() && self.error.is_none() && self.match_count > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FindNavigation {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FindUiOutcome {
    pub query_or_options_changed: bool,
    pub navigation: Option<FindNavigation>,
    pub close_requested: bool,
}

#[derive(Debug, Default)]
pub(crate) struct FindState {
    open: bool,
    query: String,
    options: FindOptions,
    results: FindResultSummary,
    selection_available: bool,
    selection_expired: bool,
    focus_requested: bool,
    select_query_requested: bool,
}

impl FindState {
    pub fn open(&mut self, selection_available: bool) {
        self.open = true;
        self.selection_available = selection_available;
        self.selection_expired = false;
        if !selection_available {
            self.options.selection_only = false;
        }
        self.results = FindResultSummary::default();
        self.request_focus();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.results = FindResultSummary::default();
        self.focus_requested = false;
        self.select_query_requested = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn request_focus(&mut self) {
        if self.open {
            self.focus_requested = true;
            self.select_query_requested = true;
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn options(&self) -> FindOptions {
        self.options
    }

    pub fn expire_selection(&mut self) {
        self.selection_available = false;
        self.selection_expired = true;
    }

    #[cfg(test)]
    pub fn configure_for_tests(&mut self, query: &str, selection_only: bool) {
        self.query = query.to_owned();
        self.options.selection_only = selection_only;
    }

    #[cfg(test)]
    pub fn selection_available(&self) -> bool {
        self.selection_available
    }

    pub fn set_results(&mut self, results: FindResultSummary) {
        self.results = results;
    }

    #[cfg(test)]
    pub fn results(&self) -> &FindResultSummary {
        &self.results
    }

    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        terminal_rect: egui::Rect,
        right_boundary: f32,
        alpha: u8,
    ) -> FindUiOutcome {
        if !self.open {
            return FindUiOutcome::default();
        }

        let layout = find_bar_layout(terminal_rect, right_boundary);
        let frame = Area::new(Id::new("phantom_scrollback_find"))
            .order(Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(layout.position)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(with_alpha(ELEVATED_SURFACE, alpha))
                    .stroke(egui::Stroke::new(1.0, DIVIDER))
                    .corner_radius(4)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 3],
                        blur: 14,
                        spread: 0,
                        color: Color32::from_black_alpha(100),
                    })
                    .inner_margin(Margin::same(BAR_INSET as i8))
                    .show(ui, |ui| {
                        let content_width = layout.width.shrink(BAR_INSET * 2.0);
                        ui.set_min_width(content_width);
                        ui.set_max_width(content_width);
                        ui.set_height(BAR_HEIGHT - BAR_INSET * 2.0);
                        ui.spacing_mut().item_spacing.x = CONTROL_GAP;
                        // Six 28px icon targets plus a useful text input cannot
                        // fit every arbitrarily narrow native window. Keep the
                        // surface clipped to the terminal pane and allow the
                        // single row to scroll horizontally at those extremes.
                        ScrollArea::horizontal()
                            .id_salt("phantom_scrollback_find_row")
                            .max_width(content_width)
                            .auto_shrink([false, true])
                            .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| self.draw_row(ui, layout.query_width))
                                    .inner
                            })
                            .inner
                    })
                    .inner
            });
        frame.inner
    }

    fn draw_row(&mut self, ui: &mut Ui, query_width: f32) -> FindUiOutcome {
        let mut outcome = FindUiOutcome::default();
        let invalid_tooltip = self
            .results
            .error
            .as_ref()
            .map(|error| error.tooltip().to_owned());
        let query_id = Id::new("phantom_scrollback_find_query");
        let query_focused = ui.memory(|memory| memory.focused() == Some(query_id));
        let input_frame = Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(egui::Stroke::new(
                1.0,
                if invalid_tooltip.is_some() {
                    Color32::from_rgb(248, 113, 113)
                } else if query_focused {
                    FOCUS_ACCENT
                } else {
                    ui.visuals().widgets.inactive.bg_stroke.color
                },
            ))
            .corner_radius(4)
            .inner_margin(Margin::symmetric(6, 0));
        let previous_query = self.query.clone();
        let edit = input_frame.show(ui, |ui| {
            ui.set_width(query_width);
            TextEdit::singleline(&mut self.query)
                .id(query_id)
                .hint_text("Find in scrollback")
                .desired_width(query_width)
                .min_size(egui::vec2(query_width, CONTROL_SIZE))
                .frame(Frame::NONE)
                .show(ui)
        });
        let mut edit = edit.inner;
        edit.response.widget_info(|| {
            let mut info = egui::WidgetInfo::text_edit(
                true,
                &previous_query,
                &self.query,
                "Find in scrollback",
            );
            info.label = Some("Find query".to_string());
            info
        });
        ui.ctx().accesskit_node_builder(edit.response.id, |node| {
            // This query is always editable. Clear stale disabled state from
            // another control's disabled scope before adding validation data.
            node.clear_disabled();
            if let Some(error) = invalid_tooltip.as_deref() {
                node.set_invalid(egui::accesskit::Invalid::True);
                node.set_description(error);
            }
        });
        if self.focus_requested {
            edit.response.request_focus();
            self.focus_requested = false;
        }
        if self.select_query_requested {
            let end = self.query.chars().count();
            edit.state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(end),
                )));
            edit.state.store(ui.ctx(), edit.response.id);
            self.select_query_requested = false;
        }
        if edit.response.changed() {
            self.results = FindResultSummary::default();
            outcome.query_or_options_changed = true;
        }
        if let Some(error) = invalid_tooltip {
            edit.response.response.clone().on_hover_text(error);
        }

        outcome.query_or_options_changed |= option_button(
            ui,
            &mut self.options.case_sensitive,
            OptionIcon::Case,
            "Case sensitive",
            true,
        );
        outcome.query_or_options_changed |= option_button(
            ui,
            &mut self.options.whole_word,
            OptionIcon::WholeWord,
            "Whole word",
            true,
        );
        outcome.query_or_options_changed |= option_button(
            ui,
            &mut self.options.regex,
            OptionIcon::Regex,
            "Regular expression",
            true,
        );
        let selection_tooltip = if self.selection_available {
            "Selection only"
        } else if self.selection_expired {
            "Selection only (captured selection expired after a terminal change)"
        } else {
            "Selection only (no selection was captured when Find opened)"
        };
        outcome.query_or_options_changed |= option_button(
            ui,
            &mut self.options.selection_only,
            OptionIcon::Selection,
            selection_tooltip,
            self.selection_available,
        );
        if outcome.query_or_options_changed {
            self.results = FindResultSummary::default();
        }

        let can_navigate = self.results.navigation_available(&self.query);
        if navigation_button(ui, FindNavigation::Previous, can_navigate) {
            outcome.navigation = Some(FindNavigation::Previous);
        }
        if navigation_button(ui, FindNavigation::Next, can_navigate) {
            outcome.navigation = Some(FindNavigation::Next);
        }

        let (escape, enter, shift) = ui.input(|input| {
            (
                input.key_pressed(Key::Escape),
                input.key_pressed(Key::Enter),
                input.modifiers.shift,
            )
        });
        if escape {
            self.close();
            outcome.close_requested = true;
        } else if enter && can_navigate && !outcome.query_or_options_changed {
            outcome.navigation = Some(if shift {
                FindNavigation::Previous
            } else {
                FindNavigation::Next
            });
        }
        outcome
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct FindBarLayout {
    position: egui::Pos2,
    width: f32,
    query_width: f32,
}

fn find_bar_layout(terminal_rect: egui::Rect, right_boundary: f32) -> FindBarLayout {
    let right = right_boundary
        .min(terminal_rect.right())
        .max(terminal_rect.left());
    let available_width = (right - terminal_rect.left() - BAR_INSET * 2.0).max(0.0);
    let fixed_width = CONTROL_SIZE * 6.0 + CONTROL_GAP * 6.0 + BAR_INSET * 2.0;
    let flexible_width = (available_width - fixed_width).max(0.0);
    let query_width = flexible_width.clamp(MIN_QUERY_WIDTH, IDEAL_QUERY_WIDTH);
    FindBarLayout {
        position: egui::pos2(right - BAR_INSET, terminal_rect.top() + BAR_INSET),
        width: (fixed_width + query_width).min(available_width),
        query_width,
    }
}

trait WidthExt {
    fn shrink(self, amount: f32) -> f32;
}

impl WidthExt for f32 {
    fn shrink(self, amount: f32) -> f32 {
        (self - amount).max(0.0)
    }
}

#[derive(Clone, Copy)]
enum OptionIcon {
    Case,
    WholeWord,
    Regex,
    Selection,
}

fn option_button(
    ui: &mut Ui,
    selected: &mut bool,
    icon: OptionIcon,
    tooltip: &str,
    enabled: bool,
) -> bool {
    let response = ui
        .add_enabled(
            enabled,
            Button::new("")
                .selected(*selected)
                .min_size(egui::vec2(CONTROL_SIZE, CONTROL_SIZE)),
        )
        .on_hover_text(tooltip);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, *selected, tooltip)
    });
    if enabled && response.clicked() {
        *selected = !*selected;
    }
    let visuals = ui.style().interact(&response);
    draw_option_icon(ui, response.rect, icon, visuals.fg_stroke.color);
    if *selected {
        ui.painter().hline(
            (response.rect.left() + 5.0)..=(response.rect.right() - 5.0),
            response.rect.bottom() - 3.0,
            egui::Stroke::new(2.0, visuals.fg_stroke.color),
        );
    }
    response.clicked()
}

fn draw_option_icon(ui: &Ui, rect: egui::Rect, icon: OptionIcon, color: Color32) {
    let painter = ui.painter();
    let center = rect.center();
    match icon {
        OptionIcon::Case => {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                "Aa",
                egui::FontId::proportional(12.0),
                color,
            );
        }
        OptionIcon::WholeWord => {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                "W",
                egui::FontId::proportional(12.0),
                color,
            );
            painter.hline(
                (center.x - 7.0)..=(center.x + 7.0),
                center.y + 7.0,
                egui::Stroke::new(1.0, color),
            );
        }
        OptionIcon::Regex => {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                ".*",
                egui::FontId::monospace(12.0),
                color,
            );
        }
        OptionIcon::Selection => {
            let min = center - egui::vec2(6.0, 5.0);
            let max = center + egui::vec2(6.0, 5.0);
            for (a, b) in [
                (min, egui::pos2(min.x + 4.0, min.y)),
                (min, egui::pos2(min.x, min.y + 4.0)),
                (max, egui::pos2(max.x - 4.0, max.y)),
                (max, egui::pos2(max.x, max.y - 4.0)),
            ] {
                painter.line_segment([a, b], egui::Stroke::new(1.2, color));
            }
        }
    }
}

fn navigation_button(ui: &mut Ui, direction: FindNavigation, enabled: bool) -> bool {
    let response = ui
        .add_enabled(
            enabled,
            Button::new("").min_size(egui::vec2(CONTROL_SIZE, CONTROL_SIZE)),
        )
        .on_hover_text(match direction {
            FindNavigation::Previous => "Previous match",
            FindNavigation::Next => "Next match",
        });
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            enabled,
            match direction {
                FindNavigation::Previous => "Previous match",
                FindNavigation::Next => "Next match",
            },
        )
    });
    let visuals = if enabled {
        ui.style().interact(&response)
    } else {
        &ui.visuals().widgets.noninteractive
    };
    let center = response.rect.center();
    let offset = match direction {
        FindNavigation::Previous => -2.0,
        FindNavigation::Next => 2.0,
    };
    let points = match direction {
        FindNavigation::Previous => [
            egui::pos2(center.x + 3.0, center.y - 5.0),
            egui::pos2(center.x - 3.0, center.y),
            egui::pos2(center.x + 3.0, center.y + 5.0),
        ],
        FindNavigation::Next => [
            egui::pos2(center.x - 3.0, center.y - 5.0),
            egui::pos2(center.x + 3.0, center.y),
            egui::pos2(center.x - 3.0, center.y + 5.0),
        ],
    };
    ui.painter().line_segment(
        [
            points[0] + egui::vec2(offset, 0.0),
            points[1] + egui::vec2(offset, 0.0),
        ],
        visuals.fg_stroke,
    );
    ui.painter().line_segment(
        [
            points[1] + egui::vec2(offset, 0.0),
            points[2] + egui::vec2(offset, 0.0),
        ],
        visuals.fg_stroke,
    );
    enabled && response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;

    #[test]
    fn reopening_preserves_ephemeral_query_and_options_but_resets_results() {
        let mut state = FindState {
            query: "needle".into(),
            ..FindState::default()
        };
        state.options.regex = true;
        state.set_results(FindResultSummary {
            match_count: 3,
            capped: false,
            active_match: Some(1),
            error: None,
        });

        state.close();
        state.open(false);

        assert_eq!(state.query(), "needle");
        assert!(state.options().regex);
        assert_eq!(state.results(), &FindResultSummary::default());
        assert!(!state.selection_available());
    }

    #[test]
    fn navigation_requires_a_nonempty_valid_query_with_matches() {
        let ready = FindResultSummary {
            match_count: 1,
            capped: false,
            active_match: Some(0),
            error: None,
        };
        assert!(ready.navigation_available("x"));
        assert!(!ready.navigation_available(""));
        assert!(!FindResultSummary::default().navigation_available("x"));
        assert!(!FindResultSummary {
            error: Some(FindError::InvalidRegex("invalid regex".into())),
            ..ready
        }
        .navigation_available("x"));
    }

    #[test]
    fn repeated_open_requests_focus_and_query_selection() {
        let mut state = FindState::default();
        state.open(true);
        state.focus_requested = false;
        state.select_query_requested = false;

        state.request_focus();

        assert!(state.focus_requested);
        assert!(state.select_query_requested);
    }

    #[test]
    fn reopening_without_a_selection_clears_selection_only_scope() {
        let mut state = FindState::default();
        state.open(true);
        state.options.selection_only = true;
        state.close();

        state.open(false);

        assert!(!state.options.selection_only);
        assert!(!state.selection_available);
    }

    #[test]
    fn bar_stays_left_of_context_boundary_and_below_terminal_top() {
        let terminal = egui::Rect::from_min_max(egui::pos2(144.0, 42.0), egui::pos2(1200.0, 800.0));
        let layout = find_bar_layout(terminal, 900.0);

        assert_eq!(layout.position, egui::pos2(892.0, 50.0));
        assert!(layout.width <= 900.0 - terminal.left() - BAR_INSET * 2.0);
        assert!(layout.position.x - layout.width >= terminal.left());
    }

    #[test]
    fn narrow_terminal_shrinks_query_before_crossing_left_edge() {
        let terminal = egui::Rect::from_min_max(egui::pos2(0.0, 32.0), egui::pos2(260.0, 500.0));
        let layout = find_bar_layout(terminal, terminal.right());

        assert!(layout.query_width < IDEAL_QUERY_WIDTH);
        assert!(layout.position.x - layout.width >= terminal.left());
    }

    #[test]
    fn rendered_extreme_narrow_bar_clips_scrollable_row_to_terminal() {
        let ctx = egui::Context::default();
        let terminal = egui::Rect::from_min_max(egui::pos2(20.0, 32.0), egui::pos2(160.0, 300.0));
        let mut state = FindState::default();
        state.open(false);
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 320.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                state.draw(ui.ctx(), terminal, terminal.right(), 220);
            },
        );

        assert!(!output.shapes.is_empty());
        for clipped in output.shapes {
            let painted = clipped
                .clip_rect
                .intersect(clipped.shape.visual_bounding_rect());
            if painted.is_positive() {
                assert!(painted.left() >= terminal.left() - 1.0);
                assert!(painted.right() <= terminal.right() + 1.0);
            }
        }
    }

    #[test]
    fn find_bar_exposes_query_and_disabled_selection_scope() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let terminal = egui::Rect::from_min_max(egui::pos2(20.0, 32.0), egui::pos2(1000.0, 700.0));
        let mut state = FindState::default();
        state.open(false);
        state.query = "[".to_string();
        state.set_results(FindResultSummary {
            error: Some(FindError::InvalidRegex(
                "unclosed character class".to_string(),
            )),
            ..Default::default()
        });

        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1024.0, 768.0),
                )),
                ..egui::RawInput::default()
            },
            |_ui| {
                state.draw(&ctx, terminal, terminal.right(), 220);
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();

        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::TextInput
                && node.label() == Some("Find query")
                && node.value() == Some("[")
                && !node.is_disabled()
                && node.invalid() == Some(egui::accesskit::Invalid::True)
                && node.description() == Some("unclosed character class")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::Button
                && node.label()
                    == Some("Selection only (no selection was captured when Find opened)")
                && node.is_disabled()
        }));
    }
}
