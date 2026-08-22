//! Non-modal egui presentation for directory-aware contextual actions.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use egui::{
    Align2, Area, Color32, ComboBox, FontId, Frame, Id, Margin, Order, RichText, Sense, Ui,
    UiBuilder,
};
use phantom_core::{
    default_home_dir, AppConfig, FREQUENT_COMMANDS_PLUGIN_ID, MANIFEST_PLUGIN_ID,
    MAX_CONTEXT_SIDEBAR_WIDTH, MIN_CONTEXT_SIDEBAR_WIDTH, RECENT_DIRECTORIES_PLUGIN_ID,
    SPDEPLOY_PLUGIN_ID,
};

use crate::context_actions::{
    ContextRequest, ContextSection, ContextSectionContent, ContextSnapshot, DirectoryTarget,
    FrequentCommandsSection, ManifestSection, ManifestTab, ManifestTrustState,
    RecentDirectoriesSection, SpdeploySection,
};

const ACTION_ROW_HEIGHT: f32 = 28.0;
const ACTION_TEXT_SIZE: f32 = 12.0;
const DIRECTORY_ROW_HEIGHT: f32 = 24.0;
const DIRECTORY_TEXT_SIZE: f32 = 12.0;
const SECTION_HEADER_HEIGHT: f32 = 28.0;
const SECTION_LABEL_TEXT_SIZE: f32 = 11.0;
/// Horizontal center of the leading chevron inside a section header.
const CHEVRON_CENTER_INSET: f32 = 13.0;
/// Left edge of the section label, clear of the chevron.
const SECTION_LABEL_INSET: f32 = 25.0;
/// Left text inset shared by all full-width action and directory rows.
const ROW_TEXT_INSET: f32 = 10.0;
/// Horizontal margin around inset section content (controls, review details).
const SECTION_BODY_MARGIN: i8 = 8;
const SIDEBAR_TOOLBAR_HEIGHT: f32 = 36.0;
const TOOLBAR_ICON_SIZE: f32 = 28.0;
const FLOATING_ICON_INSET: f32 = 8.0;
const MAX_VIEWPORT_FRACTION: f32 = 0.50;
const RESIZE_GRAB_WIDTH: f32 = 6.0;
const START_ALL_TASKS_LABEL: &str = "Start all (new tabs)";
const RUN_SPDEPLOY_LABEL: &str = "Run in new tab";

/// Warning text while a manifest awaits an explicit trust decision.
const WARNING_TEXT_COLOR: Color32 = Color32::from_rgb(250, 204, 21);
/// Error text for section-level failures (design token: danger).
const ERROR_TEXT_COLOR: Color32 = Color32::from_rgb(248, 113, 113);

fn white_alpha(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, alpha)
}

/// 1px outline shared by the sidebar surface and the floating launcher.
fn surface_border_color() -> Color32 {
    white_alpha(35)
}

/// Hairline between accordion sections (design token: divider).
fn separator_color() -> Color32 {
    white_alpha(26)
}

/// Muted text: empty states, dropdown group headings, detail labels.
fn muted_text_color() -> Color32 {
    white_alpha(110)
}

#[derive(Default)]
pub(crate) struct ContextUi {
    /// Selected spdeploy operation per section. Discovery results are immutable;
    /// selection is ephemeral and resets when an operation disappears.
    selections: BTreeMap<String, SpdeploySelection>,
    /// True only while a contextual widget has focus or its dropdown is open.
    owns_keyboard: bool,
    /// Owns the divider gesture until the primary pointer is released. This
    /// prevents terminal or scroll content beneath the edge from stealing a
    /// resize after the initial press.
    sidebar_resize_active: bool,
    /// Transfers focus between the launcher and collapse button after the
    /// control changes identity with the sidebar state.
    focus_toggle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpdeploySelection {
    config_path: PathBuf,
    operation: String,
}

impl SpdeploySelection {
    fn new(operation: &crate::context_actions::SpdeployOperation) -> Self {
        Self {
            config_path: operation.config_path.clone(),
            operation: operation.name.clone(),
        }
    }

    fn matches(&self, operation: &crate::context_actions::SpdeployOperation) -> bool {
        self.config_path == operation.config_path && self.operation == operation.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SpdeployDropdownEntry {
    Heading(String),
    Operation {
        selection: SpdeploySelection,
        text: String,
    },
}

#[derive(Default)]
pub(crate) struct ContextUiOutcome {
    pub config_changed: bool,
    pub request: Option<ContextRequest>,
    pub rect: Option<egui::Rect>,
    pub reserved_width_points: f32,
    /// Right edge available to top-right terminal overlays. This is the
    /// sidebar's left edge when expanded and the launcher's left edge when
    /// collapsed, so overlays never obscure either control.
    pub find_overlay_right_edge_points: Option<f32>,
}

impl ContextUi {
    pub fn begin_frame(&mut self) {
        self.owns_keyboard = false;
    }

    pub fn owns_keyboard(&self) -> bool {
        self.owns_keyboard
    }

    #[cfg(test)]
    pub fn draw(
        &mut self,
        root_ui: &mut Ui,
        config: &mut AppConfig,
        snapshot: &ContextSnapshot,
        top_inset_points: f32,
        alpha: u8,
    ) -> ContextUiOutcome {
        self.draw_with_commands(root_ui, config, snapshot, &[], top_inset_points, alpha)
    }

    pub fn draw_with_commands(
        &mut self,
        root_ui: &mut Ui,
        config: &mut AppConfig,
        snapshot: &ContextSnapshot,
        frequent_commands: &[String],
        top_inset_points: f32,
        alpha: u8,
    ) -> ContextUiOutcome {
        let ctx = root_ui.ctx().clone();
        if !config.context_actions.enabled {
            return ContextUiOutcome::default();
        }

        let outcome = if config.context_actions.panel_collapsed {
            self.draw_collapsed(root_ui, config, top_inset_points, alpha)
        } else {
            self.draw_expanded(
                root_ui,
                config,
                snapshot,
                frequent_commands,
                top_inset_points,
                alpha,
            )
        };
        self.owns_keyboard =
            ctx.memory(|memory| memory.focused().is_some()) || egui::Popup::is_any_open(&ctx);
        outcome
    }

    fn draw_collapsed(
        &mut self,
        root_ui: &mut Ui,
        config: &mut AppConfig,
        top_inset_points: f32,
        alpha: u8,
    ) -> ContextUiOutcome {
        let available = root_ui.available_rect_before_wrap();
        let position = egui::pos2(
            available.right() - FLOATING_ICON_INSET,
            available.top() + top_inset_points.max(0.0) + FLOATING_ICON_INSET,
        );
        let panel = Area::new(Id::new("phantom_context_actions_launcher"))
            .order(Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(position)
            .show(root_ui.ctx(), |ui| {
                sidebar_frame(alpha, 4)
                    .corner_radius(4)
                    .show(ui, |ui| {
                        let launcher = context_icon_button(ui, true);
                        if self.focus_toggle {
                            launcher.request_focus();
                            self.focus_toggle = false;
                        }
                        if launcher.clicked() {
                            config.context_actions.panel_collapsed = false;
                            self.focus_toggle = true;
                            true
                        } else {
                            false
                        }
                    })
                    .inner
            });
        ContextUiOutcome {
            config_changed: panel.inner,
            request: None,
            rect: Some(panel.response.rect),
            reserved_width_points: 0.0,
            find_overlay_right_edge_points: Some(panel.response.rect.left() - FLOATING_ICON_INSET),
        }
    }

    fn draw_expanded(
        &mut self,
        root_ui: &mut Ui,
        config: &mut AppConfig,
        snapshot: &ContextSnapshot,
        frequent_commands: &[String],
        top_inset_points: f32,
        alpha: u8,
    ) -> ContextUiOutcome {
        let available = root_ui.available_rect_before_wrap();
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(
                available.left(),
                (available.top() + top_inset_points.max(0.0)).min(available.bottom()),
            ),
            available.max,
        );
        let viewport_width = content_rect.width();
        let max_width = (viewport_width * MAX_VIEWPORT_FRACTION).clamp(
            MIN_CONTEXT_SIDEBAR_WIDTH as f32,
            MAX_CONTEXT_SIDEBAR_WIDTH as f32,
        );
        let preferred_width = config.context_actions.sidebar_width as f32;
        let mut sidebar_width = preferred_width.clamp(MIN_CONTEXT_SIDEBAR_WIDTH as f32, max_width);
        let mut width_changed = false;
        let resize_id = Id::new("phantom_context_actions_resize");
        let accessibility_width = root_ui.input(|input| {
            use egui::accesskit::{Action, ActionData};

            let increment = input
                .accesskit_action_requests(resize_id, Action::Increment)
                .count() as f32;
            let decrement = input
                .accesskit_action_requests(resize_id, Action::Decrement)
                .count() as f32;
            let set_value = input
                .accesskit_action_requests(resize_id, Action::SetValue)
                .find_map(|request| match request.data {
                    Some(ActionData::NumericValue(value)) => Some(value as f32),
                    _ => None,
                });
            set_value.or_else(|| {
                let steps = increment - decrement;
                (steps != 0.0).then_some(sidebar_width + steps * 10.0)
            })
        });
        if let Some(requested_width) = accessibility_width {
            let requested_width =
                requested_width.clamp(MIN_CONTEXT_SIDEBAR_WIDTH as f32, max_width);
            width_changed = (requested_width - sidebar_width).abs() >= 0.5;
            sidebar_width = requested_width;
        }
        let initial_resize_rect = sidebar_resize_rect(content_rect, sidebar_width);
        let (pointer_pos, primary_pressed, primary_down) = root_ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.primary_pressed(),
                input.pointer.primary_down(),
            )
        });
        if primary_pressed
            && pointer_pos.is_some_and(|pointer| initial_resize_rect.contains(pointer))
        {
            self.sidebar_resize_active = true;
        }
        if self.sidebar_resize_active {
            if primary_down {
                if let Some(pointer) = pointer_pos {
                    let dragged_width = (content_rect.right() - pointer.x)
                        .clamp(MIN_CONTEXT_SIDEBAR_WIDTH as f32, max_width);
                    width_changed = (dragged_width - sidebar_width).abs() >= 0.5;
                    sidebar_width = dragged_width;
                }
            } else {
                self.sidebar_resize_active = false;
            }
        }

