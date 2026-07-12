//! A single terminal tab: its VT model, PTY id, and display metadata.

use phantom_emu::{AlacrittyCore, VtCore};

pub struct Tab {
    /// App-assigned id used to route PTY output back to this tab (independent of
    /// the OS pty id, which we only learn after spawning).
    pub id: u64,
    pub core: AlacrittyCore,
    pub pty_id: u32,
    /// User-set title (via rename); when empty, the cwd basename is shown.
    pub custom_title: String,
    /// Current working directory (updated by cwd polling).
    pub cwd: String,
    /// Profile this tab was launched with; persisted for faithful restore.
    pub profile_id: Option<String>,
    /// A one-shot contextual task returns to the default shell when it exits,
    /// preserving its output and leaving the tab useful.
    pub return_to_shell: bool,
}

impl Tab {
    pub fn new(
        id: u64,
        core: AlacrittyCore,
        pty_id: u32,
        cwd: String,
        profile_id: Option<String>,
    ) -> Self {
        Self {
            id,
            core,
            pty_id,
            custom_title: String::new(),
            cwd,
            profile_id,
            return_to_shell: false,
        }
    }

    /// Feed PTY bytes into the terminal model verbatim.
    ///
    /// The byte stream is passed straight to the VT core with no rewriting. A
    /// terminal emulator must be a faithful sink: mutating the stream (e.g. to
    /// hide a prompt's cosmetic leading blank line) desyncs the cursor model of
    /// any program that redraws in place with erase-then-newline sequences
    /// (`inquire`/`fzf` menus, docker/build progress), garbling their output.
    pub fn advance_pty(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.core.advance(bytes);
        }
    }

    /// The label shown on the tab: the custom title if set, else the cwd
    /// basename, else a default.
    pub fn title(&self) -> String {
        if !self.custom_title.is_empty() {
            return self.custom_title.clone();
        }
        let trimmed = self.cwd.trim_end_matches('/');
        match trimmed.rsplit('/').next() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => "shell".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AlacrittyCore, Tab};
    use phantom_emu::{CursorShape, VtCore};

    fn tab(rows: u16, cols: u16) -> Tab {
        let core = AlacrittyCore::new(rows, cols, 100, CursorShape::Block);
        Tab::new(1, core, 7, "/tmp".to_string(), None)
    }

    #[test]
    fn pty_bytes_reach_the_grid_unmodified() {
        let mut tab = tab(4, 20);
        tab.advance_pty(b"hello\r\nworld");
        assert_eq!(tab.core.snapshot().row_text(0), "hello");
        assert_eq!(tab.core.snapshot().row_text(1), "world");
    }

    #[test]
    fn in_place_redraw_after_a_plain_prompt_is_not_garbled() {
        // Regression: a plain prompt (`user@host:~$ `, no newline, no erase)
        // must not arm any blank-line stripping. An in-place updater that walks
        // the screen with erase-then-newline (`\x1b[K\r\n`, as docker/build
        // progress and TUI menus do) must land on distinct rows. Previously a
        // newline here was silently dropped, collapsing and cascading the
        // output. The tab's output must match the raw VT core exactly.
        let prompt = b"user@host:~$ ".as_slice();
        let output = b"\rStep 1\x1b[K\r\nStep 2\x1b[K\r\nStep 3\x1b[K".as_slice();

        let mut tab = tab(6, 40);
        tab.advance_pty(prompt);
        tab.advance_pty(output);
        let via_tab: Vec<String> = (0..6)
            .map(|r| tab.core.snapshot().row_text(r).trim_end().to_string())
            .collect();

        let mut raw = AlacrittyCore::new(6, 40, 100, CursorShape::Block);
        raw.advance(prompt);
        raw.advance(output);
        let via_core: Vec<String> = (0..6)
            .map(|r| raw.snapshot().row_text(r).trim_end().to_string())
            .collect();

        assert_eq!(via_tab, via_core);
        // `\r` returns to column 0 and overwrites the prompt; each step then
        // lands on its own row.
        assert_eq!(via_tab[0], "Step 1");
        assert_eq!(via_tab[1], "Step 2");
        assert_eq!(via_tab[2], "Step 3");
    }
}
