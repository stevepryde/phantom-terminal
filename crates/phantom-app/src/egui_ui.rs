//! egui control plane for the native app.
//!
//! `phantom-gfx` owns terminal rendering. egui owns non-terminal UI: settings,
//! contextual side panels, forms, sliders, colour controls, and future inspector
//! surfaces.

use egui::{
    Align2, Area, Button, Color32, Context, Frame, Id, Key as EguiKey, LayerId, Margin, Order,
    Panel, RichText, Slider, TextEdit, Ui, ViewportId,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use phantom_core::{
    AppConfig, Keybinding, ShellProfile, Theme, WindowSize, FREQUENT_COMMANDS_PLUGIN_ID,
    MANIFEST_PLUGIN_ID, RECENT_DIRECTORIES_PLUGIN_ID, SPDEPLOY_PLUGIN_ID,
};
use phantom_gfx::available_terminal_font_families;
use winit::event::WindowEvent;
use winit::window::Window;

use crate::context_actions::{ContextRequest, ContextSnapshot};
use crate::context_ui::ContextUi;
use crate::find::{FindResultSummary, FindState, FindUiOutcome};
use crate::gpu::{BlurRegion, FrameOverlay};
use crate::keybindings::{parse_combo, BuiltinShortcut, Combo, ComboKey};
use crate::palette::{PaletteAction, PaletteGroup, PaletteRow, PaletteState};
use crate::themes;
use crate::ui_components::{
    configure_phantom_style, with_alpha, CustomDropdown, APP_BACKGROUND, DANGER_TEXT, DIVIDER,
    ELEVATED_SURFACE, FOCUS_ACCENT, SIDEBAR_SURFACE, TEXT_MUTED, TEXT_SECONDARY,
};

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
    Profiles,
    Terminal,
    Session,
    ContextActions,
    Colours,
    Keybindings,
}

impl SettingsTab {
    const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Profiles,
        Self::Terminal,
        Self::Session,
        Self::ContextActions,
        Self::Colours,
        Self::Keybindings,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Profiles => "profiles",
            Self::Terminal => "terminal",
            Self::Session => "session",
            Self::ContextActions => "context_actions",
            Self::Colours => "colours",
            Self::Keybindings => "keybindings",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Profiles => "Profiles",
            Self::Terminal => "Terminal",
            Self::Session => "Session",
            Self::ContextActions => "Context Actions",
            Self::Colours => "Colours",
            Self::Keybindings => "Keybindings",
        }
    }
}

#[derive(Default)]
pub struct UiOutcome {
    pub config_changed: bool,
    pub palette_action: Option<PaletteAction>,
    pub context_request: Option<ContextRequest>,
    pub find: FindUiOutcome,
}

pub struct UiState {
    active_panel: Option<PanelKind>,
    settings_tab: SettingsTab,
    panel_width_px: f32,
    context_sidebar_width_px: f32,
    font_families: Vec<String>,
    notice: Option<String>,
    /// The settings draft, cloned from the live config when the panel opens
    /// and kept for the panel's lifetime so each frame doesn't re-clone the
    /// whole config. It may hold invalid intermediate states the user is
    /// typing through; only a draft that passes `AppConfig::validate()` is
    /// committed to the live config.
    settings_draft: Option<AppConfig>,
    profile_editor_index: Option<usize>,
    confirm_profile_delete: bool,
    confirm_settings_discard: bool,
    settings_focus_requested: bool,
    context_ui: ContextUi,
    find: FindState,
    /// Rects (egui points) of panels drawn translucent this frame, whose
    /// backdrop should be frosted. Rebuilt every `draw`.
    blur_regions: Vec<egui::Rect>,
}

#[derive(Clone, Copy)]
pub(crate) struct UiFrameContext<'a> {
    pub snapshot: &'a ContextSnapshot,
    pub frequent_commands: &'a [String],
    pub top_inset_points: f32,
    pub terminal_left_points: f32,
    pub terminal_right_points: f32,
    pub global_notice: Option<&'a str>,
    pub ephemeral_indicator_hovered: bool,
}