        let panel_rect = egui::Rect::from_min_max(
            egui::pos2(content_rect.right() - sidebar_width, content_rect.top()),
            content_rect.max,
        );
        let resize_rect = sidebar_resize_rect(content_rect, sidebar_width);
        let mut panel_ui = root_ui.new_child(
            UiBuilder::new()
                .id_salt("context_actions_content_area")
                .max_rect(panel_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        panel_ui.set_min_size(panel_rect.size());
        panel_ui.set_clip_rect(panel_rect);
        let panel = sidebar_frame(alpha, 0).show(&mut panel_ui, |ui| {
            ui.set_min_size(panel_rect.size());
            let mut outcome = ContextUiOutcome::default();
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), SIDEBAR_TOOLBAR_HEIGHT),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.add_space(4.0);
                    let collapse = context_icon_button(ui, false);
                    if self.focus_toggle {
                        collapse.request_focus();
                        self.focus_toggle = false;
                    }
                    if collapse.clicked() {
                        config.context_actions.panel_collapsed = true;
                        self.focus_toggle = true;
                        outcome.config_changed = true;
                    }
                },
            );

            egui::ScrollArea::vertical()
                .id_salt("context_actions_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let frequent_section =
                        (!frequent_commands.is_empty()).then(|| ContextSection {
                            id: FREQUENT_COMMANDS_PLUGIN_ID.to_string(),
                            title: "Frequent Commands".to_string(),
                            content: ContextSectionContent::FrequentCommands(
                                FrequentCommandsSection {
                                    commands: frequent_commands.to_vec(),
                                },
                            ),
                        });
                    let mut visible_sections: Vec<_> = snapshot
                        .sections
                        .iter()
                        .chain(frequent_section.iter())
                        .filter(|section| section_is_enabled(config, section))
                        .collect();
                    visible_sections.sort_by(|left, right| {
                        let left_order = config
                            .context_actions
                            .plugin(&left.id)
                            .map_or(u16::MAX, |plugin| plugin.order);
                        let right_order = config
                            .context_actions
                            .plugin(&right.id)
                            .map_or(u16::MAX, |plugin| plugin.order);
                        left_order.cmp(&right_order).then(left.id.cmp(&right.id))
                    });
                    for (index, section) in visible_sections.into_iter().enumerate() {
                        if index > 0 {
                            thin_separator(ui);
                        }
                        let section_outcome = self.draw_section(ui, config, section);
                        outcome.config_changed |= section_outcome.config_changed;
                        if outcome.request.is_none() {
                            outcome.request = section_outcome.request;
                        }
                    }
                });
            outcome
        });

        // Register the handle after panel contents so it wins hit-testing on
        // the shared boundary, including over scrollable section content.
        let resize = root_ui.interact(resize_rect, resize_id, Sense::drag());
        resize.widget_info(|| {
            let mut info = egui::WidgetInfo::new(egui::WidgetType::ResizeHandle);
            info.label = Some("Context sidebar width".to_string());
            info.value = Some(sidebar_width as f64);
            info
        });
        root_ui.ctx().accesskit_node_builder(resize.id, |node| {
            use egui::accesskit::Action;

            node.set_min_numeric_value(MIN_CONTEXT_SIDEBAR_WIDTH as f64);
            node.set_max_numeric_value(max_width as f64);
            node.set_numeric_value_step(10.0);
            node.add_action(Action::Decrement);
            node.add_action(Action::Increment);
            node.add_action(Action::SetValue);
        });
        if resize.hovered() || self.sidebar_resize_active {
            root_ui
                .ctx()
                .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        let handle_color = if self.sidebar_resize_active {
            root_ui.visuals().widgets.active.fg_stroke.color
        } else if resize.hovered() {
            root_ui.visuals().widgets.hovered.fg_stroke.color
        } else {
            surface_border_color()
        };
        root_ui.painter().vline(
            panel_rect.left(),
            panel_rect.y_range(),
            egui::Stroke::new(
                if resize.hovered() || self.sidebar_resize_active {
                    2.0
                } else {
                    1.0
                },
                handle_color,
            ),
        );

        let measured_width = sidebar_width.round().clamp(
            MIN_CONTEXT_SIDEBAR_WIDTH as f32,
            MAX_CONTEXT_SIDEBAR_WIDTH as f32,
        ) as u16;
        let mut outcome = panel.inner;
        if width_changed && measured_width != config.context_actions.sidebar_width {
            config.context_actions.sidebar_width = measured_width;
            outcome.config_changed = true;
        }
        ContextUiOutcome {
            rect: Some(panel_rect),
            reserved_width_points: sidebar_width,
            find_overlay_right_edge_points: Some(panel_rect.left() - FLOATING_ICON_INSET),
            ..outcome
        }
    }

    fn draw_section(
        &mut self,
        ui: &mut Ui,
        config: &mut AppConfig,
        section: &ContextSection,
    ) -> ContextUiOutcome {
        let collapsed = config
            .context_actions
            .plugin(&section.id)
            .is_some_and(|plugin| plugin.section_collapsed);
        let mut outcome = ContextUiOutcome::default();
        let (header, edit) = draw_section_header(ui, section, collapsed);
        let edit_clicked = edit.as_ref().is_some_and(egui::Response::clicked);
        if edit_clicked {
            outcome.request = edit_manifest_request(section);
        }
        if header.clicked() && !edit_clicked {
            if let Some(plugin) = config.context_actions.plugin_mut(&section.id) {
                plugin.section_collapsed = !plugin.section_collapsed;
                outcome.config_changed = true;
            }
        }
        if collapsed {
            return outcome;
        }

        let request = match &section.content {
            ContextSectionContent::Manifest(manifest) => draw_manifest(ui, manifest),
            ContextSectionContent::Spdeploy(spdeploy) => {
                self.draw_spdeploy(ui, &section.id, spdeploy)
            }
            ContextSectionContent::RecentDirectories(recent) => draw_recent_directories(ui, recent),
            ContextSectionContent::FrequentCommands(frequent) => {
                draw_frequent_commands(ui, frequent)
            }
            ContextSectionContent::ManifestError(error) => {
                draw_error(ui, &error.message);
                None
            }
            ContextSectionContent::Error { message } => {
                draw_error(ui, message);
                None
            }
        };
        if outcome.request.is_none() {
            outcome.request = request;
        }
        outcome
    }

    fn draw_spdeploy(
        &mut self,
        ui: &mut Ui,
        section_id: &str,
        spdeploy: &SpdeploySection,
    ) -> Option<ContextRequest> {
        if spdeploy.operations.is_empty() {
            section_body(ui, |ui| {
                ui.label(
                    RichText::new("No runnable operations")
                        .size(ACTION_TEXT_SIZE)
                        .color(muted_text_color()),
                );
            });
            return None;
        }

        let selected = self
            .selections
            .entry(section_id.to_string())
            .or_insert_with(|| SpdeploySelection::new(&spdeploy.operations[0]));
        if !spdeploy
            .operations
            .iter()
            .any(|operation| selected.matches(operation))
        {
            *selected = SpdeploySelection::new(&spdeploy.operations[0]);
        }
        let entries = spdeploy_dropdown_entries(spdeploy);
        section_body(ui, |ui| {
            spdeploy_dropdown(ui, ("spdeploy_operation", section_id), selected, &entries);
        });
        let operation = spdeploy
            .operations
            .iter()
            .find(|operation| selected.matches(operation))?;
        ui.push_id(
            (
                "run_spdeploy",
                section_id,
                &operation.config_path,
                &operation.name,
            ),
            |ui| action_row(ui, RUN_SPDEPLOY_LABEL),
        )
        .inner
        .clicked()
        .then(|| ContextRequest::RunSpdeploy {
            config_path: operation.config_path.clone(),
            operation: operation.name.clone(),
        })
    }
}

