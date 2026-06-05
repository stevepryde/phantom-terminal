//! Command palette overlay: fuzzy-filter a list of actions and execute one.
//! Drawn on the renderer's overlay layer; keys are captured while open.

use phantom_core::AppConfig;
use phantom_emu::Key;
use phantom_gfx::Renderer;

use crate::chrome::ChromeColors;
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
}

impl PaletteState {
    /// Open the palette, rebuilding the command list from the current config.
    pub fn open(&mut self, config: &AppConfig) {
        self.commands = build_commands(config);
        self.query.clear();
        self.selected = 0;
        self.open = true;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
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

    pub fn draw(
        &self,
        r: &mut Renderer,
        queue: &wgpu::Queue,
        win_w: f32,
        win_h: f32,
        colors: &ChromeColors,
    ) {
        let cell_h = r.cell_size().1;
        let pad = 12.0;
        let row_h = cell_h + 6.0;

        // Dim backdrop.
        r.fill_rect(0.0, 0.0, win_w, win_h, [0, 0, 0, 150]);

        let panel_w = (win_w * 0.6).clamp(360.0, 760.0);
        let rows = self.filtered.len().min(MAX_ROWS);
        let panel_h = pad * 2.0 + cell_h + pad + (rows.max(1) as f32) * row_h;
        let px = ((win_w - panel_w) / 2.0).max(0.0);
        let py = (win_h * 0.12).max(20.0);

        r.fill_rect(px, py, panel_w, panel_h, colors.bar_bg);
        r.fill_rect(px, py, panel_w, 2.0, colors.accent);

        // Query line.
        let qy = py + pad;
        let prompt = format!("> {}\u{258f}", self.query);
        r.text(queue, px + pad, qy, &prompt, colors.text);

        // Results, scrolled so the selection stays visible.
        let start = self.selected.saturating_sub(MAX_ROWS - 1);
        let mut y = qy + cell_h + pad;
        let highlight = [colors.accent[0], colors.accent[1], colors.accent[2], 70];
        for (row, &ci) in self.filtered.iter().enumerate().skip(start).take(MAX_ROWS) {
            let selected = row == self.selected;
            if selected {
                r.fill_rect(px, y, panel_w, row_h, highlight);
            }
            let color = if selected {
                colors.text
            } else {
                colors.dim_text
            };
            r.text(queue, px + pad, y + 3.0, &self.commands[ci].label, color);
            y += row_h;
        }
    }
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