impl UiState {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            active_panel: None,
            settings_tab: SettingsTab::Appearance,
            panel_width_px: 0.0,
            context_sidebar_width_px: 0.0,
            font_families: font_families_with_current(&config.font_family),
            notice: None,
            settings_draft: None,
            profile_editor_index: None,
            confirm_profile_delete: false,
            confirm_settings_discard: false,
            settings_focus_requested: false,
            context_ui: ContextUi::default(),
            find: FindState::default(),
            blur_regions: Vec::new(),
        }
    }

    fn blur_regions(&self) -> &[egui::Rect] {
        &self.blur_regions
    }

    pub fn open_settings(&mut self, config: &AppConfig) {
        if self.settings_open() {
            // The command palette can temporarily cover Settings. Selecting
            // Settings again must reveal the existing draft, not replace a
            // potentially invalid edit with the live config.
            self.settings_focus_requested = true;
            return;
        }
        self.active_panel = Some(PanelKind::Settings);
        self.font_families = font_families_with_current(&config.font_family);
        self.notice = None;
        self.settings_draft = Some(config.clone());
        self.profile_editor_index = None;
        self.confirm_profile_delete = false;
        self.confirm_settings_discard = false;
        self.settings_focus_requested = true;
    }

    pub fn toggle_settings(&mut self, config: &AppConfig) {
        if self.settings_open() {
            self.close_panel();
        } else {
            self.open_settings(config);
        }
    }

    /// Request that the settings panel close. Invalid widget drafts are kept
    /// open until the user explicitly discards them through the inline prompt.
    pub fn close_panel(&mut self) -> bool {
        if self
            .settings_draft
            .as_ref()
            .is_some_and(|draft| validate_settings_draft(draft).is_err())
        {
            self.confirm_settings_discard = true;
            return false;
        }

        self.finish_close_panel();
        true
    }

    fn finish_close_panel(&mut self) {
        self.active_panel = None;
        self.panel_width_px = 0.0;
        self.notice = None;
        self.settings_draft = None;
        self.profile_editor_index = None;
        self.confirm_profile_delete = false;
        self.confirm_settings_discard = false;
        self.settings_focus_requested = false;
    }

    pub fn settings_open(&self) -> bool {
        self.active_panel == Some(PanelKind::Settings)
    }

    pub fn panel_open(&self) -> bool {
        self.active_panel.is_some()
    }

    /// Keep runtime-owned window geometry current in an open settings draft
    /// so a later valid widget edit cannot restore stale geometry.
    pub fn sync_window_size(&mut self, size: WindowSize) {
        if let Some(draft) = self.settings_draft.as_mut() {
            draft.window_size = Some(size);
        }
    }

    /// Keep a palette-driven theme change current in an open settings draft so
    /// a later settings apply cannot revert it.
    pub fn sync_ui_theme(&mut self, theme: &str) {
        if let Some(draft) = self.settings_draft.as_mut() {
            draft.ui_theme = theme.to_string();
        }
    }

    pub fn context_owns_keyboard(&self) -> bool {
        self.find.is_open() || self.context_ui.owns_keyboard()
    }

    /// Enter the contextual sidebar's egui focus order from the terminal.
    /// Returns whether expanding the sidebar changed persisted configuration.
    pub fn focus_context_sidebar(&mut self, config: &mut AppConfig) -> bool {
        if !config.context_actions.enabled {
            return false;
        }
        let changed = config.context_actions.panel_collapsed;
        config.context_actions.panel_collapsed = false;
        self.context_ui.request_sidebar_focus();
        changed
    }

    pub fn open_find(&mut self, selection_available: bool) {
        self.find.open(selection_available);
    }

    pub fn close_find(&mut self) {
        self.find.close();
    }

    pub fn find_open(&self) -> bool {
        self.find.is_open()
    }

    pub fn request_find_focus(&mut self) {
        self.find.request_focus();
    }

    pub fn expire_find_selection(&mut self) {
        self.find.expire_selection();
    }

    #[cfg(test)]
    pub fn configure_find_for_tests(&mut self, query: &str, selection_only: bool) {
        self.find.configure_for_tests(query, selection_only);
    }

    pub fn find_state(&self) -> &FindState {
        &self.find
    }

    pub fn set_find_results(&mut self, results: FindResultSummary) {
        self.find.set_results(results);
    }

    pub fn terminal_right_inset_px(&self) -> f32 {
        self.context_sidebar_width_px
    }

    fn draw(
        &mut self,
        ui: &mut Ui,
        config: &mut AppConfig,
        palette: &mut PaletteState,
        frame: UiFrameContext<'_>,
    ) -> UiOutcome {
        let mut outcome = UiOutcome::default();
        self.context_ui.begin_frame();
        // Modal overlays suppress sidebar widgets, but they must not release
        // the persistent sidebar's terminal reservation and resize the PTY
        // behind the overlay. Explicit collapse/disable still releases it.
        if !config.context_actions.enabled || config.context_actions.panel_collapsed {
            self.context_sidebar_width_px = 0.0;
        }
        self.blur_regions.clear();
        let alpha = panel_alpha(config.panel_opacity);
        let transparent = config.panel_opacity < 100;
        let context_alpha = context_panel_alpha(alpha);
        // Escape is also egui's "release focus" key for text fields: only
        // close the panel when no widget holds keyboard focus, so escaping
        // out of a keybinding edit doesn't take the whole panel with it.
        let widget_focused = ui.ctx().memory(|memory| memory.focused().is_some());
        if !palette.open
            && !widget_focused
            && ui.ctx().input(|input| input.key_pressed(egui::Key::Escape))
        {
            self.close_panel();
        }

        // The command palette is modal. Do not leave settings controls in the
        // accessibility or keyboard focus order behind its dimmed backdrop.
        if self.active_panel == Some(PanelKind::Settings) && !palette.open {
            let response = Panel::right("phantom_settings_panel")
                .default_size(PANEL_WIDTH_POINTS)
                .size_range(PANEL_MIN_WIDTH_POINTS..=PANEL_MAX_WIDTH_POINTS)
                .resizable(true)
                .frame(panel_frame(alpha))
                .show(ui, |ui| {
                    self.panel_width_px = ui.max_rect().width() * ui.ctx().pixels_per_point();
                    outcome.config_changed |= self.settings_panel(ui, config, alpha);
                });
            if transparent {
                self.blur_regions.push(response.response.rect);
            }
        } else {
            self.panel_width_px = 0.0;
        }
        let mut find_right_boundary = frame.terminal_right_points;
        if self.active_panel.is_none() && !palette.open {
            let context_overlay = self.context_ui.draw_with_commands(
                ui,
                config,
                frame.snapshot,
                frame.frequent_commands,
                frame.top_inset_points,
                context_alpha,
            );
            self.context_sidebar_width_px =
                context_overlay.reserved_width_points * ui.ctx().pixels_per_point();
            outcome.config_changed |= context_overlay.config_changed;
            outcome.context_request = context_overlay.request;
            if let Some(rect) = context_overlay.rect {
                self.blur_regions.push(rect);
            }
            find_right_boundary = context_overlay
                .find_overlay_right_edge_points
                .unwrap_or(find_right_boundary);
            let terminal_rect = egui::Rect::from_min_max(
                egui::pos2(frame.terminal_left_points, frame.top_inset_points),
                egui::pos2(frame.terminal_right_points, ui.max_rect().bottom()),
            );
            outcome.find =
                self.find
                    .draw(ui.ctx(), terminal_rect, find_right_boundary, context_alpha);
            if let Some(rect) = outcome.find.surface_rect {
                self.blur_regions.push(rect);
            }
        }
        let toggle_combo = config
            .keybindings
            .iter()
            .find(|kb| kb.action == "palette.toggle")
            .and_then(|kb| parse_combo(&kb.keys));
        let palette = command_palette_overlay(ui.ctx(), palette, alpha, toggle_combo.as_ref());
        outcome.palette_action = palette.action;
        if transparent {
            if let Some(rect) = palette.rect {
                self.blur_regions.push(rect);
            }
        }
        if let Some(notice) = frame.global_notice {
            global_notice_overlay(ui.ctx(), notice);
        }
        ephemeral_indicator_tooltip(ui.ctx(), frame.ephemeral_indicator_hovered);
        outcome
    }

    fn settings_panel(&mut self, ui: &mut Ui, config: &mut AppConfig, alpha: u8) -> bool {
        let mut changed = false;
        let mut close_requested = false;
        let mut discard_requested = false;
        // Widgets edit a draft, not the live config: a value is only applied
        // (and persisted) once the whole draft round-trips through
        // `AppConfig::validate()`. The draft persists while the panel is open
        // (so untouched frames don't clone or validate the whole config) and
        // may hold invalid intermediate states the user is typing through.
        let mut draft = self.settings_draft.take().unwrap_or_else(|| config.clone());

        ui.horizontal(|ui| {
            ui.heading("Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close_requested = true;
                }
            });
        });
        ui.add_space(8.0);
        if self.confirm_settings_discard {
            Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    "Your latest edit is invalid and has not been applied. Keep editing or discard it?",
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep editing").clicked() {
                        self.confirm_settings_discard = false;
                    }
                    if ui.button("Discard changes").clicked() {
                        discard_requested = true;
                    }
                });
            });
            ui.add_space(8.0);
        }
        if let Some(notice) = &self.notice {
            ui.colored_label(DANGER_TEXT, notice);
            ui.add_space(8.0);
        }

        ui.horizontal_top(|ui| {
            Frame::new()
                .fill(with_alpha(SIDEBAR_SURFACE, alpha))
                .corner_radius(4)
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(SETTINGS_NAV_WIDTH_POINTS - 12.0, ui.available_height()),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| self.settings_nav(ui),
                    );
                });

            ui.separator();

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    changed |= self.settings_content(ui, &mut draft);
                },
            );
        });

        // Nothing was edited this frame: keep the draft as-is and skip the
        // full-config validation pass.
        if !changed {
            self.settings_draft = Some(draft);
            if discard_requested {
                self.finish_close_panel();
            } else if close_requested {
                self.close_panel();
            }
            return false;
        }

        // Runtime-owned state the settings widgets never edit can move while
        // the panel is open (directory visits, window resizes); refresh it
        // from the live config so applying the draft cannot roll it back.
        draft.window_size = config.window_size;
        draft.context_actions.directory_history = config.context_actions.directory_history.clone();
        draft.trusted_projects = config.trusted_projects.clone();
        draft.trusted_spdeploy_projects = config.trusted_spdeploy_projects.clone();

        // Discard is authoritative even if the same input frame also made the
        // draft valid. Never let a confirmation action accidentally autosave.
        if discard_requested {
            self.settings_draft = Some(draft);
            self.finish_close_panel();
            return false;
        }

        // Core bounds validation plus the app-level rule that every binding
        // parses to a usable combo (core can't know the combo grammar).
        match validate_settings_draft(&draft) {
            Ok(()) => {
                self.notice = None;
                self.confirm_settings_discard = false;
                *config = draft.clone();
                self.settings_draft = Some(draft);
                if close_requested {
                    self.close_panel();
                }
                true
            }
            Err(error) => {
                self.notice = Some(error);
                self.settings_draft = Some(draft);
                if close_requested {
                    self.close_panel();
                }
                false
            }
        }
    }

    fn settings_nav(&mut self, ui: &mut Ui) {
        ui.set_width(SETTINGS_NAV_WIDTH_POINTS);
        ui.add_space(4.0);
        for tab in SettingsTab::ALL {
            let response = settings_tab_button(ui, tab, self.settings_tab == tab);
            if self.settings_focus_requested && self.settings_tab == tab {
                response.request_focus();
                self.settings_focus_requested = false;
            }
            if response.clicked() {
                self.settings_tab = tab;
                self.profile_editor_index = None;
                self.confirm_profile_delete = false;
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
                    SettingsTab::Profiles => {
                        changed |= self.profiles_settings(ui, config);
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
                    SettingsTab::ContextActions => {
                        section(ui, "Availability");
                        changed |= ui
                            .checkbox(
                                &mut config.context_actions.enabled,
                                "Enable contextual actions",
                            )
                            .changed();
                        ui.label(
                            RichText::new(
                                "When enabled, Phantom inspects only the active tab's current \
                                 directory. Detection never runs project code.",
                            )
                            .size(11.0)
                            .color(Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
                        );

                        section(ui, "Plugins");
                        ui.add_enabled_ui(config.context_actions.enabled, |ui| {
                            changed |= context_plugin_toggle(
                                ui,
                                config,
                                RECENT_DIRECTORIES_PLUGIN_ID,
                                "Recent directories",
                                "Quick access to recently and frequently used directories.",
                            );
                            ui.separator();
                            changed |= context_plugin_toggle(
                                ui,
                                config,
                                FREQUENT_COMMANDS_PLUGIN_ID,
                                "Frequent commands",
                                "Show this tab's three most frequent commands for manual reuse.",
                            );
                            ui.separator();
                            changed |= context_plugin_toggle(
                                ui,
                                config,
                                MANIFEST_PLUGIN_ID,
                                "Phantom manifest",
                                "Review and open tabs declared by .phantom.yml.",
                            );
                            ui.separator();
                            changed |= context_plugin_toggle(
                                ui,
                                config,
                                SPDEPLOY_PLUGIN_ID,
                                "spdeploy",
                                "List operations from deploy.yml and run one in a new tab.",
                            );
                        });
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
                        ui.add_space(20.0);
                        section(ui, "Built-in shortcuts");
                        ui.label(
                            RichText::new(
                                "These app shortcuts are always available and cannot be changed.",
                            )
                            .size(11.0)
                            .color(TEXT_MUTED),
                        );
                        builtin_shortcuts(ui);
                    }
                });
        });
        changed
    }

    fn profiles_settings(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        let Some(index) = self.profile_editor_index else {
            return self.profile_list(ui, config);
        };
        if index >= config.shell_profiles.len() {
            self.profile_editor_index = None;
            self.confirm_profile_delete = false;
            return false;
        }
        self.profile_editor(ui, config, index)
    }

    fn profile_list(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        let mut changed = false;
        section(ui, "Shell profiles");
        ui.label(
            RichText::new("Choose a profile to edit, or use the star to make it the default.")
                .size(11.0)
                .color(Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
        );
        ui.add_space(8.0);

        for index in 0..config.shell_profiles.len() {
            let profile = &config.shell_profiles[index];
            let is_default = profile.id == config.default_shell_profile_id;
            let profile_id = profile.id.clone();
            let name = profile.name.clone();
            let summary = profile_summary(profile);

            ui.horizontal(|ui| {
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &name,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::proportional(13.0),
                        color: Color32::from_rgba_unmultiplied(255, 255, 255, 230),
                        ..Default::default()
                    },
                );
                job.append(
                    &format!("\n{summary}"),
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::monospace(11.0),
                        color: Color32::from_rgba_unmultiplied(255, 255, 255, 110),
                        ..Default::default()
                    },
                );
                let row_width = (ui.available_width() - 64.0).max(40.0);
                let row = ui.add_sized([row_width, 44.0], Button::new(job));
                if profile_icon_button(
                    ui,
                    if is_default {
                        "Default profile"
                    } else {
                        "Make default"
                    },
                    ProfileIcon::Star { filled: is_default },
                    is_default,
                )
                .clicked()
                    && !is_default
                {
                    config.default_shell_profile_id = profile_id;
                    changed = true;
                }
                let edit = profile_icon_button(ui, "Edit profile", ProfileIcon::Edit, false);
                if row.clicked() || edit.clicked() {
                    self.profile_editor_index = Some(index);
                    self.confirm_profile_delete = false;
                }
            });
            ui.add_space(4.0);
        }

        ui.add_space(8.0);
        if ui.button("Add profile").clicked() {
            let new_profile = ShellProfile {
                id: next_profile_id(&config.shell_profiles),
                name: "New Profile".to_string(),
                command: String::new(),
                args: Vec::new(),
                cwd: None,
            };
            let mut candidate = config.clone();
            let index = candidate.shell_profiles.len();
            candidate.shell_profiles.push(new_profile);
            match candidate.validate() {
                Ok(()) => {
                    *config = candidate;
                    self.profile_editor_index = Some(index);
                    self.confirm_profile_delete = false;
                    changed = true;
                }
                Err(error) => self.notice = Some(error.to_string()),
            }
        }
        changed
    }

    fn profile_editor(&mut self, ui: &mut Ui, config: &mut AppConfig, index: usize) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            if ui.button("← Profiles").clicked() {
                self.profile_editor_index = None;
                self.confirm_profile_delete = false;
            }
            ui.label(
                RichText::new(&config.shell_profiles[index].name)
                    .size(13.0)
                    .strong(),
            );
        });

        section(ui, "Identity");
        let name_label = ui.label(label("Name"));
        changed |= ui
            .add_sized(
                [ui.available_width(), 32.0],
                TextEdit::singleline(&mut config.shell_profiles[index].name),
            )
            .labelled_by(name_label.id)
            .changed();

        section(ui, "Launch");
        let executable_label = ui.label(label("Executable"));
        changed |= ui
            .add_sized(
                [ui.available_width(), 32.0],
                TextEdit::singleline(&mut config.shell_profiles[index].command)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Use the user's $SHELL when empty"),
            )
            .labelled_by(executable_label.id)
            .changed();

        ui.add_space(8.0);
        let arguments_label = ui.label(label("Arguments"));
        let mut remove_arg = None;
        for (arg_index, arg) in config.shell_profiles[index].args.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let response = ui.add_sized(
                    [(ui.available_width() - 72.0).max(40.0), 32.0],
                    TextEdit::singleline(arg).font(egui::TextStyle::Monospace),
                );
                let accessible_label = format!("Argument {}", arg_index + 1);
                response.widget_info(|| {
                    let mut info =
                        egui::WidgetInfo::text_edit(true, arg.as_str(), arg.as_str(), "");
                    info.label = Some(accessible_label.clone());
                    info
                });
                changed |= response.labelled_by(arguments_label.id).changed();
                let remove = ui.button("Remove");
                remove.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("Remove argument {}", arg_index + 1),
                    )
                });
                if remove.clicked() {
                    remove_arg = Some(arg_index);
                }
            });
        }
        if let Some(arg_index) = remove_arg {
            config.shell_profiles[index].args.remove(arg_index);
            changed = true;
        }
        if ui.button("Add argument").clicked() {
            config.shell_profiles[index].args.push(String::new());
            changed = true;
        }

        ui.add_space(8.0);
        let cwd_label = ui.label(label("Working directory"));
        let mut cwd = config.shell_profiles[index].cwd.clone().unwrap_or_default();
        if ui
            .add_sized(
                [ui.available_width(), 32.0],
                TextEdit::singleline(&mut cwd)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Inherit when empty"),
            )
            .labelled_by(cwd_label.id)
            .changed()
        {
            config.shell_profiles[index].cwd = (!cwd.is_empty()).then_some(cwd);
            changed = true;
        }

        section(ui, "Delete profile");
        if config.shell_profiles.len() == 1 {
            ui.label(
                RichText::new("At least one shell profile is required.")
                    .size(11.0)
                    .color(Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
            );
        } else if self.confirm_profile_delete {
            ui.label("Delete this profile?");
            ui.horizontal(|ui| {
                if ui
                    .add(
                        Button::new("Delete")
                            .fill(Color32::from_rgba_unmultiplied(239, 68, 68, 77)),
                    )
                    .clicked()
                {
                    delete_profile(config, index);
                    self.profile_editor_index = None;
                    self.confirm_profile_delete = false;
                    changed = true;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_profile_delete = false;
                }
            });
        } else if ui
            .add(
                Button::new("Delete profile…")
                    .fill(Color32::from_rgba_unmultiplied(239, 68, 68, 77)),
            )
            .clicked()
        {
            self.confirm_profile_delete = true;
        }
        changed
    }

    fn font_family_selector(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        let label = ui.label(label("Font family"));
        if !self
            .font_families
            .iter()
            .any(|family| family == &config.font_family)
        {
            self.font_families = font_families_with_current(&config.font_family);
        }

        let before = config.font_family.clone();
        let mut selected = before.clone();
        CustomDropdown::new("font_family", selected.as_str())
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for family in &self.font_families {
                    ui.selectable_value(&mut selected, family.clone(), family.as_str());
                }
            })
            .response
            .labelled_by(label.id);

        if selected == before {
            false
        } else {
            config.font_family = selected;
            self.notice = None;
            true
        }
    }
}

