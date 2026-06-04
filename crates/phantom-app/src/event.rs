//! Engine-independent input events.
//!
//! The winit layer translates raw window events into these, so the app's logic
//! (and tests / automation) never touch winit types. A logical key is
//! represented with [`phantom_emu::Key`] (the same vocabulary used for PTY
//! encoding); modifiers carry an extra "super" (Cmd) bit that winit's encoding
//! modifiers don't.

use phantom_emu::{Key, Modifiers as EmuMods};

/// Modifier-key state, including the Cmd/Super key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Cmd on macOS, Super/Windows elsewhere.
    pub sup: bool,
}

impl Mods {
    /// The platform "primary" accelerator: Cmd on macOS, Ctrl elsewhere.
    pub fn primary(self) -> bool {
        if cfg!(target_os = "macos") {
            self.sup
        } else {
            self.ctrl
        }
    }

    /// The subset used for PTY key encoding (no super).
    pub fn emu(self) -> EmuMods {
        EmuMods {
            ctrl: self.ctrl,
            alt: self.alt,
            shift: self.shift,
        }
    }
}

/// A window/input event in engine-independent form.
#[derive(Debug, Clone)]
pub enum AppInput {
    /// A key press: the logical key (for shortcuts / navigation / PTY encoding)
    /// plus the text it produces (for insertion into the terminal or overlays).
    Key {
        key: Key,
        text: Option<String>,
        mods: Mods,
    },
    /// IME composition was committed to this text.
    ImeCommit(String),
    /// IME composition preedit changed.
    ImePreedit(String),
    MouseDown {
        x: f32,
        y: f32,
    },
    MouseUp {
        x: f32,
        y: f32,
    },
    MouseMove {
        x: f32,
        y: f32,
    },
    /// Vertical wheel in lines (positive = up / back into history).
    Wheel {
        lines: i32,
    },
    Resized {
        width: u32,
        height: u32,
    },
    ScaleChanged,
    ModifiersChanged(Mods),
    CloseRequested,
}