fn draw_manifest(ui: &mut Ui, manifest: &ManifestSection) -> Option<ContextRequest> {
    if manifest.trust == ManifestTrustState::Trusted {
        return draw_trusted_manifest(ui, manifest);
    }

    section_body(ui, |ui| draw_manifest_review(ui, manifest))
}

/// Fixed 28px header row: leading chevron, plugin label, and at most one
/// right-aligned icon-only management action that never toggles the accordion.
fn draw_section_header(
    ui: &mut Ui,
    section: &ContextSection,
    collapsed: bool,
) -> (egui::Response, Option<egui::Response>) {
    let (_, header_rect) =
        ui.allocate_space(egui::vec2(ui.available_width(), SECTION_HEADER_HEIGHT));
    let edit_rect = matches!(
        section.content,
        ContextSectionContent::Manifest(_) | ContextSectionContent::ManifestError(_)
    )
    .then(|| {
        egui::Rect::from_min_max(
            egui::pos2(
                header_rect.right() - SECTION_HEADER_HEIGHT,
                header_rect.top(),
            ),
            header_rect.right_bottom(),
        )
    });
    let toggle_rect = edit_rect.map_or(header_rect, |rect| {
        egui::Rect::from_min_max(
            header_rect.min,
            egui::pos2(rect.left(), header_rect.bottom()),
        )
    });
    let header = ui
        .interact(
            toggle_rect,
            Id::new(("context_section_toggle", section.id.as_str())),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    header.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::CollapsingHeader,
            true,
            section_label(section),
        )
    });
    ui.ctx()
        .accesskit_node_builder(header.id, |node| node.set_expanded(!collapsed));
    let header_visuals = ui.style().interact(&header);
    if header.hovered() || header.has_focus() {
        ui.painter()
            .rect_filled(toggle_rect, 0.0, header_visuals.weak_bg_fill);
    }
    draw_section_chevron(ui, header_rect, !collapsed, header_visuals.fg_stroke.color);
    ui.painter().text(
        egui::pos2(
            header_rect.left() + SECTION_LABEL_INSET,
            header_rect.center().y,
        ),
        egui::Align2::LEFT_CENTER,
        section_label(section),
        FontId::proportional(SECTION_LABEL_TEXT_SIZE),
        header_visuals.fg_stroke.color,
    );
    let edit = edit_rect.map(|rect| {
        let response = ui
            .interact(
                rect,
                Id::new(("context_section_edit", section.id.as_str())),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Edit .phantom.yml")
        });
        draw_ellipsis_icon(ui, rect, &response);
        response.on_hover_text("Edit .phantom.yml")
    });
    (header, edit)
}

/// Inset wrapper for section content that is not a full-width action row:
/// controls, empty states, errors, and trust-review details.
fn section_body<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .inner_margin(Margin {
            left: SECTION_BODY_MARGIN,
            right: SECTION_BODY_MARGIN,
            top: 4,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            add_contents(ui)
        })
        .inner
}

/// Full-width fixed-height action row with the shared text inset and hover fill.
fn action_row(ui: &mut Ui, label: &str) -> egui::Response {
    let response = full_width_row(ui, Sense::click(), |ui, rect, color| {
        ui.painter().text(
            egui::pos2(rect.left() + ROW_TEXT_INSET, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(ACTION_TEXT_SIZE),
            color,
        );
    });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response
}

/// Keep the resize gesture on the sidebar side of the divider. The terminal
/// side owns its full pointer area, including the scrollbar hit corridor.
fn sidebar_resize_rect(content_rect: egui::Rect, sidebar_width: f32) -> egui::Rect {
    let divider_x = content_rect.right() - sidebar_width;
    egui::Rect::from_min_max(
        egui::pos2(divider_x, content_rect.top()),
        egui::pos2(
            (divider_x + RESIZE_GRAB_WIDTH).min(content_rect.right()),
            content_rect.bottom(),
        ),
    )
}

fn spdeploy_dropdown_entries(spdeploy: &SpdeploySection) -> Vec<SpdeployDropdownEntry> {
    let mut entries = Vec::new();
    for operation in spdeploy
        .operations
        .iter()
        .filter(|operation| operation.breadcrumbs.is_empty())
    {
        entries.push(spdeploy_operation_entry(operation));
    }

    let mut groups = Vec::<Vec<String>>::new();
    for operation in spdeploy
        .operations
        .iter()
        .filter(|operation| !operation.breadcrumbs.is_empty())
    {
        if !groups.contains(&operation.breadcrumbs) {
            groups.push(operation.breadcrumbs.clone());
        }
    }
    for group in groups {
        entries.push(SpdeployDropdownEntry::Heading(group.join(" / ")));
        for operation in spdeploy
            .operations
            .iter()
            .filter(|operation| operation.breadcrumbs == group)
        {
            entries.push(spdeploy_operation_entry(operation));
        }
    }
    entries
}

fn spdeploy_operation_entry(
    operation: &crate::context_actions::SpdeployOperation,
) -> SpdeployDropdownEntry {
    let text = operation.description.as_ref().map_or_else(
        || operation.name.clone(),
        |description| format!("{}: {description}", operation.name),
    );
    SpdeployDropdownEntry::Operation {
        selection: SpdeploySelection::new(operation),
        text,
    }
}

fn draw_recent_directories(
    ui: &mut Ui,
    recent: &RecentDirectoriesSection,
) -> Option<ContextRequest> {
    let mut request = None;
    let home = default_home_dir();
    for directory in &recent.directories {
        let path = display_directory_path(&directory.path, home.as_deref().map(Path::new));
        let text = elide_path_start(
            ui,
            &path,
            (ui.available_width() - 2.0 * ROW_TEXT_INSET).max(0.0),
        );
        let elided = text != path;
        let mut response = ui
            .push_id(("recent_directory", &directory.path), |ui| {
                full_width_row_with_height(
                    ui,
                    DIRECTORY_ROW_HEIGHT,
                    Sense::click(),
                    |ui, rect, color| {
                        ui.painter().text(
                            egui::pos2(rect.left() + ROW_TEXT_INSET, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            text,
                            FontId::monospace(DIRECTORY_TEXT_SIZE),
                            color,
                        );
                    },
                )
            })
            .inner;
        if elided {
            response = response.on_hover_text(&path);
        }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &path));
        if response.clicked() && request.is_none() {
            let target = directory_target(ui.input(|input| input.modifiers.shift));
            request = Some(ContextRequest::OpenDirectory {
                path: directory.path.clone(),
                target,
            });
        }
    }
    request
}

