//! Validated contextual actions discovered from a project's `.phantom.yml`.
//!
//! Manifests are inert until their exact source is explicitly trusted for a
//! canonical project root. Tasks use structured executable/argument/environment
//! fields and never pass through a shell command string.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::pty::LaunchOpts;

pub const CONTEXT_MANIFEST_FILE: &str = ".phantom.yml";
pub const MANIFEST_PLUGIN_ID: &str = "phantom-manifest";
pub const SPDEPLOY_PLUGIN_ID: &str = "spdeploy";
pub const RECENT_DIRECTORIES_PLUGIN_ID: &str = "recent-directories";
pub const BUILT_IN_CONTEXT_PLUGIN_IDS: &[&str] = &[
    RECENT_DIRECTORIES_PLUGIN_ID,
    MANIFEST_PLUGIN_ID,
    SPDEPLOY_PLUGIN_ID,
];
pub const RECENT_DIRECTORIES_PLUGIN_ORDER: u16 = 0;
pub const MANIFEST_PLUGIN_ORDER: u16 = 100;
pub const SPDEPLOY_PLUGIN_ORDER: u16 = 200;
pub const MAX_CONTEXT_PLUGIN_ORDER: u16 = 10_000;
pub const MAX_CONTEXT_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_CONTEXT_TABS: usize = 32;
pub const MAX_CONTEXT_PLUGINS: usize = 32;
pub const MAX_TRUSTED_PROJECTS: usize = 64;
pub const MAX_DIRECTORY_HISTORY: usize = 256;
pub const DIRECTORY_SELECTION_PER_SIGNAL: usize = 5;
pub const MIN_CONTEXT_SIDEBAR_WIDTH: u16 = 100;
/// Persisted safety bound; the UI applies the live 50%-of-viewport limit.
pub const MAX_CONTEXT_SIDEBAR_WIDTH: u16 = 8192;
pub const DEFAULT_CONTEXT_SIDEBAR_WIDTH: u16 = 260;

const MAX_ID_LEN: usize = 128;
const MAX_NAME_LEN: usize = 256;
const MAX_PATH_LEN: usize = 4096;
const MAX_PROGRAM_LEN: usize = 4096;
const MAX_ARG_LEN: usize = 4096;
const MAX_ARGS: usize = 128;
const MAX_ENV_VARS: usize = 128;
const MAX_ENV_VALUE_LEN: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextActionsConfig {
    pub enabled: bool,
    pub panel_collapsed: bool,
    pub sidebar_width: u16,
    pub plugins: Vec<ContextPluginConfig>,
    /// Bounded navigation history used by the recent-directories provider.
    /// Paths are canonicalized by the app before they are recorded.
    pub directory_history: Vec<DirectoryHistoryEntry>,
}

impl Default for ContextActionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            panel_collapsed: false,
            sidebar_width: DEFAULT_CONTEXT_SIDEBAR_WIDTH,
            plugins: vec![
                ContextPluginConfig::new(RECENT_DIRECTORIES_PLUGIN_ID),
                ContextPluginConfig::new(MANIFEST_PLUGIN_ID),
                ContextPluginConfig::new(SPDEPLOY_PLUGIN_ID),
            ],
            directory_history: Vec::new(),
        }
    }
}

impl ContextActionsConfig {
    pub(crate) fn ensure_built_in_plugins(&mut self) {
        for id in BUILT_IN_CONTEXT_PLUGIN_IDS {
            if self.plugin(id).is_none() {
                self.plugins.push(ContextPluginConfig::new(*id));
            }
        }
        let needs_order_migration = BUILT_IN_CONTEXT_PLUGIN_IDS
            .iter()
            .filter_map(|id| self.plugin(id))
            .all(|plugin| plugin.order == 0);
        if needs_order_migration {
            for plugin in &mut self.plugins {
                if let Some(order) = built_in_plugin_order(&plugin.id) {
                    plugin.order = order;
                }
            }
        }
        self.plugins
            .sort_by(|left, right| left.order.cmp(&right.order).then(left.id.cmp(&right.id)));
    }

    pub fn plugin(&self, id: &str) -> Option<&ContextPluginConfig> {
        self.plugins.iter().find(|plugin| plugin.id == id)
    }

    pub fn plugin_mut(&mut self, id: &str) -> Option<&mut ContextPluginConfig> {
        self.plugins.iter_mut().find(|plugin| plugin.id == id)
    }

