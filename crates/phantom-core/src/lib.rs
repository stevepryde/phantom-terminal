//! Phantom Terminal core.
//!
//! UI-agnostic backend: PTY lifecycle, configuration + validation, the SQLite
//! session store, and launch-argument parsing. This crate has no GUI or
//! rendering dependencies; the native winit/wgpu app builds on top of it.
//!
//! Security note: every user-controlled config field is bounds-checked in
//! [`AppConfig::validate`], and process spawning only ever resolves a stored,
//! validated [`ShellProfile`] (never an ad-hoc command). See [`resolve_launch_opts`].

pub mod config;
pub mod error;
pub mod launch;
pub mod pty;
pub mod session;
pub mod spawn;

pub use config::{AppConfig, Keybinding, ShellProfile, Theme};
pub use error::{AppError, AppResult};
pub use launch::{LaunchContext, LaunchState};
pub use pty::{LaunchOpts, PtyManager, PtySink, SpawnOpts};
pub use session::{
    SessionStore, TabRecord, MAX_TAB_CWD_LEN, MAX_TAB_ID_LEN, MAX_TAB_PROFILE_ID_LEN,
    MAX_TAB_RECORDS, MAX_TAB_TITLE_LEN,
};
pub use spawn::{default_home_dir, resolve_launch_opts};
