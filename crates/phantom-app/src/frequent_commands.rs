//! Ephemeral, per-tab command frequency tracking.
//!
//! This observes only input Phantom sends to the PTY. It never reads a shell
//! history file and is deliberately not serializable, so closing a tab drops
//! every command and count with it.

use phantom_emu::Key;

const MAX_COMMAND_BYTES: usize = 4096;
const MAX_TRACKED_COMMANDS: usize = 256;
pub const FREQUENT_COMMAND_LIMIT: usize = 3;

#[derive(Debug)]
pub(crate) struct FrequentCommands {
    current_line: String,
    current_line_reliable: bool,
    capture_decided: bool,
    capture_allowed: bool,
    usages: Vec<CommandUsage>,
    /// Cached ranking, rebuilt on mutation so per-frame reads don't re-sort.
    top: Vec<String>,
    sequence: u64,
}

impl Default for FrequentCommands {
    fn default() -> Self {
        Self {
            current_line: String::new(),
            current_line_reliable: true,
            capture_decided: false,
            capture_allowed: false,
            usages: Vec::new(),
            top: Vec::new(),
            sequence: 0,
        }
    }
}

#[derive(Debug)]
struct CommandUsage {
    command: String,
    count: u64,
    last_used: u64,
}

impl FrequentCommands {
    /// Decide whether the current input line began at a recognizable shell
    /// prompt. This conservative gate avoids treating password prompts and
    /// interactive application input as commands.
    pub fn prepare_line(&mut self, at_shell_prompt: bool) {
        if !self.capture_decided {
            self.capture_decided = true;
            self.capture_allowed = at_shell_prompt;
        }
    }

    pub fn observe_text(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\r' | '\n' => {
                    let pasted_from_shell_prompt = self.capture_allowed;
                    self.submit_current_line();
                    if chars.peek().is_some() {
                        self.prepare_line(pasted_from_shell_prompt);
                    }
                }
                ch if !ch.is_control() => self.push_char(ch),
                _ => self.invalidate_current_line(),
            }
        }
    }

    /// Clipboard content is not authoritative evidence of a command the user
    /// executed. Mark the current line unreliable so a later Enter cannot
    /// record either the paste or a typed prefix/suffix as a partial command.
    pub fn observe_paste(&mut self) {
        self.invalidate_current_line();
    }

    pub fn observe_key(&mut self, key: Key, control: bool) {
        match (key, control) {
            (Key::Enter, _) => self.submit_current_line(),
            (Key::Backspace, false) => {
                self.current_line.pop();
            }
            (Key::Char('u' | 'U'), true) | (Key::Char('c' | 'C'), true) => {
                self.current_line.clear();
                self.current_line_reliable = true;
            }
            // Shell completion, history recall, cursor movement, deletes, and
            // other control editing can change the line without echoing the
            // resulting text back through the input stream. Skip that one
            // submission instead of recording a command Phantom did not see.
            (Key::Char(_), false) => {}
            _ => self.invalidate_current_line(),
        }
    }

    pub fn record_executed(&mut self, command: &str) {
        if command.starts_with(char::is_whitespace) {
            // Match the widespread shell convention that a leading space marks
            // a command as private / excluded from history.
            return;
        }
        let command = command.trim_end();
        if command.is_empty()
            || command.len() > MAX_COMMAND_BYTES
            || command.contains(char::is_control)
        {
            return;
        }
        self.sequence = self.sequence.saturating_add(1);
        if let Some(usage) = self
            .usages
            .iter_mut()
            .find(|usage| usage.command == command)
        {
            usage.count = usage.count.saturating_add(1);
            usage.last_used = self.sequence;
            self.rebuild_top();
            return;
        }
        if self.usages.len() == MAX_TRACKED_COMMANDS {
            let evict = self
                .usages
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| {
                    left.count
                        .cmp(&right.count)
                        .then(left.last_used.cmp(&right.last_used))
                        .then_with(|| right.command.cmp(&left.command))
                })
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.usages.remove(evict);
        }
        self.usages.push(CommandUsage {
            command: command.to_string(),
            count: 1,
            last_used: self.sequence,
        });
        self.rebuild_top();
    }

    fn rebuild_top(&mut self) {
        let mut usages: Vec<_> = self.usages.iter().collect();
        usages.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then(right.last_used.cmp(&left.last_used))
                .then(left.command.cmp(&right.command))
        });
        self.top = usages
            .into_iter()
            .take(FREQUENT_COMMAND_LIMIT)
            .map(|usage| usage.command.clone())
            .collect();
    }

    pub fn top(&self) -> &[String] {
        &self.top
    }

    pub fn reset_input_line(&mut self) {
        self.current_line.clear();
        self.current_line_reliable = true;
        self.capture_decided = false;
        self.capture_allowed = false;
    }

    fn push_char(&mut self, ch: char) {
        if self.current_line.len().saturating_add(ch.len_utf8()) > MAX_COMMAND_BYTES {
            self.invalidate_current_line();
            return;
        }
        self.current_line.push(ch);
    }

    fn invalidate_current_line(&mut self) {
        self.current_line.clear();
        self.current_line_reliable = false;
    }

    fn submit_current_line(&mut self) {
        if self.current_line_reliable && self.capture_allowed {
            let command = std::mem::take(&mut self.current_line);
            self.record_executed(&command);
        }
        self.reset_input_line();
    }
}

