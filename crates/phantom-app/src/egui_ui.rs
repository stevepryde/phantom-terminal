//! egui control plane for the native app.
//!
//! `phantom-gfx` owns terminal rendering. egui owns non-terminal UI: settings,
//! contextual side panels, forms, sliders, colour controls, and future inspector
//! surfaces.

use egui::{Button, Color32, ComboBox, Context, Panel, RichText, Slider, TextEdit, Ui, ViewportId};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use phantom_core::{AppConfig, Theme};
use winit::event::WindowEvent;
use winit::window::Window;

use crate::gpu::FrameOverlay;
use crate::themes;

const PANEL_WIDTH_POINTS: f32 = 560.0;
const PANEL_MIN_WIDTH_POINTS: f32 = 460.0;
const PANEL_MAX_WIDTH_POINTS: f32 = 760.0;
const SETTINGS_NAV_WIDTH_POINTS: f32 = 168.0;
const BACKGROUNDS: &[&str] = &["none", "phantom", "dragon"];
const CURSOR_STYLES: &[&str] = &["block", "bar", "underline"];
const LAYOUTS: &[&str] = &["horizontal", "vertical"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelKind {
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Appearance,
    Terminal,
    Session,
    Colours,
}

impl SettingsTab {
    const ALL: [Self; 4] = [
        Self::Appearance,
        Self::Terminal,
        Self::Session,
        Self::Colours,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Terminal => "terminal",
            Self::Session => "session",
            Self::Colours => "colours",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Session => "Session",
            Self::Colours => "Colours",
        }
    }
}

pub struct UiState {
    active_panel: Option<PanelKind>,
    settings_tab: SettingsTab,
    panel_width_px: f32,
    font_family_edit: String,
    notice: Option<String>,
}

