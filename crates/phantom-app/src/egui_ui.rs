//! egui control plane for the native app.
//!
//! `phantom-gfx` owns terminal rendering. egui owns non-terminal UI: settings,
//! contextual side panels, forms, sliders, colour controls, and future inspector
//! surfaces.

use egui::{
    Area, Button, Color32, ComboBox, Context, Frame, Id, Key as EguiKey, LayerId, Margin, Order,
    Panel, RichText, Slider, TextEdit, Ui, ViewportId,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use phantom_core::{AppConfig, Keybinding, Theme};
use phantom_gfx::available_terminal_font_families;
use winit::event::WindowEvent;
use winit::window::Window;

use crate::gpu::{BlurRegion, FrameOverlay};
use crate::keybindings::parse_combo;
use crate::palette::{PaletteAction, PaletteState};
use crate::themes;

const PANEL_WIDTH_POINTS: f32 = 560.0;
const PANEL_MIN_WIDTH_POINTS: f32 = 460.0;
const PANEL_MAX_WIDTH_POINTS: f32 = 760.0;
const SETTINGS_NAV_WIDTH_POINTS: f32 = 168.0;
const PALETTE_WIDTH_POINTS: f32 = 640.0;
const BACKGROUNDS: &[&str] = &["none", "phantom", "dragon"];
const CURSOR_STYLES: &[&str] = &["block", "bar", "underline"];
const LAYOUTS: &[&str] = &["horizontal", "vertical"];
const WINDOW_CHROME: &[&str] = &["system", "custom"];

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
    Keybindings,
}

impl SettingsTab {
    const ALL: [Self; 5] = [
        Self::Appearance,
        Self::Terminal,
        Self::Session,
        Self::Colours,
        Self::Keybindings,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Terminal => "terminal",
            Self::Session => "session",
            Self::Colours => "colours",
            Self::Keybindings => "keybindings",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Session => "Session",
            Self::Colours => "Colours",
            Self::Keybindings => "Keybindings",
        }
    }
}

#[derive(Default)]
pub struct UiOutcome {
    pub config_changed: bool,
    pub palette_action: Option<PaletteAction>,
}

pub struct UiState {
    active_panel: Option<PanelKind>,
    settings_tab: SettingsTab,
    panel_width_px: f32,
    font_families: Vec<String>,
    notice: Option<String>,
    /// Rects (egui points) of panels drawn translucent this frame, whose
    /// backdrop should be frosted. Rebuilt every `draw`.
    blur_regions: Vec<egui::Rect>,
}

