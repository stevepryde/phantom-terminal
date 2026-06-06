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
    startup_blank_line: StartupBlankLineFilter,
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
            startup_blank_line: StartupBlankLineFilter::new(),
        }
    }

    /// Feed PTY bytes into the terminal model.
    pub fn advance_pty(&mut self, bytes: &[u8]) {
        let bytes = self.startup_blank_line.filter(bytes);
        if !bytes.is_empty() {
            self.core.advance(&bytes);
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

/// Starship and similar prompts can intentionally start with a newline so
/// repeated prompts have breathing room. At the very start of a fresh tab that
/// spacer becomes a stored empty first scrollback row, so remove exactly one
/// initial LF before prompt content has reached the first terminal line.
struct StartupBlankLineFilter {
    done: bool,
    pending: Vec<u8>,
}

impl StartupBlankLineFilter {
    const MAX_PENDING: usize = 4096;

    fn new() -> Self {
        Self {
            done: false,
            pending: Vec::new(),
        }
    }

    fn filter(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.done {
            return bytes.to_vec();
        }

        self.pending.extend_from_slice(bytes);
        match scan_startup_bytes(&self.pending) {
            StartupScan::NeedMore if self.pending.len() <= Self::MAX_PENDING => Vec::new(),
            StartupScan::Strip { start, end } => {
                self.done = true;
                let mut out = std::mem::take(&mut self.pending);
                out.drain(start..end);
                out
            }
            StartupScan::Keep | StartupScan::NeedMore => {
                self.done = true;
                std::mem::take(&mut self.pending)
            }
        }
    }
}

enum StartupScan {
    NeedMore,
    Strip { start: usize, end: usize },
    Keep,
}

fn scan_startup_bytes(bytes: &[u8]) -> StartupScan {
    let mut index = 0;
    let mut saw_visible = false;
    let mut saw_erase = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                return if !saw_visible || saw_erase {
                    StartupScan::Strip {
                        start: index,
                        end: index + 1,
                    }
                } else {
                    StartupScan::Keep
                };
            }
            0x1b => match skip_escape(bytes, index) {
                Some(escape) => {
                    saw_erase |= escape.erases_visible_content;
                    index = escape.next;
                }
                None => return StartupScan::NeedMore,
            },
            b' ' | b'\t' => index += 1,
            0x00..=0x1f | 0x7f => index += 1,
            _ => {
                saw_visible = true;
                index += 1;
            }
        }
    }
    StartupScan::NeedMore
}

struct EscapeSkip {
    next: usize,
    erases_visible_content: bool,
}

fn skip_escape(bytes: &[u8], start: usize) -> Option<EscapeSkip> {
    let introducer = *bytes.get(start + 1)?;
    match introducer {
        b'[' => skip_csi(bytes, start + 2),
        b']' | b'P' | b'^' | b'_' | b'X' => {
            skip_string_escape(bytes, start + 2).map(|next| EscapeSkip {
                next,
                erases_visible_content: false,
            })
        }
        b'(' | b')' | b'*' | b'+' => bytes.get(start + 2).map(|_| EscapeSkip {
            next: start + 3,
            erases_visible_content: false,
        }),
        _ => Some(EscapeSkip {
            next: start + 2,
            erases_visible_content: false,
        }),
    }
}

fn skip_csi(bytes: &[u8], mut index: usize) -> Option<EscapeSkip> {
    while index < bytes.len() {
        let byte = bytes[index];
        if (0x40..=0x7e).contains(&byte) {
            return Some(EscapeSkip {
                next: index + 1,
                erases_visible_content: matches!(byte, b'J' | b'K'),
            });
        }
        index += 1;
    }
    None
}