#[cfg(test)]
mod tests {
    use super::FrequentCommands;
    use phantom_emu::Key;

    #[test]
    fn ranks_by_frequency_then_recency() {
        let mut commands = FrequentCommands::default();
        commands.prepare_line(true);
        commands.observe_text("cargo test\nls\ncargo test\ngit status\n");
        assert_eq!(commands.top(), ["cargo test", "git status", "ls"]);
    }

    #[test]
    fn tracks_only_three_commands_and_respects_backspace() {
        let mut commands = FrequentCommands::default();
        commands.prepare_line(true);
        commands.observe_text("cargo tesx");
        commands.observe_key(Key::Backspace, false);
        commands.observe_text("t");
        commands.observe_key(Key::Enter, false);
        commands.prepare_line(true);
        commands.observe_text("one\ntwo\nthree\nfour\n");
        assert_eq!(commands.top(), ["four", "three", "two"]);
        assert!(!commands.top().iter().any(|command| command == "one"));
    }

    #[test]
    fn uncertain_shell_edits_skip_only_that_submission() {
        let mut commands = FrequentCommands::default();
        commands.prepare_line(true);
        commands.observe_text("partial");
        commands.observe_key(Key::Up, false);
        commands.observe_key(Key::Enter, false);
        commands.prepare_line(true);
        commands.observe_text("visible\n");
        assert_eq!(commands.top(), ["visible"]);
    }

    #[test]
    fn control_u_clears_the_observed_line() {
        let mut commands = FrequentCommands::default();
        commands.prepare_line(true);
        commands.observe_text("wrong");
        commands.observe_key(Key::Char('u'), true);
        commands.observe_text("right");
        commands.observe_key(Key::Enter, false);
        assert_eq!(commands.top(), ["right"]);
    }

    #[test]
    fn paste_content_and_partial_surrounding_text_are_never_recorded() {
        let mut commands = FrequentCommands::default();
        commands.prepare_line(true);
        commands.observe_text("typed-prefix ");

        // Paste content is deliberately not supplied to the tracker. This is
        // the same for plain and bracketed multiline paste.
        commands.observe_paste();
        commands.observe_text("typed-suffix");
        commands.observe_key(Key::Enter, false);
        assert!(commands.top().is_empty());

        // The real Enter resets the unreliable line, so the next command can
        // be observed normally from a newly recognized prompt.
        commands.prepare_line(true);
        commands.observe_text("visible");
        commands.observe_key(Key::Enter, false);
        assert_eq!(commands.top(), ["visible"]);
    }

    #[test]
    fn ignores_non_shell_and_leading_space_submissions() {
        let mut commands = FrequentCommands::default();
        commands.prepare_line(false);
        commands.observe_text("secret\n");
        commands.prepare_line(true);
        commands.observe_text(" private-token\n");
        assert!(commands.top().is_empty());
    }
}