impl UiState {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            active_panel: None,
            settings_tab: SettingsTab::Appearance,
            panel_width_px: 0.0,
            font_families: font_families_with_current(&config.font_family),
            notice: None,
            blur_regions: Vec::new(),
        }
    }

    fn blur_regions(&self) -> &[egui::Rect] {
        &self.blur_regions
    }

    pub fn open_settings(&mut self, config: &AppConfig) {
        self.active_panel = Some(PanelKind::Settings);
        self.font_families = font_families_with_current(&config.font_family);
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

    fn draw(
        &mut self,
        ui: &mut Ui,
        config: &mut AppConfig,
        palette: &mut PaletteState,
    ) -> UiOutcome {
        let mut outcome = UiOutcome::default();
        self.blur_regions.clear();
        let alpha = panel_alpha(config.panel_opacity);
        let transparent = config.panel_opacity < 100;
        if !palette.open && ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
            self.close_panel();
        }

        if self.active_panel == Some(PanelKind::Settings) {
            let response = Panel::right("phantom_settings_panel")
                .default_size(PANEL_WIDTH_POINTS)
                .size_range(PANEL_MIN_WIDTH_POINTS..=PANEL_MAX_WIDTH_POINTS)
                .resizable(true)
                .frame(panel_frame(alpha))
                .show_inside(ui, |ui| {
                    self.panel_width_px = ui.max_rect().width() * ui.ctx().pixels_per_point();
                    outcome.config_changed |= self.settings_panel(ui, config);
                });
            if transparent {
                self.blur_regions.push(response.response.rect);
            }
        } else {
            self.panel_width_px = 0.0;
        }
        let palette = command_palette_overlay(ui.ctx(), palette, alpha);
        outcome.palette_action = palette.action;
        if transparent {
            if let Some(rect) = palette.rect {
                self.blur_regions.push(rect);
            }
        }
        outcome
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
        // Anchor every tab's widgets under a distinct parent id. Each tab fills
        // the same on-screen rects, so without a per-tab parent egui sees the
        // widget at a given rect "change id" when tabs swap and, in debug builds,
        // outlines the affected widgets in red for a frame (`warn_if_rect_changes_id`).
        ui.push_id(self.settings_tab.id(), |ui| {
            ui.heading(self.settings_tab.label());
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt(self.settings_tab.id())
                .auto_shrink([false, false])
                .show(ui, |ui| match self.settings_tab {
                    SettingsTab::Appearance => {
                        section(ui, "Font");
                        changed |= self.font_family_selector(ui, config);
                        changed |= slider_u16(ui, "Font size", &mut config.font_size, 8..=48);
                        changed |=
                            slider_f32(ui, "Line height", &mut config.line_height, 1.0..=2.5);

                        section(ui, "Cursor");
                        changed |=
                            combo(ui, "Cursor style", &mut config.cursor_style, CURSOR_STYLES);
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

                        section(ui, "Panels");
                        changed |=
                            slider_u8(ui, "Panel opacity", &mut config.panel_opacity, 50..=100);
                        ui.label(
                            RichText::new(
                                "Below 100% the settings and command panels are translucent and \
                             their backdrop is blurred.",
                            )
                            .size(11.0)
                            .color(Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
                        );
                    }
                    SettingsTab::Terminal => {
                        section(ui, "Layout");
                        changed |= combo(ui, "Tab layout", &mut config.tab_layout, LAYOUTS);
                        changed |= combo(
                            ui,
                            "Window chrome",
                            &mut config.window_chrome,
                            WINDOW_CHROME,
                        );

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
                    SettingsTab::Keybindings => {
                        section(ui, "Configured shortcuts");
                        changed |= keybindings_editor(ui, &mut config.keybindings);
                        ui.add_space(8.0);
                        if ui.button("Reset keybindings").clicked() {
                            config.keybindings = AppConfig::default().keybindings;
                            changed = true;
                        }
                    }
                });
        });
        changed
    }

    fn font_family_selector(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        ui.label(label("Font family"));
        if !self
            .font_families
            .iter()
            .any(|family| family == &config.font_family)
        {
            self.font_families = font_families_with_current(&config.font_family);
        }

        let before = config.font_family.clone();
        let mut selected = before.clone();
        ComboBox::from_id_salt("font_family")
            .selected_text(selected.as_str())
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for family in &self.font_families {
                    ui.selectable_value(&mut selected, family.clone(), family.as_str());
                }
            });

        if selected == before {
            false
        } else {
            config.font_family = selected;
            self.notice = None;
            true
        }
    }
}

fn font_families_with_current(current: &str) -> Vec<String> {
    let current = current.trim();
    let mut families = available_terminal_font_families();
    if !current.is_empty() && !families.iter().any(|family| family == current) {
        families.insert(0, current.to_string());
    }
    families
}

pub struct EguiLayer {
    ctx: Context,
    state: egui_winit::State,
    renderer: Renderer,
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_delta: egui::TexturesDelta,
    screen: ScreenDescriptor,
    blur_regions: Vec<BlurRegion>,
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
            blur_regions: Vec::new(),
        }
    }

    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    pub fn run(
        &mut self,
        window: &Window,
        ui_state: &mut UiState,
        config: &mut AppConfig,
        palette: &mut PaletteState,
    ) -> UiOutcome {
        let raw_input = self.state.take_egui_input(window);
        let mut outcome = UiOutcome::default();
        let full_output = self.ctx.run_ui(raw_input, |ui| {
            outcome = ui_state.draw(ui, config, palette);
        });
        self.state
            .handle_platform_output(window, full_output.platform_output);
        let size = window.inner_size();
        self.screen = ScreenDescriptor {
            size_in_pixels: [size.width.max(1), size.height.max(1)],
            pixels_per_point: full_output.pixels_per_point,
        };
        let ppp = full_output.pixels_per_point;
        self.blur_regions = ui_state
            .blur_regions()
            .iter()
            .filter_map(|rect| rect_to_region(rect, ppp, size.width, size.height))
            .collect();
        self.paint_jobs = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        self.textures_delta.append(full_output.textures_delta);
        outcome
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

    fn blur_regions(&self) -> &[BlurRegion] {
        &self.blur_regions
    }

    fn after_submit(&mut self) {
        self.free_textures();
    }
}

