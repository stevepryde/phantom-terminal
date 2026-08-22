//! Phantom Terminal core.
//!
//! UI-agnostic backend: PTY lifecycle, configuration + validation, the SQLite
//! session store, and launch-argument parsing. This crate has no GUI or
//! rendering dependencies; the native winit/wgpu app builds on top of it.
//!
//! Security note: every user-controlled config field is bounds-checked in
//! [`AppConfig::validate`]. Normal process spawning resolves a stored,
//! validated [`ShellProfile`]; contextual tasks require an exact-source-bound
//! stored trust record and structured argv/environment validation. Neither path
//! accepts an ad-hoc shell command string. See [`resolve_launch_opts`] and
//! [`resolve_trusted_task`].

mod bounded_file;
pub mod config;
pub mod context;
pub mod error;
pub mod launch;
pub mod pty;
pub mod session;
pub mod spawn;
pub mod spdeploy;

/// Maximum number of terminal tabs that may be live or persisted at once.
pub const MAX_TABS: usize = 128;

pub use bounded_file::{read_bounded_regular_file, BoundedReadError};
pub use config::{
    AppConfig, Keybinding, KeybindingAction, ShellProfile, Theme, WindowSize, UI_THEMES,
};
pub use context::{
    load_context_manifest, load_context_manifest_source, parse_context_manifest,
    resolve_trusted_task, trust_context_manifest, validate_context_manifest, ContextActionsConfig,
    ContextManifest, ContextManifestSource, ContextPluginConfig, ContextRun, ContextTab,
    DirectoryHistoryEntry, LoadedContextManifest, TrustedContextTab, TrustedProject,
    BUILT_IN_CONTEXT_PLUGIN_IDS, CONTEXT_MANIFEST_FILE, DEFAULT_CONTEXT_SIDEBAR_WIDTH,
    DIRECTORY_SELECTION_PER_SIGNAL, FREQUENT_COMMANDS_PLUGIN_ID, MANIFEST_PLUGIN_ID,
    MANIFEST_PLUGIN_ORDER, MAX_CONTEXT_MANIFEST_BYTES, MAX_CONTEXT_PLUGINS,
    MAX_CONTEXT_PLUGIN_ORDER, MAX_CONTEXT_SIDEBAR_WIDTH, MAX_CONTEXT_TABS, MAX_DIRECTORY_HISTORY,
    MAX_TRUSTED_PROJECTS, MIN_CONTEXT_SIDEBAR_WIDTH, RECENT_DIRECTORIES_PLUGIN_ID,
    RECENT_DIRECTORIES_PLUGIN_ORDER, SPDEPLOY_PLUGIN_ID, SPDEPLOY_PLUGIN_ORDER,
};
pub use error::{AppError, AppResult};
pub use launch::{LaunchContext, LaunchState};
pub use pty::{resolve_program, LaunchOpts, PtyManager, PtySink, SpawnOpts, StartupCommand};
pub use session::{
    RememberedTabsLock, SessionStore, TabRecord, MAX_TAB_CWD_LEN, MAX_TAB_ID_LEN,
    MAX_TAB_PROFILE_ID_LEN, MAX_TAB_TIMESTAMP_LEN, MAX_TAB_TITLE_LEN,
};
pub use spawn::{default_home_dir, resolve_launch_opts};
pub use spdeploy::{
    load_spdeploy_graph, trust_spdeploy_graph, verify_spdeploy_graph, SpdeployGraph,
    SpdeployOperation, TrustedSpdeployProject, TrustedSpdeploySource, MAX_SPDEPLOY_CONFIGS,
    MAX_SPDEPLOY_SOURCE_BYTES, SPDEPLOY_CONFIG_FILE,
};
