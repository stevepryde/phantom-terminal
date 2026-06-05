//! A single terminal tab: its VT model, PTY id, and display metadata.

use phantom_emu::AlacrittyCore;

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