/// Convert an egui rect (points) to a physical-pixel [`BlurRegion`], clamped to
/// the surface. Returns `None` for empty/off-screen rects.
fn rect_to_region(rect: &egui::Rect, ppp: f32, width: u32, height: u32) -> Option<BlurRegion> {
    if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let left = (rect.min.x * ppp).floor().max(0.0) as u32;
    let top = (rect.min.y * ppp).floor().max(0.0) as u32;
    let right = ((rect.max.x * ppp).ceil() as u32).min(width);
    let bottom = ((rect.max.y * ppp).ceil() as u32).min(height);
    if right <= left || bottom <= top {
        return None;
    }
    Some(BlurRegion {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
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

/// Map a panel opacity percentage (50..=100) onto an 8-bit alpha. The renderer
/// blurs whatever shows through whenever this is below fully opaque.
fn panel_alpha(opacity_percent: u8) -> u8 {
    let percent = opacity_percent.clamp(0, 100) as f32 / 100.0;
    (percent * 255.0).round() as u8
}

fn panel_frame(alpha: u8) -> egui::Frame {
    egui::Frame::side_top_panel(&egui::Style::default())
        .fill(Color32::from_rgba_unmultiplied(17, 17, 22, alpha))
        .stroke(egui::Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 32),
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

fn keybindings_editor(ui: &mut Ui, keybindings: &mut [Keybinding]) -> bool {
    let mut changed = false;
    for keybinding in keybindings {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(action_label(&keybinding.action)).size(13.0));
                ui.label(
                    RichText::new(keybinding.action.as_str())
                        .size(11.0)
                        .color(Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.add_sized(
                    egui::vec2(180.0, 28.0),
                    TextEdit::singleline(&mut keybinding.keys)
                        .font(egui::TextStyle::Monospace)
                        .clip_text(false),
                );
                changed |= response.changed();
                if parse_combo(&keybinding.keys).is_none() {
                    ui.colored_label(Color32::from_rgb(248, 113, 113), "Invalid");
                }
            });
        });
        ui.separator();
    }
    changed
}

fn action_label(action: &str) -> String {
    match action {
        "tab.new" => "New Tab".to_string(),
        "tab.close" => "Close Tab".to_string(),
        "tab.rename" => "Rename Tab".to_string(),
        "palette.toggle" => "Command Palette".to_string(),
        "settings.toggle" => "Settings".to_string(),
        other => display_name(other),
    }
}

/// Outcome of drawing the command palette: the action to run (if any) and the
/// palette frame's rect (in points) when it is translucent enough to frost.
#[derive(Default)]
struct PaletteOverlay {
    action: Option<PaletteAction>,
    rect: Option<egui::Rect>,
}

fn command_palette_overlay(ctx: &Context, palette: &mut PaletteState, alpha: u8) -> PaletteOverlay {
    if !palette.open {
        return PaletteOverlay::default();
    }

    let mut action = None;
    let mut close = false;
    ctx.input(|input| {
        if input.key_pressed(EguiKey::Escape)
            || (input.modifiers.command && input.key_pressed(EguiKey::K))
        {
            close = true;
        }
        if input.key_pressed(EguiKey::ArrowDown) {
            palette.move_selection(1);
        }
        if input.key_pressed(EguiKey::ArrowUp) {
            palette.move_selection(-1);
        }
        if input.key_pressed(EguiKey::Enter) {
            action = palette.execute_selected();
        }
    });
    if close {
        palette.close();
        return PaletteOverlay::default();
    }

    let screen = ctx.content_rect();
    let backdrop = ctx.layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("phantom_command_palette_backdrop"),
    ));
    backdrop.rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 150));

    let width = PALETTE_WIDTH_POINTS.min((screen.width() - 32.0).max(320.0));
    let x = screen.center().x - width / 2.0;
    let y = (screen.top() + screen.height() * 0.12).max(screen.top() + 20.0);
    let area = Area::new(Id::new("phantom_command_palette"))
        .order(Order::Foreground)
        .fixed_pos(egui::pos2(x, y))
        .show(ctx, |ui| {
            let frame = Frame::new()
                .fill(Color32::from_rgba_unmultiplied(17, 17, 22, alpha))
                .stroke(egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 35),
                ))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            ui.min_rect().min,
                            egui::vec2(ui.available_width(), 2.0),
                        ),
                        0.0,
                        Color32::from_rgb(56, 189, 248),
                    );
                    ui.add_space(8.0);

                    let mut query = palette.query().to_string();
                    let response = ui.add_sized(
                        egui::vec2(ui.available_width(), 34.0),
                        TextEdit::singleline(&mut query)
                            .hint_text("Run command")
                            .font(egui::TextStyle::Monospace),
                    );
                    if palette.take_focus_request() {
                        response.request_focus();
                    }
                    if response.changed() {
                        palette.set_query(query);
                    }
                    ui.add_space(8.0);

                    let rows = palette.rows();
                    if rows.is_empty() {
                        ui.label(
                            RichText::new("No commands")
                                .size(13.0)
                                .color(Color32::from_rgba_unmultiplied(255, 255, 255, 120)),
                        );
                        return;
                    }
                    for row in rows {
                        let text = RichText::new(row.label).size(13.0);
                        let button = Button::selectable(row.selected, text)
                            .min_size(egui::vec2(ui.available_width(), 32.0));
                        if ui.add(button).clicked() {
                            action = palette.execute_filtered(row.filtered_index);
                        }
                    }
                });
            frame.response.rect
        });

    if action.is_some() {
        palette.close();
    }
    PaletteOverlay {
        action,
        rect: Some(area.inner),
    }
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
    use crate::palette::PaletteState;

    /// Recursively count pure-red strokes in egui's emitted shapes. egui's
    /// debug diagnostics (`warn_if_rect_changes_id`, id-clash) outline offending
    /// widgets with a red stroke; a stable UI must emit none.
    fn red_stroke_count(shape: &egui::Shape) -> usize {
        match shape {
            egui::Shape::Rect(rect) => usize::from(rect.stroke.color == Color32::RED),
            egui::Shape::Vec(shapes) => shapes.iter().map(red_stroke_count).sum(),
            _ => 0,
        }
    }

    fn run_settings_frame(
        ctx: &Context,
        state: &mut UiState,
        config: &mut AppConfig,
        palette: &mut PaletteState,
    ) -> usize {
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(raw_input, |ui| {
            state.draw(ui, config, palette);
        });
        output
            .shapes
            .iter()
            .map(|s| red_stroke_count(&s.shape))
            .sum()
    }

    #[test]
    fn switching_settings_tabs_does_not_destabilize_widget_ids() {
        let ctx = Context::default();
        // This debug diagnostic is on by default in debug builds; force it on so
        // the regression is caught regardless of build profile.
        ctx.global_style_mut(|style| style.debug.warn_if_rect_changes_id = true);

        let mut config = AppConfig::default();
        let mut palette = PaletteState::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);

        // Warm up so egui has a previous pass to compare against.
        run_settings_frame(&ctx, &mut state, &mut config, &mut palette);

        // Cycle through every tab (and back). Each tab reuses the same on-screen
        // rects, so without a per-tab parent id egui flags the swapped widgets in
        // red on the frame after the switch.
        let mut red = 0;
        for tab in SettingsTab::ALL
            .into_iter()
            .chain([SettingsTab::Appearance])
        {
            state.settings_tab = tab;
            red += run_settings_frame(&ctx, &mut state, &mut config, &mut palette);
        }

        assert_eq!(
            red, 0,
            "settings panel emitted {red} red id-instability outline(s) while switching tabs"
        );
    }

    #[test]
    fn color_round_trip_preserves_alpha_when_allowed() {
        let color = parse_color("#11223344");
        assert_eq!(format_color(color, true), "#11223344");
        assert_eq!(format_color(color, false), "#112233");
    }

    #[test]
    fn ui_state_clears_settings_panel_width_when_closed() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);

        state.open_settings(&config);
        state.panel_width_px = 320.0;

        state.close_panel();
        assert_eq!(state.panel_width_px, 0.0);
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

    #[test]
    fn font_selector_keeps_current_unknown_family_available() {
        let families = font_families_with_current("Custom Mono");

        assert_eq!(families.first().map(String::as_str), Some("Custom Mono"));
    }
}