#[derive(Clone, Copy)]
enum ProfileIcon {
    Star { filled: bool },
    Edit,
}

fn profile_icon_button(
    ui: &mut Ui,
    accessible_label: &str,
    icon: ProfileIcon,
    selected: bool,
) -> egui::Response {
    let label = RichText::new(accessible_label).color(Color32::TRANSPARENT);
    let response = ui.add_sized([28.0, 28.0], Button::new(label));
    response.widget_info(|| match icon {
        ProfileIcon::Star { .. } => {
            egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, accessible_label)
        }
        ProfileIcon::Edit => {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, accessible_label)
        }
    });
    let color = if selected || response.hovered() {
        Color32::from_rgb(125, 211, 252)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 150)
    };
    let painter = ui.painter_at(response.rect);
    match icon {
        ProfileIcon::Star { filled } => {
            let center = response.rect.center();
            let mut points = Vec::with_capacity(10);
            for point in 0..10 {
                let angle =
                    -std::f32::consts::FRAC_PI_2 + point as f32 * std::f32::consts::PI / 5.0;
                let radius = if point % 2 == 0 { 7.0 } else { 3.2 };
                points.push(center + egui::vec2(angle.cos(), angle.sin()) * radius);
            }
            if filled {
                for point in 0..points.len() {
                    painter.add(egui::Shape::convex_polygon(
                        vec![center, points[point], points[(point + 1) % points.len()]],
                        color,
                        egui::Stroke::NONE,
                    ));
                }
            }
            painter.add(egui::Shape::closed_line(
                points,
                egui::Stroke::new(1.4, color),
            ));
        }
        ProfileIcon::Edit => {
            let center = response.rect.center();
            painter.line_segment(
                [
                    center + egui::vec2(-5.0, 5.0),
                    center + egui::vec2(4.5, -4.5),
                ],
                egui::Stroke::new(2.0, color),
            );
            painter.line_segment(
                [
                    center + egui::vec2(3.0, -6.0),
                    center + egui::vec2(6.0, -3.0),
                ],
                egui::Stroke::new(2.0, color),
            );
            painter.line_segment(
                [
                    center + egui::vec2(-6.0, 6.0),
                    center + egui::vec2(-2.0, 5.0),
                ],
                egui::Stroke::new(1.5, color),
            );
        }
    }
    response.on_hover_text(accessible_label)
}