fn skip_string_escape(bytes: &[u8], mut index: usize) -> Option<usize> {
    while index < bytes.len() {
        if bytes[index] == 0x07 {
            return Some(index + 1);
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
            return Some(index + 2);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{AlacrittyCore, StartupBlankLineFilter, Tab};
    use phantom_emu::{CursorShape, VtCore};

    #[test]
    fn strips_initial_lf_before_prompt_text() {
        let mut filter = StartupBlankLineFilter::new();

        assert_eq!(filter.filter(b"\nprompt"), b"prompt");
        assert_eq!(filter.filter(b"\nnext prompt"), b"\nnext prompt");
    }

    #[test]
    fn strips_actual_starship_prompt_leading_lf() {
        let mut filter = StartupBlankLineFilter::new();
        let starship_prompt = b"\n%{\x1b[1;36m%}phantom-terminal%{\x1b[0m%} via %{\
            \x1b[1;31m%}\xf0\x9f\xa5\x9f v1.3.9 %{\x1b[0m%}\n%{\x1b[1;32m%}\xe2\x9d\xaf%{\x1b[0m%} ";

        assert_eq!(&filter.filter(starship_prompt)[..1], b"%");
    }

    #[test]
    fn strips_lf_after_actual_zsh_starship_prelude() {
        let mut filter = StartupBlankLineFilter::new();
        let zsh_starship_start = b"\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m    \
            \r \r\r\x1b[0m\x1b[27m\x1b[24m\x1b[J\r\n\x1b[1;36mphantom-terminal";

        let filtered = filter.filter(zsh_starship_start);

        assert!(filtered.ends_with(b"\r\x1b[1;36mphantom-terminal"));
        assert!(!filtered.ends_with(b"\r\n\x1b[1;36mphantom-terminal"));
    }

    #[test]
    fn strips_initial_crlf_after_escape_setup() {
        let mut filter = StartupBlankLineFilter::new();

        assert_eq!(
            filter.filter(b"\x1b[?2004h\x1b]0;title\x07\r\nprompt"),
            b"\x1b[?2004h\x1b]0;title\x07\rprompt"
        );
    }

    #[test]
    fn keeps_output_that_starts_with_visible_text() {
        let mut filter = StartupBlankLineFilter::new();

        assert_eq!(filter.filter(b"motd\nprompt"), b"motd\nprompt");
    }

    #[test]
    fn strips_initial_lf_after_prompt_leading_whitespace() {
        let mut filter = StartupBlankLineFilter::new();

        assert_eq!(filter.filter(b" \r\nprompt"), b" \rprompt");
    }

    #[test]
    fn handles_escape_sequence_split_across_chunks() {
        let mut filter = StartupBlankLineFilter::new();

        assert!(filter.filter(b"\x1b[").is_empty());
        assert_eq!(filter.filter(b"?2004h\nprompt"), b"\x1b[?2004hprompt");
    }

    #[test]
    fn stripped_startup_line_does_not_enter_scrollback() {
        let core = AlacrittyCore::new(2, 20, 100, CursorShape::Block);
        let mut tab = Tab::new(1, core, 7, "/tmp".to_string(), None);

        tab.advance_pty(b" \r\nprompt\r\nline1\r\nline2\r\nline3");
        tab.core.scroll_to_offset(100);

        assert_ne!(tab.core.snapshot().row_text(0), "");
    }

    #[test]
    fn actual_zsh_starship_prelude_does_not_leave_empty_scrollback_row() {
        let core = AlacrittyCore::new(2, 40, 100, CursorShape::Block);
        let mut tab = Tab::new(1, core, 7, "/tmp".to_string(), None);
        let zsh_starship_start = b"\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m    \
            \r \r\r\x1b[0m\x1b[27m\x1b[24m\x1b[J\r\n\x1b[1;36mphantom-terminal";

        tab.advance_pty(zsh_starship_start);
        assert_eq!(tab.core.snapshot().row_text(0), "phantom-terminal");

        tab.advance_pty(b"\r\nline1\r\nline2\r\nline3");
        tab.core.scroll_to_offset(100);

        assert_ne!(tab.core.snapshot().row_text(0), "");
    }
}