fn draw_frequent_commands(
    ui: &mut Ui,
    frequent: &FrequentCommandsSection,
) -> Option<ContextRequest> {
    let mut request = None;
    for command in &frequent.commands {
        let text = elide_text_end(
            ui,
            command,
            (ui.available_width() - 2.0 * ROW_TEXT_INSET).max(0.0),
        );
        let elided = text != *command;
        let mut response = ui
            .push_id(("frequent_command", command), |ui| {
                full_width_row(ui, Sense::click(), |ui, rect, color| {
                    ui.painter().text(
                        egui::pos2(rect.left() + ROW_TEXT_INSET, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        text,
                        FontId::monospace(DIRECTORY_TEXT_SIZE),
                        color,
                    );
                })
            })
            .inner;
        if elided {
            response = response.on_hover_text(command);
        }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, command));
        if response.clicked() && request.is_none() {
            request = Some(ContextRequest::RunFrequentCommand {
                command: command.clone(),
            });
        }
    }
    request
}

fn directory_target(shift: bool) -> DirectoryTarget {
    if shift {
        DirectoryTarget::NewTab
    } else {
        DirectoryTarget::CurrentTab
    }
}

fn full_width_row(
    ui: &mut Ui,
    sense: Sense,
    paint: impl FnOnce(&Ui, egui::Rect, Color32),
) -> egui::Response {
    full_width_row_with_height(ui, ACTION_ROW_HEIGHT, sense, paint)
}

fn full_width_row_with_height(
    ui: &mut Ui,
    height: f32,
    sense: Sense,
    paint: impl FnOnce(&Ui, egui::Rect, Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), sense);
    let visuals = ui.style().interact(&response);
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 0.0, visuals.weak_bg_fill);
    }
    paint(ui, rect, visuals.fg_stroke.color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn section_label(section: &ContextSection) -> &str {
    match section.id.as_str() {
        MANIFEST_PLUGIN_ID => "Tasks",
        SPDEPLOY_PLUGIN_ID => "Deploy",
        RECENT_DIRECTORIES_PLUGIN_ID => "Directories",
        FREQUENT_COMMANDS_PLUGIN_ID => "Frequent Commands",
        _ => &section.title,
    }
}

fn draw_section_chevron(ui: &Ui, rect: egui::Rect, expanded: bool, color: Color32) {
    let center = egui::pos2(rect.left() + CHEVRON_CENTER_INSET, rect.center().y);
    let points = if expanded {
        [
            egui::pos2(center.x - 3.5, center.y - 2.0),
            egui::pos2(center.x, center.y + 2.0),
            egui::pos2(center.x + 3.5, center.y - 2.0),
        ]
    } else {
        [
            egui::pos2(center.x - 2.0, center.y - 3.5),
            egui::pos2(center.x + 2.0, center.y),
            egui::pos2(center.x - 2.0, center.y + 3.5),
        ]
    };
    ui.painter()
        .line(points.to_vec(), egui::Stroke::new(1.2, color));
}

fn draw_ellipsis_icon(ui: &Ui, rect: egui::Rect, response: &egui::Response) {
    let visuals = ui.style().interact(response);
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 0.0, visuals.weak_bg_fill);
    }
    let center = rect.center();
    for offset in [-5.0, 0.0, 5.0] {
        ui.painter().circle_filled(
            egui::pos2(center.x + offset, center.y),
            1.4,
            visuals.fg_stroke.color,
        );
    }
}

fn edit_manifest_request(section: &ContextSection) -> Option<ContextRequest> {
    let (root, manifest_source) = match &section.content {
        ContextSectionContent::Manifest(manifest) => (&manifest.root, &manifest.manifest_source),
        ContextSectionContent::ManifestError(error) => (&error.root, &error.manifest_source),
        _ => return None,
    };
    Some(ContextRequest::EditManifest {
        root: root.clone(),
        manifest_source: manifest_source.clone(),
    })
}

fn draw_error(ui: &mut Ui, message: &str) {
    section_body(ui, |ui| {
        ui.colored_label(ERROR_TEXT_COLOR, message);
    });
}

fn thin_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, separator_color()),
    );
}

fn display_directory_path(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.display().to_string();
    };
    let Ok(relative) = path.strip_prefix(home) else {
        return path.display().to_string();
    };
    if relative.as_os_str().is_empty() {
        "~".to_string()
    } else {
        format!("~/{}", relative.display())
    }
}

fn text_fits(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> bool {
    ui.painter()
        .layout_no_wrap(text.to_string(), font.clone(), Color32::PLACEHOLDER)
        .size()
        .x
        <= max_width
}

#[derive(Clone, Copy)]
enum Elide {
    /// Drop the start of the text, keeping its ending visible.
    Start,
    /// Drop the end of the text, keeping its beginning visible.
    End,
}

/// Binary-search the longest prefix/suffix that fits within `max_width`.
fn elide_to_width(ui: &Ui, text: &str, max_width: f32, elide: Elide) -> String {
    let font = FontId::monospace(DIRECTORY_TEXT_SIZE);
    if text_fits(ui, text, &font, max_width) {
        return text.to_string();
    }

    let characters: Vec<char> = text.chars().collect();
    let rendered = |keep: usize| match elide {
        Elide::Start => format!(
            "…{}",
            characters[characters.len() - keep..]
                .iter()
                .collect::<String>()
        ),
        Elide::End => format!("{}…", characters[..keep].iter().collect::<String>()),
    };
    let mut low = 0;
    let mut high = characters.len();
    while low < high {
        let keep = (low + high).div_ceil(2);
        if text_fits(ui, &rendered(keep), &font, max_width) {
            low = keep;
        } else {
            high = keep - 1;
        }
    }
    rendered(low)
}

fn elide_path_start(ui: &Ui, path: &str, max_width: f32) -> String {
    elide_to_width(ui, path, max_width, Elide::Start)
}

fn elide_text_end(ui: &Ui, text: &str, max_width: f32) -> String {
    elide_to_width(ui, text, max_width, Elide::End)
}

fn sidebar_frame(alpha: u8, margin: i8) -> Frame {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(17, 17, 22, alpha))
        .stroke(egui::Stroke::new(1.0, surface_border_color()))
        .inner_margin(Margin::same(margin))
}

fn section_is_enabled(config: &AppConfig, section: &ContextSection) -> bool {
    config
        .context_actions
        .plugin(&section.id)
        .is_some_and(|plugin| plugin.enabled)
}