impl UiState {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            active_panel: None,
            settings_tab: SettingsTab::Appearance,
            panel_width_px: 0.0,
            font_family_edit: config.font_family.clone(),
            notice: None,
        }
    }

    pub fn open_settings(&mut self, config: &AppConfig) {
        self.active_panel = Some(PanelKind::Settings);
        self.font_family_edit = config.font_family.clone();
        self.notice = None;
    }

    pub fn toggle_settings(&mut self, config: &AppConfig) {
        if self.settings_open() {
            self.close_panel();
        } else {
            self.open_settings(config);
        }
    }

    pub fn close_panel(&mut self) {
        self.active_panel = None;
        self.panel_width_px = 0.0;
        self.notice = None;
    }

    pub fn settings_open(&self) -> bool {
        self.active_panel == Some(PanelKind::Settings)
    }

    pub fn panel_width_px(&self) -> f32 {
        if self.active_panel.is_some() {
            self.panel_width_px
        } else {
            0.0
        }
    }

    fn draw(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
            self.close_panel();
        }

        let mut changed = false;
        if self.active_panel == Some(PanelKind::Settings) {
            Panel::right("phantom_settings_panel")
                .default_size(PANEL_WIDTH_POINTS)
                .size_range(PANEL_MIN_WIDTH_POINTS..=PANEL_MAX_WIDTH_POINTS)
                .resizable(true)
                .frame(panel_frame())
                .show_inside(ui, |ui| {
                    self.panel_width_px = ui.max_rect().width() * ui.ctx().pixels_per_point();
                    changed |= self.settings_panel(ui, config);
                });
        } else {
            self.panel_width_px = 0.0;
        }
        changed
    }

    fn settings_panel(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.heading("Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.close_panel();
                }
            });
        });
        ui.add_space(8.0);
        if let Some(notice) = &self.notice {
            ui.colored_label(Color32::from_rgb(125, 211, 252), notice);
            ui.add_space(8.0);
        }

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(SETTINGS_NAV_WIDTH_POINTS, ui.available_height()),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| self.settings_nav(ui),
            );

            ui.separator();

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    changed |= self.settings_content(ui, config);
                },
            );
        });

        if changed {
            match config.validate() {
                Ok(()) => self.notice = None,
                Err(error) => self.notice = Some(error.to_string()),
            }
        }
        changed
    }

    fn settings_nav(&mut self, ui: &mut Ui) {
        ui.set_width(SETTINGS_NAV_WIDTH_POINTS);
        ui.add_space(4.0);
        for tab in SettingsTab::ALL {
            if settings_tab_button(ui, tab, self.settings_tab == tab).clicked() {
                self.settings_tab = tab;
            }
        }
    }

    fn settings_content(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        let mut changed = false;
        ui.heading(self.settings_tab.label());
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt(self.settings_tab.id())
            .auto_shrink([false, false])
            .show(ui, |ui| match self.settings_tab {
                SettingsTab::Appearance => {
                    section(ui, "Font");
                    changed |= self.font_family(ui, config);
                    changed |= slider_u16(ui, "Font size", &mut config.font_size, 8..=48);
                    changed |= slider_f32(ui, "Line height", &mut config.line_height, 1.0..=2.5);

                    section(ui, "Cursor");
                    changed |= combo(ui, "Cursor style", &mut config.cursor_style, CURSOR_STYLES);
                    changed |= ui
                        .checkbox(&mut config.cursor_blink, "Cursor blink")
                        .changed();

                    section(ui, "Backdrop");
                    changed |= combo(ui, "UI theme", &mut config.ui_theme, themes::UI_THEMES);
                    changed |= combo(
                        ui,
                        "Terminal background",
                        &mut config.terminal_background,
                        BACKGROUNDS,
                    );
                    ui.add_enabled_ui(config.terminal_background != "none", |ui| {
                        changed |= slider_u8(
                            ui,
                            "Background opacity",
                            &mut config.terminal_background_opacity,
                            0..=60,
                        );
                    });
                }
                SettingsTab::Terminal => {
                    section(ui, "Layout");
                    changed |= combo(ui, "Tab layout", &mut config.tab_layout, LAYOUTS);

                    section(ui, "History");
                    changed |= slider_u32(
                        ui,
                        "Scrollback lines",
                        &mut config.scrollback_lines,
                        0..=1_000_000,
                    );
                }
                SettingsTab::Session => {
                    section(ui, "Launch");
                    changed |= ui
                        .checkbox(&mut config.restore_on_launch, "Restore tabs on launch")
                        .changed();
                }
                SettingsTab::Colours => {
                    section(ui, "Theme Colours");
                    changed |= theme_colors(ui, &mut config.theme);
                }
            });
        changed
    }

    fn font_family(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        ui.label(label("Font family"));
        let changed = ui
            .add(TextEdit::singleline(&mut self.font_family_edit).desired_width(f32::INFINITY))
            .changed();
        if !changed {
            return false;
        }
        let mut next = config.clone();
        next.font_family = self.font_family_edit.trim().to_string();
        match next.validate() {
            Ok(()) => {
                config.font_family = next.font_family;
                self.notice = None;
                true
            }
            Err(error) => {
                self.notice = Some(error.to_string());
                false
            }
        }
    }
}

pub struct EguiLayer {
    ctx: Context,
    state: egui_winit::State,
    renderer: Renderer,
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_delta: egui::TexturesDelta,
    screen: ScreenDescriptor,
}

impl EguiLayer {
    pub fn new(window: &Window, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let ctx = Context::default();
        configure_style(&ctx);
        let state = egui_winit::State::new(
            ctx.clone(),
            ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let renderer = Renderer::new(device, format, RendererOptions::default());
        Self {
            ctx,
            state,
            renderer,
            paint_jobs: Vec::new(),
            textures_delta: egui::TexturesDelta::default(),
            screen: ScreenDescriptor {
                size_in_pixels: [1, 1],
                pixels_per_point: window.scale_factor() as f32,
            },
        }
    }

    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    pub fn run(&mut self, window: &Window, ui_state: &mut UiState, config: &mut AppConfig) -> bool {
        let raw_input = self.state.take_egui_input(window);
        let mut config_changed = false;
        let full_output = self.ctx.run_ui(raw_input, |ui| {
            config_changed = ui_state.draw(ui, config);
        });
        self.state
            .handle_platform_output(window, full_output.platform_output);
        let size = window.inner_size();
        self.screen = ScreenDescriptor {
            size_in_pixels: [size.width.max(1), size.height.max(1)],
            pixels_per_point: full_output.pixels_per_point,
        };
        self.paint_jobs = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        self.textures_delta.append(full_output.textures_delta);
        config_changed
    }

    fn prepare_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        for (id, image_delta) in &self.textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        let _ =
            self.renderer
                .update_buffers(device, queue, encoder, &self.paint_jobs, &self.screen);
    }

