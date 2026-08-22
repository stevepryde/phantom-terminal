//! Command palette state: fuzzy-filter a list of actions and execute one.
//!
//! Production rendering is owned by the egui control plane. The key handling
//! methods stay here so headless app-logic tests can drive the same command
//! model without a window.

use phantom_core::AppConfig;
use phantom_emu::Key;

use crate::themes;

const MAX_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    NewTab,
    CloseTab,
    RenameTab,
    OpenSettings,
    NewTabWithProfile(String),
    SetUiTheme(String),
}

struct Command {
    label: String,
    action: PaletteAction,
    group: PaletteGroup,
    shortcut: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteGroup {
    Commands,
    Profiles,
    Appearance,
}

impl PaletteGroup {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Commands => "COMMANDS",
            Self::Profiles => "PROFILES",
            Self::Appearance => "APPEARANCE",
        }
    }
}

pub enum PaletteOutcome {
    None,
    Close,
    Execute(PaletteAction),
}

#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    query: String,
    selected: usize,
    commands: Vec<Command>,
    filtered: Vec<usize>,
    focus_requested: bool,
}

impl PaletteState {
    /// Open the palette, rebuilding the command list from the current config.
    pub fn open(&mut self, config: &AppConfig) {
        self.commands = build_commands(config);
        self.query.clear();
        self.selected = 0;
        self.open = true;
        self.focus_requested = true;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.focus_requested = false;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, query: String) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.selected = 0;
        self.refilter();
    }

    pub fn take_focus_request(&mut self) -> bool {
        let requested = self.focus_requested;
        self.focus_requested = false;
        requested
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        if delta < 0 {
            self.selected = self.selected.saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.selected = (self.selected + delta as usize).min(self.filtered.len() - 1);
        }
    }

    pub fn rows(&self) -> Vec<PaletteRow> {
        let start = self.selected.saturating_sub(MAX_ROWS - 1);
        self.filtered
            .iter()
            .enumerate()
            .skip(start)
            .take(MAX_ROWS)
            .map(|(filtered_index, &command_index)| PaletteRow {
                filtered_index,
                id: self.commands[command_index].action.stable_id(),
                label: self.commands[command_index].label.clone(),
                group: self.commands[command_index].group,
                shortcut: self.commands[command_index].shortcut.clone(),
                selected: filtered_index == self.selected,
            })
            .collect()
    }

    pub fn execute_selected(&self) -> Option<PaletteAction> {
        let command_index = *self.filtered.get(self.selected)?;
        Some(self.commands[command_index].action.clone())
    }

    pub fn execute_filtered(&self, filtered_index: usize) -> Option<PaletteAction> {
        let command_index = *self.filtered.get(filtered_index)?;
        Some(self.commands[command_index].action.clone())
    }

    pub fn handle_key(&mut self, key: Key, text: Option<&str>) -> PaletteOutcome {
        match key {
            Key::Escape => return PaletteOutcome::Close,
            Key::Enter => {
                return match self.filtered.get(self.selected) {
                    Some(&ci) => PaletteOutcome::Execute(self.commands[ci].action.clone()),
                    None => PaletteOutcome::Close,
                };
            }
            Key::Down => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + 1).min(self.filtered.len() - 1);
                }
                return PaletteOutcome::None;
            }
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                return PaletteOutcome::None;
            }
            Key::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.refilter();
                return PaletteOutcome::None;
            }
            _ => {}
        }

        if let Some(text) = text {
            let mut changed = false;
            for c in text.chars().filter(|c| !c.is_control()) {
                self.query.push(c);
                changed = true;
            }
            if changed {
                self.selected = 0;
                self.refilter();
            }
        }
        PaletteOutcome::None
    }

    fn refilter(&mut self) {
        let mut scored: Vec<(i32, usize)> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fuzzy_score(&self.query, &c.label).map(|s| (s, i)))
            .collect();
        // Highest score first; stable by original order on ties.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
}

pub struct PaletteRow {
    pub filtered_index: usize,
    pub id: String,
    pub label: String,
    pub group: PaletteGroup,
    pub shortcut: Option<String>,
    pub selected: bool,
}