    pub fn validate(&self) -> AppResult<()> {
        if !(MIN_CONTEXT_SIDEBAR_WIDTH..=MAX_CONTEXT_SIDEBAR_WIDTH).contains(&self.sidebar_width) {
            return invalid(format!(
                "context sidebar width must be between {MIN_CONTEXT_SIDEBAR_WIDTH} and {MAX_CONTEXT_SIDEBAR_WIDTH}"
            ));
        }
        if self.plugins.len() > MAX_CONTEXT_PLUGINS {
            return invalid(format!(
                "no more than {MAX_CONTEXT_PLUGINS} context plugins are allowed"
            ));
        }
        let mut ids = HashSet::new();
        let mut orders = HashSet::new();
        for plugin in &self.plugins {
            plugin.validate()?;
            if !ids.insert(plugin.id.as_str()) {
                return invalid(format!("duplicate context plugin id '{}'", plugin.id));
            }
            if !orders.insert(plugin.order) {
                return invalid(format!("duplicate context plugin order '{}'", plugin.order));
            }
        }
        validate_directory_history(&self.directory_history)?;
        Ok(())
    }

    /// Record one visit to a canonical absolute directory.
    ///
    /// Existing entries retain their latest timestamp if the system clock
    /// moves backwards. When the bounded history fills, the stalest and then
    /// least-used entry is evicted deterministically.
    pub fn record_directory_visit(&mut self, path: &Path, visited_at: u64) -> AppResult<()> {
        let path = path
            .to_str()
            .ok_or_else(|| {
                AppError::InvalidConfig(
                    "directory history path must contain valid UTF-8".to_string(),
                )
            })?
            .to_string();
        validate_directory_path(&path)?;
        if let Some(entry) = self
            .directory_history
            .iter_mut()
            .find(|entry| entry.path == path)
        {
            entry.visit_count = entry.visit_count.saturating_add(1);
            entry.last_visited = entry.last_visited.max(visited_at);
            return Ok(());
        }

        self.directory_history.push(DirectoryHistoryEntry {
            path,
            visit_count: 1,
            last_visited: visited_at,
        });
        if self.directory_history.len() > MAX_DIRECTORY_HISTORY {
            let Some(evict) = self
                .directory_history
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| {
                    left.last_visited
                        .cmp(&right.last_visited)
                        .then_with(|| left.visit_count.cmp(&right.visit_count))
                        .then_with(|| right.path.cmp(&left.path))
                })
                .map(|(index, _)| index)
            else {
                return invalid("directory history eviction failed");
            };
            self.directory_history.remove(evict);
        }
        Ok(())
    }

    /// Select up to five most-recent and five most-frequent directories.
    /// Duplicate paths are included once, then the combined result is sorted
    /// case-insensitively by path for a predictable navigation list.
    pub fn selected_directories(&self) -> Vec<DirectoryHistoryEntry> {
        let mut recent = self.directory_history.clone();
        recent.sort_by(compare_directory_recency);
        recent.truncate(DIRECTORY_SELECTION_PER_SIGNAL);

        let mut frequent = self.directory_history.clone();
        frequent.sort_by(compare_directory_frequency);
        frequent.truncate(DIRECTORY_SELECTION_PER_SIGNAL);

        let mut selected = recent;
        let mut paths: HashSet<String> = selected.iter().map(|entry| entry.path.clone()).collect();
        selected.extend(
            frequent
                .into_iter()
                .filter(|entry| paths.insert(entry.path.clone())),
        );
        selected.sort_by(|left, right| {
            left.path
                .to_lowercase()
                .cmp(&right.path.to_lowercase())
                .then_with(|| left.path.cmp(&right.path))
        });
        selected
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryHistoryEntry {
    pub path: String,
    pub visit_count: u64,
    pub last_visited: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPluginConfig {
    pub id: String,
    #[serde(default)]
    pub order: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub section_collapsed: bool,
}

impl ContextPluginConfig {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            order: built_in_plugin_order(&id).unwrap_or(1_000),
            id,
            enabled: true,
            section_collapsed: false,
        }
    }

    fn validate(&self) -> AppResult<()> {
        validate_identifier("context plugin id", &self.id, MAX_ID_LEN)?;
        if self.order > MAX_CONTEXT_PLUGIN_ORDER {
            return invalid(format!(
                "context plugin order must be no more than {MAX_CONTEXT_PLUGIN_ORDER}"
            ));
        }
        if self.id == RECENT_DIRECTORIES_PLUGIN_ID && self.order != 0 {
            return invalid("recent-directories context plugin order must be 0");
        }
        if self.id != RECENT_DIRECTORIES_PLUGIN_ID && self.order == 0 {
            return invalid("only recent-directories may use context plugin order 0");
        }
        Ok(())
    }
}

