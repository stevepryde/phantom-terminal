//! Shared Phantom styling and native egui controls.

use std::hash::Hash;

use egui::style::WidgetVisuals;
use egui::{Color32, ComboBox, Context, Id, InnerResponse, Stroke, Ui};

pub const APP_BACKGROUND: Color32 = Color32::from_rgb(11, 11, 14);
pub const SIDEBAR_SURFACE: Color32 = Color32::from_rgb(17, 17, 22);
pub const ELEVATED_SURFACE: Color32 = Color32::from_rgb(26, 26, 32);
pub const DIVIDER_RGBA: [u8; 4] = [255, 255, 255, 26];
pub const DIVIDER: Color32 = Color32::from_rgba_unmultiplied_const(
    DIVIDER_RGBA[0],
    DIVIDER_RGBA[1],
    DIVIDER_RGBA[2],
    DIVIDER_RGBA[3],
);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 230);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 140);
pub const TEXT_MUTED: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 90);
pub const FOCUS_ACCENT: Color32 = Color32::from_rgba_unmultiplied_const(56, 189, 248, 153);

pub fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

pub fn configure_phantom_style(ctx: &Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.weak_text_color = Some(TEXT_SECONDARY);
    visuals.panel_fill = SIDEBAR_SURFACE;
    visuals.window_fill = ELEVATED_SURFACE;
    visuals.window_stroke = Stroke::new(1.0, DIVIDER);
    visuals.window_corner_radius = 4.into();
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 3],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(130),
    };
    visuals.menu_corner_radius = 4.into();
    visuals.extreme_bg_color = APP_BACKGROUND;
    visuals.text_edit_bg_color = Some(Color32::from_rgb(14, 14, 18));
    visuals.code_bg_color = Color32::from_rgb(14, 14, 18);
    visuals.faint_bg_color = Color32::from_rgba_unmultiplied(255, 255, 255, 10);
    visuals.selection.bg_fill = FOCUS_ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.65 };

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = 4.into();
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = APP_BACKGROUND;
    visuals.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, DIVIDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    visuals.widgets.inactive.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 13);
    visuals.widgets.inactive.weak_bg_fill = visuals.widgets.inactive.bg_fill;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, DIVIDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 24);
    visuals.widgets.hovered.weak_bg_fill = visuals.widgets.hovered.bg_fill;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, TEXT_MUTED);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.active.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 38);
    visuals.widgets.active.weak_bg_fill = visuals.widgets.active.bg_fill;
    visuals.widgets.active.bg_stroke = Stroke::new(1.5, FOCUS_ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    visuals.widgets.open = visuals.widgets.active;

    let mut style = (*ctx.global_style()).clone();
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 7.0);
    style.spacing.interact_size.y = 32.0;
    style.spacing.combo_width = 180.0;
    style.spacing.slider_width = 180.0;
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(16.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(11.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, egui::FontId::monospace(12.0));
    ctx.set_global_style(style);
}

/// Phantom's only dropdown primitive. It keeps the stock egui interaction and
/// accessibility model while enforcing the app's sizing, truncation, popup,
/// and chevron presentation.
pub struct CustomDropdown {
    id: Id,
    selected_text: String,
    width: f32,
    truncate: bool,
}

impl CustomDropdown {
    pub fn new(id_salt: impl Hash + std::fmt::Debug, selected_text: impl Into<String>) -> Self {
        Self {
            id: Id::new(id_salt),
            selected_text: selected_text.into(),
            width: 180.0,
            truncate: false,
        }
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }

    pub fn show_ui<R>(
        self,
        ui: &mut Ui,
        contents: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        let mut combo = ComboBox::from_id_salt(self.id)
            .selected_text(self.selected_text)
            .width(self.width)
            .icon(dropdown_chevron);
        if self.truncate {
            combo = combo.truncate();
        }
        combo.show_ui(ui, |ui| {
            ui.set_min_width(self.width);
            contents(ui)
        })
    }
}

fn dropdown_chevron(ui: &Ui, rect: egui::Rect, visuals: &WidgetVisuals, open: bool) {
    let center = rect.center();
    let points = if open {
        [
            egui::pos2(center.x - 4.0, center.y + 2.0),
            egui::pos2(center.x, center.y - 2.0),
            egui::pos2(center.x + 4.0, center.y + 2.0),
        ]
    } else {
        [
            egui::pos2(center.x - 4.0, center.y - 2.0),
            egui::pos2(center.x, center.y + 2.0),
            egui::pos2(center.x + 4.0, center.y - 2.0),
        ]
    };
    ui.painter()
        .line(points.to_vec(), Stroke::new(1.25, visuals.fg_stroke.color));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phantom_style_uses_compact_controls_and_visible_focus() {
        let ctx = Context::default();
        configure_phantom_style(&ctx);
        let style = ctx.global_style();

        assert_eq!(style.spacing.interact_size.y, 32.0);
        assert_eq!(style.visuals.widgets.inactive.corner_radius, 4.into());
        assert_eq!(style.visuals.widgets.active.bg_stroke.color, FOCUS_ACCENT);
        assert!(style.visuals.slider_trailing_fill);
        assert_eq!(style.visuals.menu_corner_radius, 4.into());
    }
}