    fn paint_inner(&self, pass: &mut wgpu::RenderPass<'static>) {
        self.renderer.render(pass, &self.paint_jobs, &self.screen);
    }

    fn free_textures(&mut self) {
        for id in &self.textures_delta.free {
            self.renderer.free_texture(id);
        }
        self.textures_delta.clear();
    }
}

impl FrameOverlay for EguiLayer {
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        self.prepare_inner(device, queue, encoder);
    }

    fn paint(&mut self, pass: &mut wgpu::RenderPass<'static>) {
        self.paint_inner(pass);
    }

    fn after_submit(&mut self) {
        self.free_textures();
    }
}

fn configure_style(ctx: &Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(17, 17, 22);
    visuals.window_fill = Color32::from_rgb(17, 17, 22);
    visuals.extreme_bg_color = Color32::from_rgb(11, 11, 14);
    visuals.faint_bg_color = Color32::from_rgba_unmultiplied(255, 255, 255, 10);
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(56, 189, 248, 96);
    visuals.widgets.inactive.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 13);
    visuals.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 24);
    visuals.widgets.active.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 38);
    visuals.widgets.noninteractive.fg_stroke.color =
        Color32::from_rgba_unmultiplied(255, 255, 255, 150);

    let mut style = (*ctx.global_style()).clone();
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.slider_width = 160.0;
    ctx.set_global_style(style);
}

fn panel_frame() -> egui::Frame {
    egui::Frame::side_top_panel(&egui::Style::default())
        .fill(Color32::from_rgb(17, 17, 22))
        .stroke(egui::Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
        ))
        .inner_margin(egui::Margin::same(18))
}

fn settings_tab_button(ui: &mut Ui, tab: SettingsTab, selected: bool) -> egui::Response {
    let text = RichText::new(tab.label()).size(13.0);
    let button = Button::selectable(selected, text)
        .min_size(egui::vec2(SETTINGS_NAV_WIDTH_POINTS - 18.0, 34.0));
    ui.add(button)
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(16.0);
    ui.label(
        RichText::new(title.to_ascii_uppercase())
            .size(11.0)
            .color(Color32::from_rgba_unmultiplied(255, 255, 255, 140)),
    );
    ui.separator();
}

fn label(text: &str) -> RichText {
    RichText::new(text)
        .size(12.0)
        .color(Color32::from_rgba_unmultiplied(255, 255, 255, 150))
}

fn slider_u8(
    ui: &mut Ui,
    label_text: &str,
    value: &mut u8,
    range: std::ops::RangeInclusive<u8>,
) -> bool {
    ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true)).changed()
}

fn slider_u16(
    ui: &mut Ui,
    label_text: &str,
    value: &mut u16,
    range: std::ops::RangeInclusive<u16>,
) -> bool {
    ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true)).changed()
}

fn slider_u32(
    ui: &mut Ui,
    label_text: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
) -> bool {
    ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true)).changed()
}

fn slider_f32(
    ui: &mut Ui,
    label_text: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true)).changed()
}

fn combo(ui: &mut Ui, label_text: &str, value: &mut String, options: &[&str]) -> bool {
    ui.label(label(label_text));
    let mut changed = false;
    ComboBox::from_id_salt(label_text)
        .selected_text(display_name(value))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for option in options {
                changed |= ui
                    .selectable_value(value, (*option).to_string(), display_name(option))
                    .changed();
            }
        });
    changed
}