fn profile_summary(profile: &ShellProfile) -> String {
    let command = if profile.command.is_empty() {
        "$SHELL"
    } else {
        profile.command.as_str()
    };
    match profile.args.len() {
        0 => command.to_string(),
        1 => format!("{command} · 1 argument"),
        count => format!("{command} · {count} arguments"),
    }
}

fn next_profile_id(profiles: &[ShellProfile]) -> String {
    let mut suffix = profiles.len() + 1;
    loop {
        let candidate = format!("profile-{suffix}");
        if profiles.iter().all(|profile| profile.id != candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn delete_profile(config: &mut AppConfig, index: usize) -> bool {
    if config.shell_profiles.len() <= 1 || index >= config.shell_profiles.len() {
        return false;
    }
    let deleted_default = config.shell_profiles[index].id == config.default_shell_profile_id;
    config.shell_profiles.remove(index);
    if deleted_default {
        config.default_shell_profile_id = config.shell_profiles[0].id.clone();
    }
    true
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
    blur_suppressed: bool,
    /// egui's requested repaint delay from the last `run` (`MAX` = idle).
    repaint_delay: std::time::Duration,
}

impl EguiLayer {
    pub fn new(window: &Window, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let ctx = Context::default();
        configure_phantom_style(&ctx);
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
            blur_suppressed: false,
            repaint_delay: std::time::Duration::MAX,
        }
    }

    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    /// Forward events that egui itself must not consume. AccessKit's adapter
    /// still requires every native window event, including terminal-owned
    /// keyboard input and redraw notifications.
    pub fn on_accesskit_window_event(&mut self, window: &Window, event: &WindowEvent) {
        if let Some(accesskit) = self.state.accesskit.as_mut() {
            accesskit.process_event(window, event);
        }
    }

    pub fn init_accesskit<T>(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window: &Window,
        event_loop_proxy: winit::event_loop::EventLoopProxy<T>,
    ) where
        T: From<egui_winit::accesskit_winit::Event> + Send + 'static,
    {
        self.state
            .init_accesskit(event_loop, window, event_loop_proxy);
    }

    pub fn enable_accesskit(&self) {
        self.ctx.enable_accesskit();
    }

    pub fn disable_accesskit(&self) {
        self.ctx.disable_accesskit();
    }

    pub fn on_accesskit_action_request(&mut self, request: egui::accesskit::ActionRequest) {
        self.state.on_accesskit_action_request(request);
    }

    pub fn run_with_context(
        &mut self,
        window: &Window,
        ui_state: &mut UiState,
        config: &mut AppConfig,
        palette: &mut PaletteState,
        frame: UiFrameContext<'_>,
    ) -> UiOutcome {
        let raw_input = self.state.take_egui_input(window);
        let mut outcome = UiOutcome::default();
        let full_output = self.ctx.run_ui(raw_input, |ui| {
            outcome = ui_state.draw(ui, config, palette, frame);
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
        // egui's requested repaint (caret blink, animations) — `Duration::MAX`
        // when idle. The app folds this into its wake-up deadline; dropping it
        // would freeze egui animations between window events.
        self.repaint_delay = full_output
            .viewport_output
            .values()
            .map(|viewport| viewport.repaint_delay)
            .min()
            .unwrap_or(std::time::Duration::MAX);
        outcome
    }

    /// How soon egui wants to be repainted after the last `run`.
    pub fn repaint_delay(&self) -> std::time::Duration {
        self.repaint_delay
    }

    pub fn wants_keyboard_input(&self) -> bool {
        self.ctx.egui_wants_keyboard_input()
    }

    pub fn set_blur_suppressed(&mut self, suppressed: bool) {
        self.blur_suppressed = suppressed;
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
        active_blur_regions(&self.blur_regions, self.blur_suppressed)
    }

    fn after_submit(&mut self) {
        self.free_textures();
    }
}

fn active_blur_regions(regions: &[BlurRegion], suppressed: bool) -> &[BlurRegion] {
    if suppressed {
        &[]
    } else {
        regions
    }
}

fn global_notice_order() -> Order {
    Order::Tooltip
}

fn global_notice_overlay(ctx: &Context, notice: &str) {
    let content = ctx.content_rect();
    let position = egui::pos2(content.center().x, content.bottom() - 16.0);
    let max_width = (content.width() - 24.0).clamp(40.0, 720.0);
    Area::new(Id::new("phantom_global_notice"))
        .order(global_notice_order())
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(position)
        .interactable(false)
        .show(ctx, |ui| {
            Frame::new()
                .fill(Color32::from_rgba_unmultiplied(12, 16, 22, 245))
                .stroke(egui::Stroke::new(1.0, Color32::from_rgb(56, 189, 248)))
                .corner_radius(4)
                .inner_margin(Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.set_max_width(max_width);
                    ui.add(egui::Label::new(RichText::new(notice).size(12.0)).wrap());
                });
        });
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

/// Map a panel opacity percentage (50..=100) onto an 8-bit alpha. The renderer
/// blurs whatever shows through whenever this is below fully opaque.
fn panel_alpha(opacity_percent: u8) -> u8 {
    let percent = opacity_percent.clamp(0, 100) as f32 / 100.0;
    (percent * 255.0).round() as u8
}

/// Context actions sit over the full-window terminal backdrop and intentionally
/// remain translucent even when ordinary panels are configured as opaque.
fn context_panel_alpha(shared_panel_alpha: u8) -> u8 {
    shared_panel_alpha.min(220)
}

fn panel_frame(alpha: u8) -> egui::Frame {
    egui::Frame::side_top_panel(&egui::Style::default())
        .fill(with_alpha(APP_BACKGROUND, alpha))
        .stroke(egui::Stroke::new(1.0, DIVIDER))
        .corner_radius(4)
        .shadow(egui::epaint::Shadow {
            offset: [-3, 0],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(100),
        })
        .inner_margin(egui::Margin::same(20))
}

fn settings_tab_button(ui: &mut Ui, tab: SettingsTab, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(SETTINGS_NAV_WIDTH_POINTS - 18.0, 36.0),
        egui::Sense::click(),
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, tab.label())
    });
    let visuals = ui.style().interact(&response);
    if selected || response.hovered() || response.has_focus() {
        let fill = if selected {
            ui.visuals().widgets.active.weak_bg_fill
        } else {
            visuals.weak_bg_fill
        };
        ui.painter().rect_filled(rect, 4.0, fill);
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, FOCUS_ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        tab.label(),
        egui::FontId::proportional(13.0),
        visuals.fg_stroke.color,
    );
    if selected {
        ui.painter().vline(
            rect.left() + 1.0,
            (rect.top() + 7.0)..=(rect.bottom() - 7.0),
            egui::Stroke::new(2.0, FOCUS_ACCENT),
        );
    }
    response
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(16.0);
    ui.label(
        RichText::new(title.to_ascii_uppercase())
            .size(11.0)
            .color(TEXT_SECONDARY),
    );
    ui.separator();
}

fn context_plugin_toggle(
    ui: &mut Ui,
    config: &mut AppConfig,
    id: &str,
    name: &str,
    description: &str,
) -> bool {
    let Some(plugin) = config.context_actions.plugin_mut(id) else {
        return false;
    };
    let mut changed = false;
    ui.horizontal(|ui| {
        let name_label = ui
            .vertical(|ui| {
                let name_label = ui.label(RichText::new(name).size(13.0));
                ui.label(
                    RichText::new(description)
                        .size(11.0)
                        .color(Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
                );
                name_label
            })
            .inner;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let enabled = ui.checkbox(&mut plugin.enabled, "Enabled");
            enabled.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    true,
                    plugin.enabled,
                    format!("Enable {name}"),
                )
            });
            changed |= enabled.labelled_by(name_label.id).changed();
        });
    });
    changed
}

