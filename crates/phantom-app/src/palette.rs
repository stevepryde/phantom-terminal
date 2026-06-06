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
                label: self.commands[command_index].label.clone(),
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
    pub label: String,
    pub selected: bool,
}

fn build_commands(config: &AppConfig) -> Vec<Command> {
    let mut commands = vec![
        Command {
            label: "New Tab".into(),
            action: PaletteAction::NewTab,
        },
        Command {
            label: "Close Tab".into(),
            action: PaletteAction::CloseTab,
        },
        Command {
            label: "Rename Tab".into(),
            action: PaletteAction::RenameTab,
        },
        Command {
            label: "Open Settings".into(),
            action: PaletteAction::OpenSettings,
        },
    ];
    for profile in &config.shell_profiles {
        commands.push(Command {
            label: format!("New Tab: {}", profile.name),
            action: PaletteAction::NewTabWithProfile(profile.id.clone()),
        });
    }
    for theme in themes::UI_THEMES {
        commands.push(Command {
            label: format!("Theme: {theme}"),
            action: PaletteAction::SetUiTheme((*theme).to_string()),
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
}