fn built_in_plugin_order(id: &str) -> Option<u16> {
    match id {
        RECENT_DIRECTORIES_PLUGIN_ID => Some(RECENT_DIRECTORIES_PLUGIN_ORDER),
        MANIFEST_PLUGIN_ID => Some(MANIFEST_PLUGIN_ORDER),
        SPDEPLOY_PLUGIN_ID => Some(SPDEPLOY_PLUGIN_ORDER),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextManifest {
    pub version: u32,
    pub name: String,
    pub tabs: Vec<ContextTab>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextTab {
    pub id: String,
    pub title: String,
    #[serde(default = "default_dot")]
    pub cwd: String,
    #[serde(default)]
    pub run: Option<ContextRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRun {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedContextManifest {
    pub root: PathBuf,
    pub source: String,
    pub manifest: ContextManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedProject {
    pub root: String,
    pub manifest_source: String,
    pub name: String,
    pub tasks: Vec<TrustedContextTab>,
}

impl TrustedProject {
    pub fn validate(&self) -> AppResult<()> {
        validate_text("trusted project root", &self.root, MAX_PATH_LEN, false)?;
        if !Path::new(&self.root).is_absolute() {
            return invalid("trusted project root must be absolute");
        }
        validate_text("trusted project name", &self.name, MAX_NAME_LEN, false)?;
        validate_source(&self.manifest_source)?;
        let manifest = parse_context_manifest(&self.manifest_source)?;
        if manifest.name != self.name || manifest.tabs.len() != self.tasks.len() {
            return invalid("trusted project tasks do not match their exact manifest source");
        }
        if self.tasks.is_empty() {
            return invalid("trusted project must contain at least one task");
        }
        if self.tasks.len() > MAX_CONTEXT_TABS {
            return invalid(format!(
                "trusted project may contain no more than {MAX_CONTEXT_TABS} tasks"
            ));
        }
        let root = Path::new(&self.root);
        let mut ids = HashSet::new();
        for (task, source_tab) in self.tasks.iter().zip(&manifest.tabs) {
            task.validate(root)?;
            if task.id != source_tab.id
                || task.title != source_tab.title
                || task.source_cwd != source_tab.cwd
                || task.run != source_tab.run
            {
                return invalid("trusted project tasks do not match their exact manifest source");
            }
            if !ids.insert(task.id.as_str()) {
                return invalid(format!("duplicate trusted task id '{}'", task.id));
            }
        }
        Ok(())
    }

    pub fn task(&self, id: &str) -> Option<&TrustedContextTab> {
        self.tasks.iter().find(|task| task.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedContextTab {
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// Original relative path from the exact manifest source. Retaining it
    /// lets config validation prove that the persisted typed task was derived
    /// from that source rather than paired with it after the fact.
    pub source_cwd: String,
    #[serde(default)]
    pub run: Option<ContextRun>,
}

impl TrustedContextTab {
    fn validate(&self, root: &Path) -> AppResult<()> {
        validate_identifier("trusted task id", &self.id, MAX_ID_LEN)?;
        validate_text("trusted task title", &self.title, MAX_NAME_LEN, false)?;
        validate_text("trusted task cwd", &self.cwd, MAX_PATH_LEN, false)?;
        validate_text(
            "trusted task source cwd",
            &self.source_cwd,
            MAX_PATH_LEN,
            false,
        )?;
        let cwd = Path::new(&self.cwd);
        if !cwd.is_absolute() || !cwd.starts_with(root) {
            return invalid(format!(
                "trusted task '{}' cwd must stay within its project root",
                self.id
            ));
        }
        if let Some(run) = &self.run {
            run.validate()?;
        }
        Ok(())
    }
}

/// Read and validate the manifest in `root`. Missing manifests are not errors.
pub fn load_context_manifest(root: &Path) -> AppResult<Option<LoadedContextManifest>> {
    let root = canonical_directory(root, "context project root")?;
    let path = root.join(CONTEXT_MANIFEST_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return invalid(format!(
            "{} must be a regular file, not a symlink",
            path.display()
        ));
    }
    if metadata.len() > MAX_CONTEXT_MANIFEST_BYTES as u64 {
        return invalid(format!(
            "{} must be no more than {MAX_CONTEXT_MANIFEST_BYTES} bytes",
            path.display()
        ));
    }
    let source = fs::read_to_string(&path)?;
    validate_source(&source)?;
    let manifest = parse_context_manifest(&source)?;
    Ok(Some(LoadedContextManifest {
        root,
        source,
        manifest,
    }))
}

/// Parse a strict version-one manifest without granting execution trust.
pub fn parse_context_manifest(source: &str) -> AppResult<ContextManifest> {
    validate_source(source)?;
    let manifest: ContextManifest = noyalib::from_str(source)
        .map_err(|error| AppError::InvalidConfig(format!("invalid .phantom.yml: {error}")))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Validate the manifest in `root` through task cwd resolution without
/// creating or storing a trust record.
pub fn validate_context_manifest(root: &Path) -> AppResult<LoadedContextManifest> {
    let loaded = load_context_manifest(root)?.ok_or_else(|| {
        AppError::InvalidConfig(format!(
            "{} does not contain {CONTEXT_MANIFEST_FILE}",
            root.display()
        ))
    })?;
    validated_trusted_project(&loaded.root, loaded.source.clone(), loaded.manifest.clone())?;
    Ok(loaded)
}

/// Bind an exact manifest source and its typed tasks to a canonical root.
pub fn trust_context_manifest(root: &Path, source: String) -> AppResult<TrustedProject> {
    let root = canonical_directory(root, "context project root")?;
    let manifest = parse_context_manifest(&source)?;
    validated_trusted_project(&root, source, manifest)
}

fn validated_trusted_project(
    root: &Path,
    source: String,
    manifest: ContextManifest,
) -> AppResult<TrustedProject> {
    let tasks = resolve_context_tabs(root, &manifest)?;
    let trusted = TrustedProject {
        root: root.to_string_lossy().into_owned(),
        manifest_source: source,
        name: manifest.name,
        tasks,
    };
    trusted.validate()?;
    Ok(trusted)
}

fn resolve_context_tabs(
    root: &Path,
    manifest: &ContextManifest,
) -> AppResult<Vec<TrustedContextTab>> {
    manifest
        .tabs
        .iter()
        .map(|tab| {
            Ok(TrustedContextTab {
                id: tab.id.clone(),
                title: tab.title.clone(),
                cwd: resolve_task_cwd(root, &tab.cwd)?
                    .to_string_lossy()
                    .into_owned(),
                source_cwd: tab.cwd.clone(),
                run: tab.run.clone(),
            })
        })
        .collect()
}

/// Resolve a trusted task directly to structured process launch options.
///
/// Both canonical root and exact source must still match at execution time.
pub fn resolve_trusted_task(
    project: &TrustedProject,
    observed_root: &Path,
    observed_source: &str,
    task_id: &str,
    rows: u16,
    cols: u16,
) -> AppResult<LaunchOpts> {
    project.validate()?;
    let root = canonical_directory(observed_root, "context project root")?;
    if root != Path::new(&project.root) {
        return invalid("trusted project root no longer matches the current project");
    }
    if observed_source != project.manifest_source {
        return invalid(".phantom.yml changed after it was trusted");
    }
    let task = project
        .task(task_id)
        .ok_or_else(|| AppError::InvalidConfig(format!("unknown trusted task '{task_id}'")))?;
    let cwd = canonical_directory(Path::new(&task.cwd), "trusted task cwd")?;
    if !cwd.starts_with(&root) || cwd != Path::new(&task.cwd) {
        return invalid(format!(
            "trusted task '{}' cwd no longer matches its trusted path",
            task.id
        ));
    }
    let (command, args, env) = match &task.run {
        Some(run) => {
            run.validate()?;
            (Some(run.program.clone()), run.args.clone(), run.env.clone())
        }
        None => (None, Vec::new(), BTreeMap::new()),
    };
    Ok(LaunchOpts {
        command,
        args,
        env,
        cwd: Some(task.cwd.clone()),
        rows,
        cols,
    })
}

impl ContextManifest {
    fn validate(&self) -> AppResult<()> {
        if self.version != 1 {
            return invalid(format!(
                "unsupported .phantom.yml version {}; expected 1",
                self.version
            ));
        }
        validate_text("manifest name", &self.name, MAX_NAME_LEN, false)?;
        if self.tabs.is_empty() {
            return invalid(".phantom.yml must contain at least one tab");
        }
        if self.tabs.len() > MAX_CONTEXT_TABS {
            return invalid(format!(
                ".phantom.yml may contain no more than {MAX_CONTEXT_TABS} tabs"
            ));
        }
        let mut ids = HashSet::new();
        for tab in &self.tabs {
            tab.validate()?;
            if !ids.insert(tab.id.as_str()) {
                return invalid(format!("duplicate context tab id '{}'", tab.id));
            }
        }
        Ok(())
    }
}

impl ContextTab {
    fn validate(&self) -> AppResult<()> {
        validate_identifier("context tab id", &self.id, MAX_ID_LEN)?;
        validate_text("context tab title", &self.title, MAX_NAME_LEN, false)?;
        validate_text("context tab cwd", &self.cwd, MAX_PATH_LEN, false)?;
        let cwd = Path::new(&self.cwd);
        if cwd.is_absolute()
            || cwd.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return invalid(format!(
                "context tab '{}' cwd must be relative and stay within the project root",
                self.id
            ));
        }
        if let Some(run) = &self.run {
            run.validate()?;
        }
        Ok(())
    }
}

impl ContextRun {
    fn validate(&self) -> AppResult<()> {
        validate_text(
            "context task program",
            &self.program,
            MAX_PROGRAM_LEN,
            false,
        )?;
        if self.args.len() > MAX_ARGS {
            return invalid(format!(
                "context task may contain no more than {MAX_ARGS} arguments"
            ));
        }
        for arg in &self.args {
            validate_text("context task argument", arg, MAX_ARG_LEN, true)?;
        }
        if self.env.len() > MAX_ENV_VARS {
            return invalid(format!(
                "context task may contain no more than {MAX_ENV_VARS} environment variables"
            ));
        }
        for (key, value) in &self.env {
            validate_env_key(key)?;
            validate_text(
                "context task environment value",
                value,
                MAX_ENV_VALUE_LEN,
                true,
            )?;
        }
        Ok(())
    }
}

fn resolve_task_cwd(root: &Path, relative: &str) -> AppResult<PathBuf> {
    let target = canonical_directory(&root.join(relative), "context task cwd")?;
    if !target.starts_with(root) {
        return invalid("context task cwd escapes the project root");
    }
    Ok(target)
}

fn validate_directory_history(history: &[DirectoryHistoryEntry]) -> AppResult<()> {
    if history.len() > MAX_DIRECTORY_HISTORY {
        return invalid(format!(
            "directory history may contain no more than {MAX_DIRECTORY_HISTORY} entries"
        ));
    }
    let mut paths = HashSet::new();
    for entry in history {
        validate_directory_path(&entry.path)?;
        if entry.visit_count == 0 {
            return invalid(format!(
                "directory history visit count for '{}' must be at least 1",
                entry.path
            ));
        }
        if !paths.insert(entry.path.as_str()) {
            return invalid(format!("duplicate directory history path '{}'", entry.path));
        }
    }
    Ok(())
}

fn validate_directory_path(path: &str) -> AppResult<()> {
    validate_text("directory history path", path, MAX_PATH_LEN, false)?;
    if !Path::new(path).is_absolute() {
        return invalid("directory history path must be absolute");
    }
    Ok(())
}

fn compare_directory_recency(
    left: &DirectoryHistoryEntry,
    right: &DirectoryHistoryEntry,
) -> std::cmp::Ordering {
    right
        .last_visited
        .cmp(&left.last_visited)
        .then_with(|| right.visit_count.cmp(&left.visit_count))
        .then_with(|| left.path.cmp(&right.path))
}

fn compare_directory_frequency(
    left: &DirectoryHistoryEntry,
    right: &DirectoryHistoryEntry,
) -> std::cmp::Ordering {
    right
        .visit_count
        .cmp(&left.visit_count)
        .then_with(|| right.last_visited.cmp(&left.last_visited))
        .then_with(|| left.path.cmp(&right.path))
}

fn canonical_directory(path: &Path, name: &str) -> AppResult<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        AppError::InvalidConfig(format!("{name} '{}' is invalid: {error}", path.display()))
    })?;
    if !canonical.is_dir() {
        return invalid(format!("{name} '{}' is not a directory", path.display()));
    }
    Ok(canonical)
}

fn validate_source(source: &str) -> AppResult<()> {
    if source.is_empty() {
        return invalid(".phantom.yml cannot be empty");
    }
    if source.len() > MAX_CONTEXT_MANIFEST_BYTES {
        return invalid(format!(
            ".phantom.yml must be no more than {MAX_CONTEXT_MANIFEST_BYTES} bytes"
        ));
    }
    if source.contains('\0') {
        return invalid(".phantom.yml cannot contain NUL bytes");
    }
    Ok(())
}

fn validate_identifier(name: &str, value: &str, max: usize) -> AppResult<()> {
    validate_text(name, value, max, false)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return invalid(format!(
            "{name} may contain only ASCII letters, digits, '-' and '_'"
        ));
    }
    Ok(())
}

fn validate_text(name: &str, value: &str, max: usize, allow_empty: bool) -> AppResult<()> {
    if !allow_empty && value.trim().is_empty() {
        return invalid(format!("{name} cannot be empty"));
    }
    if value.len() > max {
        return invalid(format!("{name} must be at most {max} bytes"));
    }
    if value.contains('\0') {
        return invalid(format!("{name} cannot contain NUL bytes"));
    }
    if value.chars().any(char::is_control) {
        return invalid(format!("{name} cannot contain control characters"));
    }
    Ok(())
}

fn validate_env_key(key: &str) -> AppResult<()> {
    if key.is_empty() || key.len() > MAX_ID_LEN {
        return invalid(format!(
            "context task environment key must contain 1 to {MAX_ID_LEN} bytes"
        ));
    }
    let mut chars = key.chars();
    if !chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        || !chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return invalid(format!("invalid context task environment key '{key}'"));
    }
    let upper = key.to_ascii_uppercase();
    let reserved = matches!(
        upper.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "TERM"
            | "COLORTERM"
            | "TERM_PROGRAM"
            | "TERM_PROGRAM_VERSION"
            | "CLICOLOR"
            | "LSCOLORS"
    ) || upper.starts_with("LD_")
        || upper.starts_with("DYLD_");
    if reserved {
        return invalid(format!(
            "context task may not override reserved environment variable '{key}'"
        ));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> AppResult<T> {
    Err(AppError::InvalidConfig(message.into()))
}

const fn default_true() -> bool {
    true
}

fn default_dot() -> String {
    ".".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const VALID: &str = r#"version: 1
name: Soulfire
tabs:
  - id: api
    title: API
    cwd: soulfire-api
    run:
      program: cargo
      args: [run]
      env:
        RUST_LOG: info
  - id: deploy
    title: Deploy
    cwd: .
"#;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("phantom-context-{}-{unique}", std::process::id()));
            fs::create_dir_all(path.join("soulfire-api")).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_strict_v1_manifest() {
        let manifest = parse_context_manifest(VALID).unwrap();
        assert_eq!(manifest.name, "Soulfire");
        assert_eq!(manifest.tabs.len(), 2);
        assert_eq!(manifest.tabs[0].run.as_ref().unwrap().program, "cargo");
    }

    #[test]
    fn read_only_validation_resolves_every_task_directory() {
        let dir = TestDir::new();
        fs::write(dir.0.join(CONTEXT_MANIFEST_FILE), VALID).unwrap();
        let loaded = validate_context_manifest(&dir.0).unwrap();
        assert_eq!(loaded.manifest.name, "Soulfire");
        assert_eq!(loaded.manifest.tabs.len(), 2);

        fs::remove_dir_all(dir.0.join("soulfire-api")).unwrap();
        assert!(validate_context_manifest(&dir.0).is_err());
    }

    #[test]
    fn read_only_validation_requires_a_manifest() {
        let dir = TestDir::new();
        assert!(validate_context_manifest(&dir.0).is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_versions() {
        assert!(parse_context_manifest(&VALID.replace("name:", "extra: true\nname:")).is_err());
        assert!(parse_context_manifest(&VALID.replace("version: 1", "version: 2")).is_err());
    }

    #[test]
    fn rejects_duplicate_ids_and_oversized_manifests() {
        let duplicate = VALID.replace("id: deploy", "id: api");
        assert!(parse_context_manifest(&duplicate).is_err());
        assert!(parse_context_manifest(&"x".repeat(MAX_CONTEXT_MANIFEST_BYTES + 1)).is_err());
    }

    #[test]
    fn rejects_absolute_and_parent_cwds() {
        assert!(parse_context_manifest(&VALID.replace("soulfire-api", "/tmp")).is_err());
        assert!(parse_context_manifest(&VALID.replace("soulfire-api", "../outside")).is_err());
    }

    #[test]
    fn rejects_invalid_and_reserved_environment_keys() {
        assert!(parse_context_manifest(&VALID.replace("RUST_LOG", "BAD-KEY")).is_err());
        assert!(parse_context_manifest(&VALID.replace("RUST_LOG", "PATH")).is_err());
        assert!(
            parse_context_manifest(&VALID.replace("RUST_LOG", "DYLD_INSERT_LIBRARIES")).is_err()
        );
    }

    #[test]
    fn rejects_control_characters_that_could_spoof_the_trust_review() {
        assert!(parse_context_manifest(
            &VALID.replace("program: cargo", "program: \"cargo\\nrm\"")
        )
        .is_err());
        assert!(parse_context_manifest(
            &VALID.replace("RUST_LOG: info", "RUST_LOG: \"info\\nPATH=/tmp\"")
        )
        .is_err());
    }

    #[test]
    fn trust_binds_canonical_root_source_and_typed_tasks() {
        let dir = TestDir::new();
        let trusted = trust_context_manifest(&dir.0, VALID.to_string()).unwrap();
        assert_eq!(
            trusted.root,
            fs::canonicalize(&dir.0).unwrap().to_string_lossy()
        );
        assert!(Path::new(&trusted.tasks[0].cwd).is_absolute());

        let launch = resolve_trusted_task(&trusted, &dir.0, VALID, "api", 40, 120).unwrap();
        assert_eq!(launch.command.as_deref(), Some("cargo"));
        assert_eq!(launch.args, ["run"]);
        assert_eq!(launch.env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert_eq!((launch.rows, launch.cols), (40, 120));
    }

    #[test]
    fn source_or_root_change_invalidates_trust() {
        let dir = TestDir::new();
        let other = TestDir::new();
        let trusted = trust_context_manifest(&dir.0, VALID.to_string()).unwrap();
        assert!(resolve_trusted_task(&trusted, &dir.0, "# changed\n", "api", 24, 80).is_err());
        assert!(resolve_trusted_task(&trusted, &other.0, VALID, "api", 24, 80).is_err());
    }

    #[test]
    fn persisted_typed_task_must_match_exact_source() {
        let dir = TestDir::new();
        let mut trusted = trust_context_manifest(&dir.0, VALID.to_string()).unwrap();
        trusted.tasks[0].run.as_mut().unwrap().program = "different".to_string();

        assert!(trusted.validate().is_err());
    }

    #[test]
    fn shell_task_resolves_without_direct_command() {
        let dir = TestDir::new();
        let trusted = trust_context_manifest(&dir.0, VALID.to_string()).unwrap();
        let launch = resolve_trusted_task(&trusted, &dir.0, VALID, "deploy", 24, 80).unwrap();
        assert_eq!(launch.command, None);
        assert!(launch.args.is_empty());
        assert!(launch.env.is_empty());
    }

    #[test]
    fn missing_manifest_is_none_and_regular_file_loads() {
        let dir = TestDir::new();
        assert!(load_context_manifest(&dir.0).unwrap().is_none());
        fs::write(dir.0.join(CONTEXT_MANIFEST_FILE), VALID).unwrap();
        let loaded = load_context_manifest(&dir.0).unwrap().unwrap();
        assert_eq!(loaded.source, VALID);
        assert_eq!(loaded.manifest.name, "Soulfire");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_manifest_symlinks_and_task_symlink_escape() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new();
        let outside = TestDir::new();
        let external_manifest = outside.0.join("manifest.yml");
        fs::write(&external_manifest, VALID).unwrap();
        symlink(&external_manifest, dir.0.join(CONTEXT_MANIFEST_FILE)).unwrap();
        assert!(load_context_manifest(&dir.0).is_err());
        fs::remove_file(dir.0.join(CONTEXT_MANIFEST_FILE)).unwrap();

        fs::remove_dir_all(dir.0.join("soulfire-api")).unwrap();
        symlink(&outside.0, dir.0.join("soulfire-api")).unwrap();
        assert!(trust_context_manifest(&dir.0, VALID.to_string()).is_err());
    }

    #[test]
    fn plugin_config_requires_bounded_unique_ids() {
        let mut config = ContextActionsConfig::default();
        config
            .plugins
            .push(ContextPluginConfig::new(MANIFEST_PLUGIN_ID));
        assert!(config.validate().is_err());
        config.plugins.pop();
        config.plugins[0].id = "bad id".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn default_registers_every_built_in_context_plugin() {
        let config = ContextActionsConfig::default();
        let ids: Vec<_> = config
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect();

        assert_eq!(ids, BUILT_IN_CONTEXT_PLUGIN_IDS);
        let orders: Vec<_> = config.plugins.iter().map(|plugin| plugin.order).collect();
        assert_eq!(
            orders,
            [
                RECENT_DIRECTORIES_PLUGIN_ORDER,
                MANIFEST_PLUGIN_ORDER,
                SPDEPLOY_PLUGIN_ORDER,
            ]
        );
        assert!(config.directory_history.is_empty());
    }

    #[test]
    fn older_context_config_defaults_missing_directory_history() {
        let mut value = serde_json::to_value(ContextActionsConfig::default()).unwrap();
        value.as_object_mut().unwrap().remove("directory_history");

        let config: ContextActionsConfig = serde_json::from_value(value).unwrap();

        assert!(config.directory_history.is_empty());
        config.validate().unwrap();
    }

    #[test]
    fn records_visits_and_keeps_history_bounded() {
        let mut config = ContextActionsConfig::default();
        config
            .record_directory_visit(Path::new("/projects/soulfire"), 10)
            .unwrap();
        config
            .record_directory_visit(Path::new("/projects/soulfire"), 9)
            .unwrap();
        assert_eq!(
            config.directory_history,
            [DirectoryHistoryEntry {
                path: "/projects/soulfire".to_string(),
                visit_count: 2,
                last_visited: 10,
            }]
        );

        for index in 0..=MAX_DIRECTORY_HISTORY {
            config
                .record_directory_visit(Path::new(&format!("/history/{index}")), 100 + index as u64)
                .unwrap();
        }
        assert_eq!(config.directory_history.len(), MAX_DIRECTORY_HISTORY);
        assert!(config
            .directory_history
            .iter()
            .all(|entry| entry.path != "/projects/soulfire"));
        config.validate().unwrap();
    }

    #[test]
    fn selects_top_five_recent_and_frequent_then_sorts_together() {
        let config = ContextActionsConfig {
            directory_history: vec![
                history("/frequent/a", 100, 10),
                history("/frequent/b", 90, 20),
                history("/frequent/c", 80, 30),
                history("/frequent/d", 70, 40),
                history("/frequent/e", 60, 50),
                history("/recent/f", 1, 100),
                history("/recent/g", 1, 99),
                history("/recent/h", 1, 98),
                history("/recent/i", 1, 97),
                history("/recent/j", 1, 96),
            ],
            ..ContextActionsConfig::default()
        };

        let selected: Vec<_> = config
            .selected_directories()
            .into_iter()
            .map(|entry| entry.path)
            .collect();

        assert_eq!(
            selected,
            [
                "/frequent/a",
                "/frequent/b",
                "/frequent/c",
                "/frequent/d",
                "/frequent/e",
                "/recent/f",
                "/recent/g",
                "/recent/h",
                "/recent/i",
                "/recent/j",
            ]
        );
    }

    #[test]
    fn selection_deduplicates_directories_that_rank_in_both_signals() {
        let config = ContextActionsConfig {
            directory_history: (0..5)
                .map(|index| history(&format!("/project/{index}"), 10 - index, 20 - index))
                .collect(),
            ..ContextActionsConfig::default()
        };

        let selected = config.selected_directories();

        assert_eq!(selected.len(), 5);
        assert_eq!(selected[0].path, "/project/0");
    }

    #[test]
    fn selected_directories_are_case_insensitively_sorted_by_path() {
        let config = ContextActionsConfig {
            directory_history: vec![
                history("/Zoo", 10, 30),
                history("/alpha", 1, 20),
                history("/Beta", 1, 10),
            ],
            ..ContextActionsConfig::default()
        };

        let paths: Vec<_> = config
            .selected_directories()
            .into_iter()
            .map(|entry| entry.path)
            .collect();

        assert_eq!(paths, ["/alpha", "/Beta", "/Zoo"]);
    }

    #[test]
    fn directory_history_validation_rejects_illegal_states() {
        let invalid_path = ContextActionsConfig {
            directory_history: vec![history("relative/path", 1, 1)],
            ..ContextActionsConfig::default()
        };
        assert!(invalid_path.validate().is_err());

        let zero_visits = ContextActionsConfig {
            directory_history: vec![history("/project", 0, 1)],
            ..ContextActionsConfig::default()
        };
        assert!(zero_visits.validate().is_err());

        let duplicate = ContextActionsConfig {
            directory_history: vec![history("/project", 1, 1), history("/project", 2, 2)],
            ..ContextActionsConfig::default()
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn plugin_order_validation_keeps_directories_first_and_orders_unique() {
        let mut misplaced_directories = ContextActionsConfig::default();
        misplaced_directories
            .plugin_mut(RECENT_DIRECTORIES_PLUGIN_ID)
            .unwrap()
            .order = 50;
        assert!(misplaced_directories.validate().is_err());

        let mut duplicate_order = ContextActionsConfig::default();
        duplicate_order
            .plugin_mut(SPDEPLOY_PLUGIN_ID)
            .unwrap()
            .order = MANIFEST_PLUGIN_ORDER;
        assert!(duplicate_order.validate().is_err());
    }

    #[test]
    fn sidebar_width_is_bounded() {
        let too_narrow = ContextActionsConfig {
            sidebar_width: MIN_CONTEXT_SIDEBAR_WIDTH - 1,
            ..ContextActionsConfig::default()
        };
        assert!(too_narrow.validate().is_err());

        let too_wide = ContextActionsConfig {
            sidebar_width: MAX_CONTEXT_SIDEBAR_WIDTH + 1,
            ..ContextActionsConfig::default()
        };
        assert!(too_wide.validate().is_err());
    }

    fn history(path: &str, visit_count: u64, last_visited: u64) -> DirectoryHistoryEntry {
        DirectoryHistoryEntry {
            path: path.to_string(),
            visit_count,
            last_visited,
        }
    }
}