fn label(text: &str) -> RichText {
    RichText::new(text).size(12.0).color(TEXT_SECONDARY)
}

fn slider_u8(
    ui: &mut Ui,
    label_text: &str,
    value: &mut u8,
    range: std::ops::RangeInclusive<u8>,
) -> bool {
    let label = ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true))
        .labelled_by(label.id)
        .changed()
}

fn slider_u16(
    ui: &mut Ui,
    label_text: &str,
    value: &mut u16,
    range: std::ops::RangeInclusive<u16>,
) -> bool {
    let label = ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true))
        .labelled_by(label.id)
        .changed()
}

fn slider_u32(
    ui: &mut Ui,
    label_text: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
) -> bool {
    let label = ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true))
        .labelled_by(label.id)
        .changed()
}

fn slider_f32(
    ui: &mut Ui,
    label_text: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    let label = ui.label(label(label_text));
    ui.add(Slider::new(value, range).show_value(true))
        .labelled_by(label.id)
        .changed()
}

fn combo(ui: &mut Ui, label_text: &str, value: &mut String, options: &[&str]) -> bool {
    let label = ui.label(label(label_text));
    let mut changed = false;
    CustomDropdown::new(label_text, display_name(value))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for option in options {
                changed |= ui
                    .selectable_value(value, (*option).to_string(), display_name(option))
                    .changed();
            }
        })
        .response
        .labelled_by(label.id);
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
                let accessible_label = format!("{} shortcut", action_label(&keybinding.action));
                let response = ui.add_sized(
                    egui::vec2(180.0, 28.0),
                    TextEdit::singleline(&mut keybinding.keys)
                        .font(egui::TextStyle::Monospace)
                        .clip_text(false),
                );
                response.widget_info(|| {
                    let mut info =
                        egui::WidgetInfo::text_edit(true, &keybinding.keys, &keybinding.keys, "");
                    info.label = Some(accessible_label.clone());
                    info
                });
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

fn builtin_shortcuts(ui: &mut Ui) {
    for shortcut in BuiltinShortcut::ALL {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(shortcut.label()).size(13.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.add_sized(
                    egui::vec2(140.0, 20.0),
                    egui::Label::new(
                        RichText::new(shortcut.keys())
                            .monospace()
                            .size(12.0)
                            .color(TEXT_SECONDARY),
                    ),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Label,
                        true,
                        format!("{} shortcut: {}", shortcut.label(), shortcut.keys()),
                    )
                });
                ui.ctx().accesskit_node_builder(response.id, |node| {
                    node.set_label(format!(
                        "{} shortcut: {}",
                        shortcut.label(),
                        shortcut.keys()
                    ));
                });
            });
        });
        ui.separator();
    }
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

fn validate_settings_draft(config: &AppConfig) -> Result<(), String> {
    config.validate().map_err(|error| error.to_string())?;
    match config
        .keybindings
        .iter()
        .find(|keybinding| parse_combo(&keybinding.keys).is_none())
    {
        Some(bad) => Err(format!(
            "keybinding '{}' is not a valid key combination \
             (letter keys need Cmd/Ctrl/Alt)",
            bad.keys
        )),
        None => Ok(()),
    }
}

/// Outcome of drawing the command palette: the action to run (if any) and the
/// palette frame's rect (in points) when it is translucent enough to frost.
#[derive(Default)]
struct PaletteOverlay {
    action: Option<PaletteAction>,
    rect: Option<egui::Rect>,
}