fn context_icon_button(ui: &mut Ui, open: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(TOOLBAR_ICON_SIZE, TOOLBAR_ICON_SIZE),
        Sense::click(),
    );
    let visuals = ui.style().interact(&response);
    let accessible_label = if open {
        "Open context actions"
    } else {
        "Collapse context actions"
    };
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, accessible_label)
    });
    ui.painter().rect_filled(rect, 4.0, visuals.weak_bg_fill);
    ui.painter()
        .rect_stroke(rect, 4.0, visuals.bg_stroke, egui::StrokeKind::Inside);
    let color = visuals.fg_stroke.color;
    let center = rect.center();
    if open {
        // Draw the icon so it remains available on minimal font installations.
        for (offset, knob) in [(-6.0, -3.0), (0.0, 3.0), (6.0, -1.0)] {
            let y = center.y + offset;
            ui.painter().line_segment(
                [egui::pos2(center.x - 7.0, y), egui::pos2(center.x + 7.0, y)],
                egui::Stroke::new(1.3, color),
            );
            ui.painter()
                .circle_filled(egui::pos2(center.x + knob, y), 2.0, color);
        }
    } else {
        ui.painter().line_segment(
            [
                egui::pos2(center.x - 6.0, center.y),
                egui::pos2(center.x + 6.0, center.y),
            ],
            egui::Stroke::new(1.5, color),
        );
    }
    response.on_hover_text(accessible_label)
}

fn draw_manifest_review(ui: &mut Ui, manifest: &ManifestSection) -> Option<ContextRequest> {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.colored_label(WARNING_TEXT_COLOR, "Review before trusting:");
    for tab in &manifest.tabs {
        draw_manifest_tab_summary(ui, tab, &manifest.root);
    }
    ui.add_space(2.0);
    ui.button("Trust project tasks")
        .clicked()
        .then(|| ContextRequest::TrustManifest {
            root: manifest.root.clone(),
            manifest_source: manifest.manifest_source.clone(),
        })
}

/// One compact line per task: title, the exact command (env prefix, quoted
/// program and args), and the working directory only when it is not the
/// project root. Everything executable stays visible for the trust decision.
fn draw_manifest_tab_summary(ui: &mut Ui, tab: &ManifestTab, root: &Path) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(&tab.title).size(11.0).strong());
        match manifest_task_command(tab) {
            Some(command) => ui.label(RichText::new(command).monospace().size(11.0)),
            None => ui.label(
                RichText::new("shell only")
                    .size(11.0)
                    .color(muted_text_color()),
            ),
        };
        if let Some(location) = manifest_task_location(tab, root) {
            ui.label(RichText::new(location).size(11.0).color(muted_text_color()));
        }
    });
}

/// Shell-style single-line rendering of the task: `KEY=val program args…`.
/// Returns `None` for shell-only tabs.
fn manifest_task_command(tab: &ManifestTab) -> Option<String> {
    let task = tab.task.as_ref()?;
    let mut tokens: Vec<String> = task
        .env
        .iter()
        .map(|(key, value)| format!("{key}={}", quote_token(value)))
        .collect();
    tokens.push(quote_token(&task.program));
    tokens.extend(task.args.iter().map(|arg| quote_token(arg)));
    Some(tokens.join(" "))
}

/// Quote a command token exactly (Rust debug escapes) unless it is plain
/// enough to be unambiguous on a space-separated line.
fn quote_token(token: &str) -> String {
    let plain = !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/:=@%+,".contains(c));
    if plain {
        token.to_string()
    } else {
        format!("{token:?}")
    }
}

/// Where the task runs, shown only when it differs from the project root. A
/// cwd outside the root keeps its full path so it cannot hide from review.
fn manifest_task_location(tab: &ManifestTab, root: &Path) -> Option<String> {
    let relative = tab.cwd.strip_prefix(root).unwrap_or(&tab.cwd);
    if relative.as_os_str().is_empty() || relative == Path::new(".") {
        return None;
    }
    Some(format!("in {}", relative.display()))
}

fn draw_trusted_manifest(ui: &mut Ui, manifest: &ManifestSection) -> Option<ContextRequest> {
    let mut request = ui
        .push_id(("manifest_all", &manifest.root), |ui| {
            action_row(ui, START_ALL_TASKS_LABEL)
        })
        .inner
        .clicked()
        .then(|| ContextRequest::OpenManifestAll {
            root: manifest.root.clone(),
            manifest_source: manifest.manifest_source.clone(),
        });
    for tab in &manifest.tabs {
        let row = ui
            .push_id(("manifest_tab", &tab.id), |ui| {
                action_row(ui, &task_start_label(&tab.title))
            })
            .inner;
        if row.clicked() && request.is_none() {
            request = Some(ContextRequest::OpenManifestTab {
                root: manifest.root.clone(),
                manifest_source: manifest.manifest_source.clone(),
                tab_id: tab.id.clone(),
            });
        }
    }
    request
}

fn task_start_label(title: &str) -> String {
    format!("Start {title}")
}