impl PaletteAction {
    fn stable_id(&self) -> String {
        match self {
            Self::NewTab => "new_tab".to_string(),
            Self::CloseTab => "close_tab".to_string(),
            Self::RenameTab => "rename_tab".to_string(),
            Self::OpenSettings => "open_settings".to_string(),
            Self::NewTabWithProfile(id) => format!("new_tab_profile:{id}"),
            Self::SetUiTheme(theme) => format!("set_ui_theme:{theme}"),
        }
    }
}

fn build_commands(config: &AppConfig) -> Vec<Command> {
    let shortcut = |action: &str| {
        config
            .keybindings
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| binding.keys.clone())
    };
    let mut commands = vec![
        Command {
            label: "New Tab".into(),
            action: PaletteAction::NewTab,
            group: PaletteGroup::Commands,
            shortcut: shortcut("tab.new"),
        },
        Command {
            label: "Close Tab".into(),
            action: PaletteAction::CloseTab,
            group: PaletteGroup::Commands,
            shortcut: shortcut("tab.close"),
        },
        Command {
            label: "Rename Tab".into(),
            action: PaletteAction::RenameTab,
            group: PaletteGroup::Commands,
            shortcut: shortcut("tab.rename"),
        },
        Command {
            label: "Open Settings".into(),
            action: PaletteAction::OpenSettings,
            group: PaletteGroup::Commands,
            shortcut: shortcut("settings.toggle"),
        },
    ];
    for profile in &config.shell_profiles {
        commands.push(Command {
            label: format!("New Tab: {}", profile.name),
            action: PaletteAction::NewTabWithProfile(profile.id.clone()),
            group: PaletteGroup::Profiles,
            shortcut: None,
        });
    }
    for theme in themes::UI_THEMES {
        commands.push(Command {
            label: format!("Theme: {theme}"),
            action: PaletteAction::SetUiTheme((*theme).to_string()),
            group: PaletteGroup::Appearance,
            shortcut: None,
        });
    }
    commands
}

/// Score `text` against a fuzzy `query` (subsequence match). Higher is better;
/// `None` means no match. An empty query matches everything with score 0.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();

    let mut qi = 0;
    let mut score = 0i32;
    let mut last_match: Option<usize> = None;
    for (ti, &tc) in t.iter().enumerate() {
        if qi < q.len() && tc == q[qi] {
            score += 10;
            if ti == 0 {
                score += 8; // start-of-string bonus
            }
            if let Some(lm) = last_match {
                if ti == lm + 1 {
                    score += 5; // consecutive bonus
                }
            }
            last_match = Some(ti);
            qi += 1;
        }
    }
    if qi == q.len() {
        // Prefer shorter matches when scores are otherwise equal.
        Some(score - t.len() as i32)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn subsequence_matches_and_non_subsequence_does_not() {
        assert!(fuzzy_score("nt", "New Tab").is_some());
        assert!(fuzzy_score("xyz", "New Tab").is_none());
    }

    #[test]
    fn prefix_scores_higher_than_scattered() {
        let prefix = fuzzy_score("new", "New Tab").unwrap();
        let scattered = fuzzy_score("nta", "New Tab Abc").unwrap();
        assert!(prefix > scattered, "{prefix} !> {scattered}");
    }

    #[test]
    fn consecutive_beats_gapped() {
        let consecutive = fuzzy_score("tab", "Tab").unwrap();
        let gapped = fuzzy_score("tab", "T a b").unwrap();
        assert!(consecutive > gapped);
    }

    #[test]
    fn duplicate_profile_names_have_distinct_row_ids() {
        let mut config = AppConfig::default();
        config.shell_profiles.push(phantom_core::ShellProfile {
            id: "second".to_string(),
            name: config.shell_profiles[0].name.clone(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
        });
        let mut palette = PaletteState::default();
        palette.open(&config);
        palette.set_query("New Tab: Default Shell".to_string());

        let rows = palette.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, rows[1].label);
        assert_ne!(rows[0].id, rows[1].id);
    }

    #[test]
    fn core_palette_rows_include_configured_shortcuts_and_groups() {
        let config = AppConfig::default();
        let mut palette = PaletteState::default();
        palette.open(&config);

        let rows = palette.rows();
        assert_eq!(rows[0].group, PaletteGroup::Commands);
        assert_eq!(rows[0].shortcut.as_deref(), Some("CmdOrCtrl+T"));
        assert!(rows.iter().any(|row| row.group == PaletteGroup::Profiles));
        assert!(rows.iter().any(|row| row.group == PaletteGroup::Appearance));
    }
}