fn command_palette_overlay(
    ctx: &Context,
    palette: &mut PaletteState,
    alpha: u8,
    toggle_combo: Option<&Combo>,
) -> PaletteOverlay {
    if !palette.open {
        return PaletteOverlay::default();
    }

    let mut action = None;
    let mut close = false;
    ctx.input(|input| {
        // The configured `palette.toggle` chord closes an open palette, so a
        // rebound shortcut keeps working both ways. Escape always closes.
        let chord_close = match toggle_combo {
            Some(combo) => combo_matches_egui_input(input, combo),
            None => input.modifiers.command && input.key_pressed(EguiKey::K),
        };
        if input.key_pressed(EguiKey::Escape) || chord_close {
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
                .fill(with_alpha(ELEVATED_SURFACE, alpha))
                .stroke(egui::Stroke::new(1.0, DIVIDER))
                .corner_radius(4)
                .shadow(egui::epaint::Shadow {
                    offset: [0, 5],
                    blur: 20,
                    spread: 0,
                    color: Color32::from_black_alpha(130),
                })
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.label(
                        RichText::new("COMMAND PALETTE")
                            .size(11.0)
                            .strong()
                            .color(TEXT_SECONDARY),
                    );
                    ui.add_space(4.0);

                    let previous_query = palette.query().to_string();
                    let mut query = previous_query.clone();
                    let response = ui.add_sized(
                        egui::vec2(ui.available_width(), 34.0),
                        TextEdit::singleline(&mut query)
                            .hint_text("Run command")
                            .font(egui::TextStyle::Monospace),
                    );
                    response.widget_info(|| {
                        let mut info = egui::WidgetInfo::text_edit(
                            true,
                            &previous_query,
                            &query,
                            "Run command",
                        );
                        info.label = Some("Command search".to_string());
                        info
                    });
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
                    let show_groups = palette.query().is_empty();
                    let mut previous_group = None;
                    for row in rows {
                        if show_groups && previous_group != Some(row.group) {
                            if previous_group.is_some() {
                                ui.add_space(4.0);
                            }
                            ui.label(
                                RichText::new(row.group.label())
                                    .size(10.0)
                                    .strong()
                                    .color(TEXT_MUTED),
                            );
                            previous_group = Some(row.group);
                        }
                        let clicked = ui
                            .push_id(("palette_result", &row.id), |ui| {
                                palette_result_row(ui, &row).clicked()
                            })
                            .inner;
                        if clicked {
                            action = palette.execute_filtered(row.filtered_index);
                        }
                    }
                    if let Some(window) = palette.visible_window() {
                        let hint = format!(
                            "Showing {}–{} of {} · Use ↑/↓ to browse",
                            window.first, window.last, window.total
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new(&hint).size(11.0).color(TEXT_MUTED));
                        ui.ctx().accesskit_node_builder(response.id, |node| {
                            node.set_description(hint.as_str());
                        });
                    }
                });
            frame.response.rect
        });

    // A click on the dimmed backdrop (outside the palette frame) dismisses it,
    // matching every other overlay convention.
    let clicked_backdrop = ctx.input(|input| {
        input.pointer.primary_pressed()
            && input
                .pointer
                .interact_pos()
                .is_some_and(|pos| !area.inner.contains(pos))
    });
    if clicked_backdrop {
        palette.close();
        return PaletteOverlay::default();
    }

    if action.is_some() {
        palette.close();
    }
    PaletteOverlay {
        action,
        rect: Some(area.inner),
    }
}

fn ephemeral_indicator_tooltip(ctx: &Context, hovered: bool) {
    if !hovered {
        return;
    }
    let id = Id::new("phantom_ephemeral_indicator_tooltip");
    let layer = LayerId::new(Order::Foreground, Id::new("phantom_native_chrome"));
    egui::Tooltip::always_open(ctx.clone(), layer, id, egui::PopupAnchor::Pointer).show(|ui| {
        ui.label("Temporary session — tabs will not be saved or restored");
    });
}

fn palette_result_row(ui: &mut Ui, row: &PaletteRow) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            true,
            row.selected,
            row.label.as_str(),
        )
    });
    let visuals = ui.style().interact(&response);
    if row.selected || response.hovered() || response.has_focus() {
        let fill = if row.selected {
            ui.visuals().widgets.active.weak_bg_fill
        } else {
            visuals.weak_bg_fill
        };
        ui.painter().rect_filled(rect, 4.0, fill);
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, FOCUS_ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    let label_color = if row.group == PaletteGroup::Appearance && !row.selected {
        TEXT_SECONDARY
    } else {
        visuals.fg_stroke.color
    };
    ui.painter().text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        &row.label,
        egui::FontId::proportional(13.0),
        label_color,
    );
    if let Some(shortcut) = &row.shortcut {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            shortcut,
            egui::FontId::monospace(11.0),
            TEXT_MUTED,
        );
    }
    response
}

/// Whether the parsed config combo is pressed in this egui frame. egui's
/// `command` modifier already means Cmd on macOS / Ctrl elsewhere, matching
/// `Combo::primary`; `Combo::ctrl` is the literal Ctrl key (macOS only).
fn combo_matches_egui_input(input: &egui::InputState, combo: &Combo) -> bool {
    let Some(key) = egui_key_for_combo(&combo.key) else {
        return false;
    };
    if !input.key_pressed(key) {
        return false;
    }
    let mods = input.modifiers;
    (!combo.primary || mods.command)
        && (!combo.ctrl || mods.ctrl)
        && (!combo.alt || mods.alt)
        && (!combo.shift || mods.shift)
}