fn theme_colors(ui: &mut Ui, theme: &mut Theme) -> bool {
    let mut changed = false;
    changed |= color_row(ui, "Background", &mut theme.background, false);
    changed |= color_row(ui, "Foreground", &mut theme.foreground, false);
    changed |= color_row(ui, "Cursor", &mut theme.cursor, false);
    changed |= color_row(ui, "Selection", &mut theme.selection, true);
    changed |= color_row(ui, "Black", &mut theme.black, false);
    changed |= color_row(ui, "Red", &mut theme.red, false);
    changed |= color_row(ui, "Green", &mut theme.green, false);
    changed |= color_row(ui, "Yellow", &mut theme.yellow, false);
    changed |= color_row(ui, "Blue", &mut theme.blue, false);
    changed |= color_row(ui, "Magenta", &mut theme.magenta, false);
    changed |= color_row(ui, "Cyan", &mut theme.cyan, false);
    changed |= color_row(ui, "White", &mut theme.white, false);
    changed |= color_row(ui, "Bright black", &mut theme.bright_black, false);
    changed |= color_row(ui, "Bright red", &mut theme.bright_red, false);
    changed |= color_row(ui, "Bright green", &mut theme.bright_green, false);
    changed |= color_row(ui, "Bright yellow", &mut theme.bright_yellow, false);
    changed |= color_row(ui, "Bright blue", &mut theme.bright_blue, false);
    changed |= color_row(ui, "Bright magenta", &mut theme.bright_magenta, false);
    changed |= color_row(ui, "Bright cyan", &mut theme.bright_cyan, false);
    changed |= color_row(ui, "Bright white", &mut theme.bright_white, false);
    changed
}

fn color_row(ui: &mut Ui, name: &str, value: &mut String, alpha: bool) -> bool {
    let mut color = parse_color(value);
    let before = color;
    ui.horizontal(|ui| {
        ui.set_min_height(32.0);
        ui.label(label(name));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if alpha {
                ui.color_edit_button_srgba_unmultiplied(&mut color);
            } else {
                let mut rgb = [color[0], color[1], color[2]];
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    color = [rgb[0], rgb[1], rgb[2], 255];
                }
            }
            ui.monospace(value.as_str());
        });
    });
    if color != before {
        *value = format_color(color, alpha);
        true
    } else {
        false
    }
}

fn parse_color(value: &str) -> [u8; 4] {
    let hex = value.trim().trim_start_matches('#');
    let byte = |i| {
        hex.get(i..i + 2)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .unwrap_or(255)
    };
    let alpha = if hex.len() >= 8 { byte(6) } else { 255 };
    [byte(0), byte(2), byte(4), alpha]
}

fn format_color(color: [u8; 4], alpha: bool) -> String {
    let [red, green, blue, color_alpha] = color;
    if alpha {
        format!("#{red:02x}{green:02x}{blue:02x}{color_alpha:02x}")
    } else {
        format!("#{red:02x}{green:02x}{blue:02x}")
    }
}

fn display_name(value: &str) -> String {
    value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_round_trip_preserves_alpha_when_allowed() {
        let color = parse_color("#11223344");
        assert_eq!(format_color(color, true), "#11223344");
        assert_eq!(format_color(color, false), "#112233");
    }

    #[test]
    fn ui_state_tracks_settings_panel_width_only_when_open() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        state.panel_width_px = 320.0;
        assert_eq!(state.panel_width_px(), 0.0);

        state.open_settings(&config);
        state.panel_width_px = 320.0;
        assert_eq!(state.panel_width_px(), 320.0);

        state.close_panel();
        assert_eq!(state.panel_width_px(), 0.0);
    }

    #[test]
    fn ui_state_keeps_settings_tab_selection_when_reopened() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_tab = SettingsTab::Terminal;

        state.close_panel();
        state.open_settings(&config);

        assert_eq!(state.settings_tab, SettingsTab::Terminal);
    }
}