fn spdeploy_dropdown(
    ui: &mut Ui,
    id_salt: impl egui::AsIdSalt,
    value: &mut SpdeploySelection,
    entries: &[SpdeployDropdownEntry],
) {
    let selected_text = entries
        .iter()
        .find_map(|entry| match entry {
            SpdeployDropdownEntry::Operation { selection, text } if selection == value => {
                Some(text.as_str())
            }
            _ => None,
        })
        .unwrap_or(value.operation.as_str())
        .to_string();
    let width = ui.available_width();
    let button_font = egui::TextStyle::Button.resolve(ui.style());
    // Reserve room for the combo's chevron icon when deciding if a tooltip is
    // needed for the truncated selection.
    let selected_fits = text_fits(ui, &selected_text, &button_font, width - 24.0);
    // Truncate instead of wrapping so the closed control keeps one fixed
    // height regardless of the selected operation or sidebar width.
    let combo = ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text.as_str())
        .width(width)
        .truncate()
        .show_ui(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for entry in entries {
                match entry {
                    SpdeployDropdownEntry::Heading(heading) => {
                        ui.add_space(3.0);
                        ui.label(
                            RichText::new(heading)
                                .size(10.0)
                                .strong()
                                .color(muted_text_color()),
                        );
                    }
                    SpdeployDropdownEntry::Operation { selection, text } => {
                        let fits = text_fits(ui, text, &button_font, ui.available_width());
                        let mut response = ui.selectable_value(value, selection.clone(), text);
                        if !fits {
                            response = response.on_hover_text(text);
                        }
                        let _ = response;
                    }
                }
            }
        });
    combo.response.widget_info(|| {
        let mut info = egui::WidgetInfo::new(egui::WidgetType::ComboBox);
        info.label = Some("Deploy operation".to_string());
        info.current_text_value = Some(selected_text.clone());
        info
    });
    if !selected_fits {
        combo.response.on_hover_text(selected_text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{
        accesskit::{Action, Role},
        Context, Id,
    };
    use phantom_core::{MANIFEST_PLUGIN_ID, RECENT_DIRECTORIES_PLUGIN_ID, SPDEPLOY_PLUGIN_ID};

    use crate::context_actions::{
        ContextSection, ContextSectionContent, ManifestTask, SpdeployOperation,
    };

    fn snapshot() -> ContextSnapshot {
        ContextSnapshot {
            cwd: "/projects/soulfire".into(),
            sections: vec![
                ContextSection {
                    id: MANIFEST_PLUGIN_ID.to_string(),
                    title: "Soulfire workspace".to_string(),
                    content: ContextSectionContent::Manifest(ManifestSection {
                        manifest_path: "/projects/soulfire/.phantom.yml".into(),
                        root: "/projects/soulfire".into(),
                        manifest_source: "version: 1\nname: Soulfire\n".to_string(),
                        name: "Soulfire".to_string(),
                        trust: ManifestTrustState::NeedsTrust,
                        tabs: vec![ManifestTab {
                            id: "api".to_string(),
                            title: "Soulfire API".to_string(),
                            cwd: "/projects/soulfire/soulfire-api".into(),
                            task: Some(ManifestTask {
                                program: "cargo".to_string(),
                                args: vec!["run".to_string()],
                                env: vec![(
                                    "RUST_LOG".to_string(),
                                    "info,soulfire=debug".to_string(),
                                )],
                            }),
                        }],
                    }),
                },
                ContextSection {
                    id: SPDEPLOY_PLUGIN_ID.to_string(),
                    title: "spdeploy".to_string(),
                    content: ContextSectionContent::Spdeploy(SpdeploySection {
                        config_path: "/projects/soulfire/deploy.yml".into(),
                        project_name: "Soulfire".to_string(),
                        operations: vec![SpdeployOperation {
                            name: "deploy".to_string(),
                            breadcrumbs: Vec::new(),
                            description: Some("Deploy Soulfire".to_string()),
                            config_path: "/projects/soulfire/deploy.yml".into(),
                        }],
                        config_sources: Vec::new(),
                    }),
                },
            ],
        }
    }

    fn run_frame(ctx: &Context, state: &mut ContextUi, config: &mut AppConfig) -> (usize, bool) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        state.begin_frame();
        let output = ctx.run_ui(input, |ui| {
            state.draw(ui, config, &snapshot(), 42.0, 220);
        });
        let focused = ctx.memory(|memory| memory.focused().is_some());
        (output.shapes.len(), focused)
    }

    #[test]
    fn idle_overlay_draws_without_owning_keyboard() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();

        let (shape_count, focused) = run_frame(&ctx, &mut state, &mut config);

        assert!(shape_count > 0);
        assert!(!focused);
        assert!(!state.owns_keyboard());
    }

    #[test]
    fn expanded_sidebar_exposes_controls_values_and_expanded_state() {
        let ctx = Context::default();
        ctx.enable_accesskit();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };

        let output = ctx.run_ui(input, |ui| {
            state.draw(ui, &mut config, &snapshot(), 42.0, 220);
        });
        let update = output.platform_output.accesskit_update.unwrap();

        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::Button && node.label() == Some("Collapse context actions")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::Button
                && node.label() == Some("Tasks")
                && node.is_expanded() == Some(true)
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == Role::Splitter
                && node.label() == Some("Context sidebar width")
                && node.numeric_value() == Some(260.0)
                && node.supports_action(Action::Increment)
                && node.supports_action(Action::Decrement)
                && node.supports_action(Action::SetValue)
        }));
    }

    #[test]
    fn sidebar_resize_handles_accesskit_increment_actions() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            events: vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: Action::Increment,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: Id::new("phantom_context_actions_resize").accesskit_id(),
                    data: None,
                },
            )],
            ..Default::default()
        };

        let mut outcome = None;
        let _ = ctx.run_ui(input, |ui| {
            outcome = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
        });

        assert_eq!(config.context_actions.sidebar_width, 270);
        assert!(outcome.unwrap().config_changed);
    }

    #[test]
    fn sidebar_width_defaults_to_a_compact_desktop_proportion() {
        let config = AppConfig::default();
        assert_eq!(config.context_actions.sidebar_width, 260);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn expanded_sidebar_starts_below_chrome_and_reports_reserved_width() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let mut outcome = None;

        let _ = ctx.run_ui(input, |ui| {
            outcome = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
        });

        let outcome = outcome.unwrap();
        assert!((outcome.reserved_width_points - 260.0).abs() <= 1.0);
        let rect = outcome.rect.unwrap();
        assert_eq!(
            outcome.find_overlay_right_edge_points,
            Some(rect.left() - FLOATING_ICON_INSET)
        );
        assert!((rect.top() - 42.0).abs() <= 1.0);
        assert!((rect.bottom() - 800.0).abs() <= 1.0);
        assert!((rect.height() - 758.0).abs() <= 1.0);
    }

    #[test]
    fn sidebar_resize_hitbox_stays_entirely_inside_sidebar() {
        let content = egui::Rect::from_min_max(egui::pos2(0.0, 42.0), egui::pos2(1280.0, 800.0));
        let sidebar_width = 260.0;
        let divider_x = content.right() - sidebar_width;

        let hitbox = sidebar_resize_rect(content, sidebar_width);

        assert_eq!(hitbox.left(), divider_x);
        assert_eq!(hitbox.right(), divider_x + RESIZE_GRAB_WIDTH);
        assert!(!hitbox.contains(egui::pos2(divider_x - 0.5, 120.0)));
        assert!(hitbox.contains(egui::pos2(divider_x, 120.0)));
    }

    #[test]
    fn viewport_clamp_does_not_rewrite_the_persisted_preferred_width() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        config.context_actions.sidebar_width = 900;
        let mut state = ContextUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let mut outcome = None;

        let _ = ctx.run_ui(input, |ui| {
            outcome = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
        });

        let outcome = outcome.unwrap();
        assert_eq!(outcome.reserved_width_points, 600.0);
        assert_eq!(config.context_actions.sidebar_width, 900);
        assert!(!outcome.config_changed);
    }

    #[test]
    fn maximize_restore_layout_reaches_stable_widths_without_config_feedback() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        config.context_actions.sidebar_width = 900;
        let mut state = ContextUi::default();
        let mut draw_at_width = |width: f32| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 800.0),
                )),
                ..Default::default()
            };
            let mut outcome = None;
            let _ = ctx.run_ui(input, |ui| {
                outcome = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
            });
            outcome.unwrap()
        };

        let maximized = draw_at_width(2000.0);
        assert_eq!(maximized.reserved_width_points, 900.0);
        assert!(!maximized.config_changed);
        let restored = draw_at_width(1200.0);
        assert_eq!(restored.reserved_width_points, 600.0);
        assert!(!restored.config_changed);
        let maximized_again = draw_at_width(2000.0);
        assert_eq!(maximized_again.reserved_width_points, 900.0);
        assert!(!maximized_again.config_changed);
        assert_eq!(config.context_actions.sidebar_width, 900);
    }

    #[test]
    fn untrusted_manifest_review_does_not_trust_without_explicit_click() {
        let ctx = Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let snapshot = snapshot();
        let ContextSectionContent::Manifest(manifest) = &snapshot.sections[0].content else {
            panic!("expected manifest section");
        };
        let mut request = None;

        let _ = ctx.run_ui(input, |ui| {
            request = draw_manifest_review(ui, manifest);
        });

        assert!(request.is_none());
    }

    #[test]
    fn open_popup_claims_keyboard() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        egui::Popup::open_id(&ctx, Id::new("context_test_popup"));

        run_frame(&ctx, &mut state, &mut config);

        assert!(state.owns_keyboard());
    }

    #[test]
    fn selection_recovers_when_operations_change() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        state.selections.insert(
            SPDEPLOY_PLUGIN_ID.to_string(),
            SpdeploySelection {
                config_path: "/removed/deploy.yml".into(),
                operation: "removed".to_string(),
            },
        );

        run_frame(&ctx, &mut state, &mut config);

        assert_eq!(
            state.selections.get(SPDEPLOY_PLUGIN_ID),
            Some(&SpdeploySelection {
                config_path: "/projects/soulfire/deploy.yml".into(),
                operation: "deploy".to_string(),
            })
        );
    }

    #[test]
    fn collapsed_provider_does_not_build_controls() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        config
            .context_actions
            .plugin_mut(SPDEPLOY_PLUGIN_ID)
            .unwrap()
            .section_collapsed = true;
        let mut state = ContextUi::default();

        run_frame(&ctx, &mut state, &mut config);

        assert!(!state.selections.contains_key(SPDEPLOY_PLUGIN_ID));
    }

    #[test]
    fn cached_section_is_hidden_immediately_when_provider_is_disabled() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        config
            .context_actions
            .plugin_mut(SPDEPLOY_PLUGIN_ID)
            .unwrap()
            .enabled = false;
        let mut cached = snapshot();
        cached
            .sections
            .retain(|section| section.id == SPDEPLOY_PLUGIN_ID);
        let mut state = ContextUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let mut outcome = None;

        let _ = ctx.run_ui(input, |ui| {
            state.begin_frame();
            outcome = Some(state.draw(ui, &mut config, &cached, 42.0, 220));
        });

        let outcome = outcome.unwrap();
        assert!(outcome.rect.is_some());
        assert!(outcome.reserved_width_points > 0.0);
        assert!(outcome.request.is_none());
        assert_eq!(config.context_actions.sidebar_width, 260);
        assert!(!state.selections.contains_key(SPDEPLOY_PLUGIN_ID));
        assert!(!state.owns_keyboard());
    }

    #[test]
    fn collapsed_overlay_remains_non_modal() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        config.context_actions.panel_collapsed = true;
        let mut state = ContextUi::default();

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let mut outcome = None;
        state.begin_frame();
        let output = ctx.run_ui(input, |ui| {
            outcome = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
        });
        let focused = ctx.memory(|memory| memory.focused().is_some());
        let outcome = outcome.unwrap();

        assert!(!output.shapes.is_empty());
        assert_eq!(outcome.reserved_width_points, 0.0);
        let rect = outcome.rect.unwrap();
        assert_eq!(
            outcome.find_overlay_right_edge_points,
            Some(rect.left() - FLOATING_ICON_INSET)
        );
        assert!(rect.top() >= 50.0);
        assert!(rect.width() <= 40.0);
        assert!(rect.height() <= 40.0);
        assert!(!focused);
        assert!(config.context_actions.panel_collapsed);
        assert!(!state.owns_keyboard());
    }

    #[test]
    fn empty_snapshot_keeps_open_sidebar() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let mut outcome = None;

        let _ = ctx.run_ui(input, |ui| {
            outcome = Some(state.draw(
                ui,
                &mut config,
                &ContextSnapshot::empty("/empty".into()),
                42.0,
                220,
            ));
        });

        let outcome = outcome.unwrap();
        assert!(outcome.rect.is_some());
        assert!(outcome.reserved_width_points > 0.0);
    }

    #[test]
    fn empty_snapshot_keeps_floating_closed_icon() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        config.context_actions.panel_collapsed = true;
        let mut state = ContextUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let mut outcome = None;

        let _ = ctx.run_ui(input, |ui| {
            outcome = Some(state.draw(
                ui,
                &mut config,
                &ContextSnapshot::empty("/empty".into()),
                42.0,
                220,
            ));
        });

        let outcome = outcome.unwrap();
        assert!(outcome.rect.is_some());
        assert_eq!(outcome.reserved_width_points, 0.0);
    }

    #[test]
    fn directory_click_modifier_selects_target() {
        assert_eq!(directory_target(false), DirectoryTarget::CurrentTab);
        assert_eq!(directory_target(true), DirectoryTarget::NewTab);
    }

    #[test]
    fn long_directory_paths_elide_from_the_start() {
        let ctx = Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(320.0, 100.0),
            )),
            ..Default::default()
        };
        let mut text = None;
        let _ = ctx.run_ui(input, |ui| {
            text = Some(elide_path_start(
                ui,
                "/projects/very/long/application/directory/soulfire-ui",
                100.0,
            ));
        });

        let text = text.unwrap();
        assert!(text.starts_with('…'));
        assert!(text.ends_with("soulfire-ui"));
    }

    #[test]
    fn long_commands_elide_from_the_end() {
        let ctx = Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(320.0, 100.0),
            )),
            ..Default::default()
        };
        let mut text = None;
        let _ = ctx.run_ui(input, |ui| {
            text = Some(elide_text_end(
                ui,
                "cargo run --release --features full-stack-orchestration",
                100.0,
            ));
        });

        let text = text.unwrap();
        assert!(text.starts_with("cargo"));
        assert!(text.ends_with('…'));
    }

    fn closed_dropdown_height(ctx: &Context, text: &str) -> f32 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(300.0, 600.0),
            )),
            ..Default::default()
        };
        let mut height = 0.0;
        let _ = ctx.run_ui(input, |ui| {
            ui.allocate_ui(egui::vec2(220.0, 600.0), |ui| {
                let mut value = SpdeploySelection {
                    config_path: "/projects/soulfire/deploy.yml".into(),
                    operation: "deploy".to_string(),
                };
                let entries = vec![SpdeployDropdownEntry::Operation {
                    selection: value.clone(),
                    text: text.to_string(),
                }];
                spdeploy_dropdown(ui, "dropdown_height_probe", &mut value, &entries);
                height = ui.min_rect().height();
            });
        });
        height
    }

    #[test]
    fn closed_dropdown_keeps_one_fixed_height_for_long_operation_text() {
        let ctx = Context::default();

        let short = closed_dropdown_height(&ctx, "deploy");
        let long = closed_dropdown_height(
            &ctx,
            "deploy: a very long operation description that would otherwise wrap \
             onto several lines inside a narrow sidebar and change the row height",
        );

        assert!(short > 0.0);
        assert_eq!(short, long);
    }

    #[test]
    fn directory_paths_use_a_home_relative_label_only_for_the_exact_home_prefix() {
        let home = Path::new("/Users/steve");

        assert_eq!(display_directory_path(home, Some(home)), "~");
        assert_eq!(
            display_directory_path(Path::new("/Users/steve/projects/soulfire"), Some(home)),
            "~/projects/soulfire"
        );
        assert_eq!(
            display_directory_path(Path::new("/Users/steven/projects"), Some(home)),
            "/Users/steven/projects"
        );
    }

    #[test]
    fn directory_rows_are_dense_without_shrinking_their_text() {
        assert_eq!(DIRECTORY_ROW_HEIGHT, 24.0);
        assert_eq!(DIRECTORY_TEXT_SIZE, 12.0);
        assert_eq!(DIRECTORY_ROW_HEIGHT * 7.0, 168.0);
    }

    #[test]
    fn recent_directory_section_is_a_registered_provider() {
        let config = AppConfig::default();
        assert!(config
            .context_actions
            .plugin(RECENT_DIRECTORIES_PLUGIN_ID)
            .is_some());
    }

    #[test]
    fn built_in_section_labels_are_compact_plugin_names() {
        let snapshot = snapshot();
        assert_eq!(section_label(&snapshot.sections[0]), "Tasks");
        assert_eq!(section_label(&snapshot.sections[1]), "Deploy");
        let directories = ContextSection {
            id: RECENT_DIRECTORIES_PLUGIN_ID.to_string(),
            title: "Wrong dynamic title".to_string(),
            content: ContextSectionContent::RecentDirectories(RecentDirectoriesSection {
                directories: Vec::new(),
            }),
        };
        assert_eq!(section_label(&directories), "Directories");
    }

    #[test]
    fn tasks_header_edit_request_uses_the_discovered_manifest_identity() {
        let snapshot = snapshot();

        assert_eq!(
            edit_manifest_request(&snapshot.sections[0]),
            Some(ContextRequest::EditManifest {
                root: "/projects/soulfire".into(),
                manifest_source: "version: 1\nname: Soulfire\n".to_string(),
            })
        );
        assert_eq!(edit_manifest_request(&snapshot.sections[1]), None);
    }

    #[test]
    fn invalid_tasks_manifest_still_provides_an_edit_request() {
        let section = ContextSection {
            id: MANIFEST_PLUGIN_ID.to_string(),
            title: ".phantom.yml".to_string(),
            content: ContextSectionContent::ManifestError(
                crate::context_actions::ManifestErrorSection {
                    root: "/projects/soulfire".into(),
                    manifest_source: "version: [unterminated".to_string(),
                    message: "invalid .phantom.yml".to_string(),
                },
            ),
        };

        assert_eq!(
            edit_manifest_request(&section),
            Some(ContextRequest::EditManifest {
                root: "/projects/soulfire".into(),
                manifest_source: "version: [unterminated".to_string(),
            })
        );
    }

    #[test]
    fn invalid_tasks_manifest_keeps_the_header_edit_control_visible() {
        let snapshot = ContextSnapshot {
            cwd: "/projects/soulfire".into(),
            sections: vec![ContextSection {
                id: MANIFEST_PLUGIN_ID.to_string(),
                title: ".phantom.yml".to_string(),
                content: ContextSectionContent::ManifestError(
                    crate::context_actions::ManifestErrorSection {
                        root: "/projects/soulfire".into(),
                        manifest_source: "version: [unterminated".to_string(),
                        message: "invalid .phantom.yml".to_string(),
                    },
                ),
            }],
        };
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();

        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            state.draw(ui, &mut config, &snapshot, 42.0, 220);
        });

        assert!(ctx
            .read_response(Id::new(("context_section_edit", MANIFEST_PLUGIN_ID)))
            .is_some());
    }

    #[test]
    fn tasks_header_ellipsis_emits_edit_without_collapsing_section() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        let screen_rect =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0));
        let mut initial = None;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                initial = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
            },
        );
        initial.unwrap();
        let ellipsis = ctx
            .read_response(Id::new(("context_section_edit", MANIFEST_PLUGIN_ID)))
            .unwrap()
            .rect
            .center();
        let click_input = egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(ellipsis),
                egui::Event::PointerButton {
                    pos: ellipsis,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: ellipsis,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut clicked = None;
        let _ = ctx.run_ui(click_input, |ui| {
            clicked = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
        });

        assert!(matches!(
            clicked.unwrap().request,
            Some(ContextRequest::EditManifest { .. })
        ));
        assert!(
            !config
                .context_actions
                .plugin(MANIFEST_PLUGIN_ID)
                .unwrap()
                .section_collapsed
        );
    }

    #[test]
    fn trust_review_renders_tasks_as_single_exact_command_lines() {
        let tab = ManifestTab {
            id: "api".to_string(),
            title: "API".to_string(),
            cwd: "/projects/soulfire".into(),
            task: Some(ManifestTask {
                program: "cargo".to_string(),
                args: vec!["run".to_string(), "--flag with space".to_string()],
                env: vec![("RUST_LOG".to_string(), "info,soulfire=debug".to_string())],
            }),
        };

        assert_eq!(
            manifest_task_command(&tab),
            Some("RUST_LOG=info,soulfire=debug cargo run \"--flag with space\"".to_string())
        );
    }

    #[test]
    fn trust_review_omits_the_command_for_shell_only_tabs() {
        let tab = ManifestTab {
            id: "shell".to_string(),
            title: "Shell".to_string(),
            cwd: "/projects/soulfire".into(),
            task: None,
        };

        assert_eq!(manifest_task_command(&tab), None);
    }

    #[test]
    fn trust_review_shows_cwd_only_when_it_differs_from_the_project_root() {
        let root = Path::new("/projects/soulfire");
        let tab = |cwd: &str| ManifestTab {
            id: "api".to_string(),
            title: "API".to_string(),
            cwd: cwd.into(),
            task: None,
        };

        assert_eq!(
            manifest_task_location(&tab("/projects/soulfire"), root),
            None
        );
        assert_eq!(
            manifest_task_location(&tab("/projects/soulfire/soulfire-api"), root),
            Some("in soulfire-api".to_string())
        );
        assert_eq!(
            manifest_task_location(&tab("/elsewhere/tasks"), root),
            Some("in /elsewhere/tasks".to_string())
        );
    }

    #[test]
    fn trusted_task_actions_use_compact_start_labels() {
        assert_eq!(START_ALL_TASKS_LABEL, "Start all (new tabs)");
        assert_eq!(task_start_label("Soulfire API"), "Start Soulfire API");
        assert_eq!(task_start_label("Soulfire UI"), "Start Soulfire UI");
    }

    #[test]
    fn spdeploy_dropdown_formats_leaf_name_and_description() {
        let snapshot = snapshot();
        let ContextSectionContent::Spdeploy(spdeploy) = &snapshot.sections[1].content else {
            panic!("expected spdeploy section");
        };

        let entries = spdeploy_dropdown_entries(spdeploy);

        assert_eq!(
            entries,
            vec![SpdeployDropdownEntry::Operation {
                selection: SpdeploySelection {
                    config_path: "/projects/soulfire/deploy.yml".into(),
                    operation: "deploy".to_string(),
                },
                text: "deploy: Deploy Soulfire".to_string(),
            }]
        );
    }

    #[test]
    fn spdeploy_breadcrumbs_are_non_selectable_group_headings() {
        let section = SpdeploySection {
            config_path: "/projects/soulfire/deploy.yml".into(),
            project_name: "Soulfire".to_string(),
            operations: vec![
                SpdeployOperation {
                    name: "release".to_string(),
                    breadcrumbs: vec!["api".to_string()],
                    description: Some("Release the API".to_string()),
                    config_path: "/projects/soulfire/api.yml".into(),
                },
                SpdeployOperation {
                    name: "release".to_string(),
                    breadcrumbs: vec!["ui".to_string()],
                    description: Some("Release the UI".to_string()),
                    config_path: "/projects/soulfire/ui.yml".into(),
                },
            ],
            config_sources: Vec::new(),
        };

        let entries = spdeploy_dropdown_entries(&section);

        assert!(matches!(&entries[0], SpdeployDropdownEntry::Heading(text) if text == "api"));
        assert!(
            matches!(&entries[1], SpdeployDropdownEntry::Operation { text, .. } if text == "release: Release the API")
        );
        assert!(matches!(&entries[2], SpdeployDropdownEntry::Heading(text) if text == "ui"));
        assert!(
            matches!(&entries[3], SpdeployDropdownEntry::Operation { text, .. } if text == "release: Release the UI")
        );
        let selections: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                SpdeployDropdownEntry::Operation { selection, .. } => Some(selection),
                SpdeployDropdownEntry::Heading(_) => None,
            })
            .collect();
        assert_ne!(selections[0], selections[1]);
    }

    #[test]
    fn dragging_the_left_divider_updates_persisted_width() {
        let ctx = Context::default();
        let mut config = AppConfig::default();
        let mut state = ContextUi::default();
        let screen_rect =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0));
        let mut initial = None;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                initial = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
            },
        );
        let start = egui::pos2(initial.unwrap().rect.unwrap().left(), 120.0);
        let pressed = egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let _ = ctx.run_ui(pressed, |ui| {
            state.draw(ui, &mut config, &snapshot(), 42.0, 220);
        });

        let dragged = egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerMoved(egui::pos2(
                start.x - 340.0,
                120.0,
            ))],
            ..Default::default()
        };
        let mut outcome = None;
        let _ = ctx.run_ui(dragged, |ui| {
            outcome = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
        });

        let settled_drag = egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let _ = ctx.run_ui(settled_drag, |ui| {
            outcome = Some(state.draw(ui, &mut config, &snapshot(), 42.0, 220));
        });

        assert_eq!(config.context_actions.sidebar_width, 600);
        assert_eq!(outcome.unwrap().reserved_width_points, 600.0);
    }
}