fn egui_key_for_combo(key: &ComboKey) -> Option<EguiKey> {
    match key {
        // Letters/digits register under their uppercase name; punctuation
        // under its symbol.
        ComboKey::Char(c) => EguiKey::from_name(&c.to_ascii_uppercase().to_string())
            .or_else(|| EguiKey::from_name(&c.to_string())),
        // Named keys are stored lowercase ("escape", "f2"); egui uses
        // capitalized names ("Escape", "F2").
        ComboKey::Named(name) => {
            let mut chars = name.chars();
            let capitalized: String = chars
                .next()
                .map(|c| c.to_ascii_uppercase())
                .into_iter()
                .chain(chars)
                .collect();
            EguiKey::from_name(&capitalized).or_else(|| EguiKey::from_name(name))
        }
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
            let response = if alpha {
                ui.color_edit_button_srgba_unmultiplied(&mut color)
            } else {
                let mut rgb = [color[0], color[1], color[2]];
                let response = ui.color_edit_button_srgb(&mut rgb);
                if response.changed() {
                    color = [rgb[0], rgb[1], rgb[2], 255];
                }
                response
            };
            let accessible_value = format_color(color, alpha);
            response.widget_info(|| {
                let mut info = egui::WidgetInfo::new(egui::WidgetType::ColorButton);
                info.enabled = ui.is_enabled();
                info.label = Some(name.to_string());
                info.current_text_value = Some(accessible_value.clone());
                info
            });
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
    use egui::accesskit::{Role, Toggled};

    #[test]
    fn window_resize_keeps_invalid_settings_draft_geometry_current() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        let mut draft = config;
        draft.font_size = 0;
        state.settings_draft = Some(draft);
        let size = WindowSize::new(1280, 720).unwrap();

        state.sync_window_size(size);

        assert_eq!(
            state.settings_draft.as_ref().unwrap().window_size,
            Some(size)
        );
    }

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

    fn text_uses_color(shape: &egui::Shape, expected: &str, color: Color32) -> bool {
        match shape {
            egui::Shape::Text(text) => {
                text.galley.job.text == expected
                    && text
                        .galley
                        .job
                        .sections
                        .iter()
                        .any(|section| section.format.color == color)
            }
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .any(|shape| text_uses_color(shape, expected, color)),
            _ => false,
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
            state.draw(
                ui,
                config,
                palette,
                UiFrameContext {
                    snapshot: &ContextSnapshot::empty(std::path::PathBuf::from("/tmp")),
                    frequent_commands: &[],
                    top_inset_points: 0.0,
                    terminal_left_points: 0.0,
                    terminal_right_points: 1280.0,
                    global_notice: None,
                    ephemeral_indicator_hovered: false,
                },
            );
        });
        output
            .shapes
            .iter()
            .map(|s| red_stroke_count(&s.shape))
            .sum()
    }

    fn test_raw_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        }
    }

    fn run_ui_frame(
        ctx: &Context,
        state: &mut UiState,
        config: &mut AppConfig,
        palette: &mut PaletteState,
    ) {
        let _ = ctx.run_ui(test_raw_input(), |ui| {
            state.draw(
                ui,
                config,
                palette,
                UiFrameContext {
                    snapshot: &ContextSnapshot::empty(std::path::PathBuf::from("/tmp")),
                    frequent_commands: &[],
                    top_inset_points: 0.0,
                    terminal_left_points: 0.0,
                    terminal_right_points: 1280.0,
                    global_notice: None,
                    ephemeral_indicator_hovered: false,
                },
            );
        });
    }

    fn settings_accesskit_update(tab: SettingsTab) -> egui::accesskit::TreeUpdate {
        let ctx = Context::default();
        ctx.enable_accesskit();
        let mut config = AppConfig::default();
        let mut palette = PaletteState::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_tab = tab;

        ctx.run_ui(test_raw_input(), |ui| {
            state.draw(
                ui,
                &mut config,
                &mut palette,
                UiFrameContext {
                    snapshot: &ContextSnapshot::empty(std::path::PathBuf::from("/tmp")),
                    frequent_commands: &[],
                    top_inset_points: 0.0,
                    terminal_left_points: 0.0,
                    terminal_right_points: 1280.0,
                    global_notice: None,
                    ephemeral_indicator_hovered: false,
                },
            );
        })
        .platform_output
        .accesskit_update
        .unwrap()
    }

    #[test]
    fn settings_expose_selected_tabs_and_labelled_slider_values() {
        let update = settings_accesskit_update(SettingsTab::Appearance);

        let appearance = update.nodes.iter().find(|(_, node)| {
            node.role() == Role::Button
                && node.label() == Some("Appearance")
                && node.toggled() == Some(Toggled::True)
        });
        assert!(appearance.is_some());
        assert_eq!(update.focus, appearance.unwrap().0);
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::Slider
                && node.numeric_value().is_some()
                && !node.labelled_by().is_empty()
        }));
    }

    #[test]
    fn context_plugin_toggles_have_distinct_names() {
        let update = settings_accesskit_update(SettingsTab::ContextActions);

        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::CheckBox && node.label() == Some("Enable Recent directories")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::CheckBox && node.label() == Some("Enable Frequent commands")
        }));
        assert!(!update.nodes.iter().any(|(_, node)| {
            node.label()
                .is_some_and(|label| label.starts_with("Order "))
        }));
    }

    #[test]
    fn colour_wells_expose_their_current_hex_value() {
        let update = settings_accesskit_update(SettingsTab::Colours);

        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::ColorWell
                && node.label() == Some("Background")
                && node.value() == Some("#0b0b0e")
        }));
    }

    #[test]
    fn keybindings_settings_expose_read_only_builtin_shortcuts() {
        let ctx = Context::default();
        ctx.enable_accesskit();
        let output = ctx.run_ui(test_raw_input(), builtin_shortcuts);
        let update = output.platform_output.accesskit_update.unwrap();

        for shortcut in BuiltinShortcut::ALL {
            let expected = format!("{} shortcut: {}", shortcut.label(), shortcut.keys());
            assert!(update.nodes.iter().any(|(_, node)| {
                node.role() == Role::Label && node.label() == Some(expected.as_str())
            }));
        }
        assert!(!update.nodes.iter().any(|(_, node)| {
            node.role() == Role::Button
                && BuiltinShortcut::ALL
                    .iter()
                    .any(|shortcut| node.label() == Some(shortcut.label()))
        }));
    }

    #[test]
    fn builtin_shortcuts_stay_within_a_narrow_settings_column() {
        let ctx = Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(300.0, 800.0),
            )),
            ..Default::default()
        };
        let mut used_rect = egui::Rect::NOTHING;
        let output = ctx.run_ui(input, |ui| {
            used_rect = ui.scope(builtin_shortcuts).response.rect;
        });

        assert!(used_rect.right() <= 300.0);
        assert_eq!(
            output
                .shapes
                .iter()
                .map(|shape| red_stroke_count(&shape.shape))
                .sum::<usize>(),
            0
        );
    }

    #[test]
    fn command_palette_exposes_query_value_and_selected_result() {
        let ctx = Context::default();
        ctx.enable_accesskit();
        let config = AppConfig::default();
        let mut palette = PaletteState::default();
        palette.open(&config);

        let output = ctx.run_ui(test_raw_input(), |_ui| {
            command_palette_overlay(&ctx, &mut palette, 220, None);
        });
        let update = output.platform_output.accesskit_update.unwrap();

        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::TextInput
                && node.label() == Some("Command search")
                && node.value() == Some("")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::Button
                && node.label() == Some("New Tab")
                && node.toggled() == Some(Toggled::True)
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::TextInput
                && node
                    .description()
                    .is_some_and(|description| description.starts_with("Showing 1–10 of "))
        }));
    }

    #[test]
    fn settings_validation_notice_uses_the_danger_text_token() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut palette = PaletteState::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.notice = Some("Invalid settings draft".to_string());

        let output = ctx.run_ui(test_raw_input(), |ui| {
            state.draw(
                ui,
                &mut config,
                &mut palette,
                UiFrameContext {
                    snapshot: &ContextSnapshot::empty(std::path::PathBuf::from("/tmp")),
                    frequent_commands: &[],
                    top_inset_points: 0.0,
                    terminal_left_points: 0.0,
                    terminal_right_points: 1280.0,
                    global_notice: None,
                    ephemeral_indicator_hovered: false,
                },
            );
        });

        assert!(output
            .shapes
            .iter()
            .any(|shape| { text_uses_color(&shape.shape, "Invalid settings draft", DANGER_TEXT) }));
    }

    #[test]
    fn command_palette_hides_settings_from_the_accessibility_tree() {
        let ctx = Context::default();
        ctx.enable_accesskit();
        let mut config = AppConfig::default();
        let mut palette = PaletteState::default();
        palette.open(&config);
        let mut state = UiState::new(&config);
        state.open_settings(&config);

        let output = ctx.run_ui(test_raw_input(), |ui| {
            state.draw(
                ui,
                &mut config,
                &mut palette,
                UiFrameContext {
                    snapshot: &ContextSnapshot::empty(std::path::PathBuf::from("/tmp")),
                    frequent_commands: &[],
                    top_inset_points: 0.0,
                    terminal_left_points: 0.0,
                    terminal_right_points: 1280.0,
                    global_notice: None,
                    ephemeral_indicator_hovered: false,
                },
            );
        });
        let update = output.platform_output.accesskit_update.unwrap();

        assert!(update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Command search")));
        assert!(!update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Appearance")));
    }

    #[test]
    fn context_sidebar_reservation_survives_settings_and_palette_overlays() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut palette = PaletteState::default();
        let mut state = UiState::new(&config);

        run_ui_frame(&ctx, &mut state, &mut config, &mut palette);
        let expanded_width = state.terminal_right_inset_px();
        assert!(expanded_width > 0.0);

        state.open_settings(&config);
        run_ui_frame(&ctx, &mut state, &mut config, &mut palette);
        assert_eq!(state.terminal_right_inset_px(), expanded_width);

        assert!(state.close_panel());
        palette.open(&config);
        run_ui_frame(&ctx, &mut state, &mut config, &mut palette);
        assert_eq!(state.terminal_right_inset_px(), expanded_width);

        config.context_actions.panel_collapsed = true;
        run_ui_frame(&ctx, &mut state, &mut config, &mut palette);
        assert_eq!(state.terminal_right_inset_px(), 0.0);
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

        assert!(state.close_panel());
        assert_eq!(state.panel_width_px, 0.0);
    }

    #[test]
    fn invalid_settings_draft_requires_explicit_discard() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_draft.as_mut().unwrap().keybindings[0].keys = "T".to_string();

        assert!(!state.close_panel());
        assert!(state.settings_open());
        assert!(state.confirm_settings_discard);
        assert_eq!(config.keybindings[0].keys, "CmdOrCtrl+T");

        state.finish_close_panel();
        assert!(!state.settings_open());
        assert!(state.settings_draft.is_none());
        assert_eq!(config.keybindings[0].keys, "CmdOrCtrl+T");
    }

    #[test]
    fn settings_toggle_uses_guarded_close_for_invalid_drafts() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_draft.as_mut().unwrap().keybindings[0].keys = "T".to_string();

        state.toggle_settings(&config);

        assert!(state.settings_open());
        assert!(state.confirm_settings_discard);
    }

    #[test]
    fn reopening_settings_preserves_an_existing_invalid_draft() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_draft.as_mut().unwrap().keybindings[0].keys = "T".to_string();

        state.open_settings(&config);

        assert_eq!(
            state.settings_draft.as_ref().unwrap().keybindings[0].keys,
            "T"
        );
    }

    #[test]
    fn valid_settings_draft_closes_without_confirmation() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);

        assert!(state.close_panel());
        assert!(!state.settings_open());
        assert!(!state.confirm_settings_discard);
    }

    #[test]
    fn escape_requests_confirmation_for_an_invalid_settings_draft() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut palette = PaletteState::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_draft.as_mut().unwrap().keybindings[0].keys = "T".to_string();
        let mut input = test_raw_input();
        input.events.push(egui::Event::Key {
            key: EguiKey::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });

        let _ = ctx.run_ui(input, |ui| {
            state.draw(
                ui,
                &mut config,
                &mut palette,
                UiFrameContext {
                    snapshot: &ContextSnapshot::empty(std::path::PathBuf::from("/tmp")),
                    frequent_commands: &[],
                    top_inset_points: 0.0,
                    terminal_left_points: 0.0,
                    terminal_right_points: 1280.0,
                    global_notice: None,
                    ephemeral_indicator_hovered: false,
                },
            );
        });

        assert!(state.settings_open());
        assert!(state.confirm_settings_discard);
    }

    #[test]
    fn discard_confirmation_actions_are_accessible() {
        let ctx = Context::default();
        ctx.enable_accesskit();
        let mut config = AppConfig::default();
        let mut palette = PaletteState::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_draft.as_mut().unwrap().keybindings[0].keys = "T".to_string();
        assert!(!state.close_panel());

        let output = ctx.run_ui(test_raw_input(), |ui| {
            state.draw(
                ui,
                &mut config,
                &mut palette,
                UiFrameContext {
                    snapshot: &ContextSnapshot::empty(std::path::PathBuf::from("/tmp")),
                    frequent_commands: &[],
                    top_inset_points: 0.0,
                    terminal_left_points: 0.0,
                    terminal_right_points: 1280.0,
                    global_notice: None,
                    ephemeral_indicator_hovered: false,
                },
            );
        });
        let update = output.platform_output.accesskit_update.unwrap();

        for label in ["Keep editing", "Discard changes"] {
            assert!(update
                .nodes
                .iter()
                .any(|(_, node)| { node.role() == Role::Button && node.label() == Some(label) }));
        }
    }

    #[test]
    fn ui_state_keeps_settings_tab_selection_when_reopened() {
        let config = AppConfig::default();
        let mut state = UiState::new(&config);
        state.open_settings(&config);
        state.settings_tab = SettingsTab::Terminal;

        assert!(state.close_panel());
        state.open_settings(&config);

        assert_eq!(state.settings_tab, SettingsTab::Terminal);
    }

    #[test]
    fn font_selector_keeps_current_unknown_family_available() {
        let families = font_families_with_current("Custom Mono");

        assert_eq!(families.first().map(String::as_str), Some("Custom Mono"));
    }

    #[test]
    fn next_profile_id_skips_existing_suffixes() {
        let mut profiles = AppConfig::default().shell_profiles;
        profiles.push(ShellProfile {
            id: "profile-2".to_string(),
            name: "Two".to_string(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
        });
        profiles.push(ShellProfile {
            id: "profile-3".to_string(),
            name: "Three".to_string(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
        });

        assert_eq!(next_profile_id(&profiles), "profile-4");
    }

    #[test]
    fn deleting_default_profile_selects_a_remaining_profile() {
        let mut config = AppConfig::default();
        config.shell_profiles.push(ShellProfile {
            id: "secondary".to_string(),
            name: "Secondary".to_string(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
        });

        assert!(delete_profile(&mut config, 0));
        assert_eq!(config.default_shell_profile_id, "secondary");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn deleting_the_only_profile_is_rejected() {
        let mut config = AppConfig::default();

        assert!(!delete_profile(&mut config, 0));
        assert_eq!(config.shell_profiles.len(), 1);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn live_resize_suppresses_expensive_backdrop_blur() {
        let regions = [BlurRegion {
            x: 10,
            y: 20,
            width: 300,
            height: 400,
        }];

        assert_eq!(active_blur_regions(&regions, false), &regions);
        assert!(active_blur_regions(&regions, true).is_empty());
    }

    #[test]
    fn global_notices_use_the_topmost_egui_layer() {
        assert_eq!(global_notice_order(), Order::Tooltip);
    }

    #[test]
    fn translucent_find_surface_registers_for_backdrop_blur() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        config.context_actions.enabled = false;
        let mut palette = PaletteState::default();
        let mut state = UiState::new(&config);
        state.open_find(false);

        run_ui_frame(&ctx, &mut state, &mut config, &mut palette);

        assert_eq!(state.blur_regions.len(), 1);
        assert!(state.blur_regions[0].width() > 0.0);
        assert!(state.blur_regions[0].height() > 0.0);
    }

    #[test]
    fn ephemeral_indicator_hover_draws_an_explanation_without_a_widget_hit_target() {
        let ctx = Context::default();
        let mut input = test_raw_input();
        input
            .events
            .push(egui::Event::PointerMoved(egui::pos2(20.0, 20.0)));

        let _ = ctx.run_ui(input, |_ui| ephemeral_indicator_tooltip(&ctx, true));
        let output = ctx.run_ui(test_raw_input(), |_ui| {
            ephemeral_indicator_tooltip(&ctx, true)
        });

        assert!(output.shapes.iter().any(|shape| {
            match &shape.shape {
                egui::Shape::Text(text) => text
                    .galley
                    .job
                    .text
                    .contains("tabs will not be saved or restored"),
                egui::Shape::Vec(shapes) => shapes.iter().any(|shape| {
                    matches!(shape, egui::Shape::Text(text) if text.galley.job.text.contains("tabs will not be saved or restored"))
                }),
                _ => false,
            }
        }));
    }

    #[test]
    fn context_sidebar_remains_translucent_when_other_panels_are_opaque() {
        assert_eq!(panel_alpha(100), 255);
        assert_eq!(context_panel_alpha(panel_alpha(100)), 220);
        assert_eq!(context_panel_alpha(panel_alpha(50)), 128);
    }
}
