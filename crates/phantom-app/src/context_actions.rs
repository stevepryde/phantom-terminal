//! Directory-aware action discovery and the typed model consumed by egui.
//!
//! Providers in this module are compiled into Phantom. Discovery is read-only:
//! it inspects bounded local files, but never launches a project task, deploy
//! operation, or discovery subprocess.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use noyalib::Value;
use phantom_core::{
    load_context_manifest_source, parse_context_manifest, read_bounded_regular_file,
    trust_context_manifest, ContextActionsConfig, ContextRun, TrustedProject,
    BUILT_IN_CONTEXT_PLUGIN_IDS, CONTEXT_MANIFEST_FILE, RECENT_DIRECTORIES_PLUGIN_ID,
};

pub const MANIFEST_PROVIDER_ID: &str = "phantom-manifest";
pub const SPDEPLOY_PROVIDER_ID: &str = "spdeploy";

const DEPLOY_FILE: &str = "deploy.yml";
const MAX_CONFIG_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_CONFIGS: usize = 32;
const MAX_DEPTH: usize = 8;
const MAX_OPERATIONS: usize = 256;
const MAX_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_ERROR_BYTES: usize = 512;

/// A complete discovery result for one active-tab working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSnapshot {
    pub cwd: PathBuf,
    pub sections: Vec<ContextSection>,
}

impl ContextSnapshot {
    pub fn empty(cwd: PathBuf) -> Self {
        Self {
            cwd,
            sections: Vec::new(),
        }
    }
}

/// One independently collapsible provider section in the contextual overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSection {
    pub id: String,
    pub title: String,
    pub content: ContextSectionContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextSectionContent {
    Manifest(ManifestSection),
    Spdeploy(SpdeploySection),
    RecentDirectories(RecentDirectoriesSection),
    FrequentCommands(FrequentCommandsSection),
    ManifestError(ManifestErrorSection),
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestErrorSection {
    pub root: PathBuf,
    /// Exact bounded source observed when parsing failed.
    pub manifest_source: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrequentCommandsSection {
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentDirectoriesSection {
    pub directories: Vec<RecentDirectory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentDirectory {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestTrustState {
    NeedsTrust,
    Trusted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestSection {
    pub manifest_path: PathBuf,
    pub root: PathBuf,
    /// Exact bounded manifest source used by the core trust comparison.
    pub manifest_source: String,
    pub name: String,
    pub trust: ManifestTrustState,
    pub tabs: Vec<ManifestTab>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTab {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub task: Option<ManifestTask>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTask {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpdeploySection {
    pub config_path: PathBuf,
    pub project_name: String,
    pub operations: Vec<SpdeployOperation>,
    /// Exact bounded bytes of every config used to construct `operations`.
    /// Dispatch re-reads and compares all of them before launching spdeploy.
    pub config_sources: Vec<ConfigSourceStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSourceStamp {
    pub path: PathBuf,
    pub source: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpdeployOperation {
    /// Stable operation name accepted by `spdeploy --operation`.
    pub name: String,
    /// Nested submenu names leading to this leaf, excluding the leaf itself.
    pub breadcrumbs: Vec<String>,
    pub description: Option<String>,
    /// Exact config declaring this leaf operation.
    pub config_path: PathBuf,
}

/// User intent returned by egui. `App` must revalidate manifest trust or use
/// the fixed spdeploy argv shape immediately before dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextRequest {
    TrustManifest {
        root: PathBuf,
        manifest_source: String,
    },
    EditManifest {
        root: PathBuf,
        manifest_source: String,
    },
    OpenManifestAll {
        root: PathBuf,
        manifest_source: String,
    },
    OpenManifestTab {
        root: PathBuf,
        manifest_source: String,
        tab_id: String,
    },
    RunSpdeploy {
        config_path: PathBuf,
        operation: String,
    },
    OpenDirectory {
        path: PathBuf,
    },
}

/// Run every enabled compiled-in provider for `cwd`. This function is
/// synchronous and read-only; callers execute it on the background discovery
/// worker so bounded filesystem parsing never interrupts terminal rendering.
pub fn discover_context(
    cwd: &Path,
    config: &ContextActionsConfig,
    trusted_projects: &[TrustedProject],
) -> ContextSnapshot {
    let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut snapshot = ContextSnapshot::empty(canonical_cwd.clone());
    if !config.enabled {
        return snapshot;
    }

    let mut providers = BUILT_IN_CONTEXT_PLUGIN_IDS.to_vec();
    providers.sort_by(|left, right| {
        let left_order = config.plugin(left).map_or(u16::MAX, |plugin| plugin.order);
        let right_order = config.plugin(right).map_or(u16::MAX, |plugin| plugin.order);
        left_order.cmp(&right_order).then(left.cmp(right))
    });
    for provider in providers {
        let enabled = config.plugin(provider).is_some_and(|plugin| plugin.enabled);
        if !enabled {
            continue;
        }
        let section = match provider {
            MANIFEST_PROVIDER_ID => discover_manifest(&canonical_cwd, trusted_projects),
            SPDEPLOY_PROVIDER_ID => discover_spdeploy(&canonical_cwd),
            RECENT_DIRECTORIES_PLUGIN_ID => discover_recent_directories(config),
            _ => None,
        };
        if let Some(section) = section {
            snapshot.sections.push(section);
        }
    }
    snapshot
}

fn discover_recent_directories(config: &ContextActionsConfig) -> Option<ContextSection> {
    let directories: Vec<_> = config
        .selected_directories()
        .into_iter()
        .map(|entry| RecentDirectory {
            path: PathBuf::from(entry.path),
        })
        .collect();
    if directories.is_empty() {
        return None;
    }
    Some(ContextSection {
        id: RECENT_DIRECTORIES_PLUGIN_ID.to_string(),
        title: "Directories".to_string(),
        content: ContextSectionContent::RecentDirectories(RecentDirectoriesSection { directories }),
    })
}

pub fn discover_manifest(
    cwd: &Path,
    trusted_projects: &[TrustedProject],
) -> Option<ContextSection> {
    let loaded = match load_context_manifest_source(cwd) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return None,
        Err(error) => return Some(manifest_error(error.to_string(), None)),
    };
    let manifest = match parse_context_manifest(&loaded.source) {
        Ok(manifest) => manifest,
        Err(error) => {
            return Some(manifest_error(
                error.to_string(),
                Some((loaded.root, loaded.source)),
            ))
        }
    };
    let proposal = match trust_context_manifest(&loaded.root, loaded.source.clone()) {
        Ok(project) => project,
        Err(error) => {
            return Some(manifest_error(
                error.to_string(),
                Some((loaded.root, loaded.source)),
            ))
        }
    };
    let trusted = trusted_projects.iter().any(|project| {
        Path::new(&project.root) == loaded.root && project.manifest_source == loaded.source
    });
    let tabs = proposal
        .tasks
        .into_iter()
        .map(|tab| ManifestTab {
            id: tab.id,
            title: tab.title,
            cwd: PathBuf::from(tab.cwd),
            task: tab.run.map(manifest_task),
        })
        .collect();

    Some(ContextSection {
        id: MANIFEST_PROVIDER_ID.to_string(),
        title: manifest.name.clone(),
        content: ContextSectionContent::Manifest(ManifestSection {
            manifest_path: loaded.root.join(CONTEXT_MANIFEST_FILE),
            root: loaded.root,
            manifest_source: loaded.source,
            name: manifest.name,
            trust: if trusted {
                ManifestTrustState::Trusted
            } else {
                ManifestTrustState::NeedsTrust
            },
            tabs,
        }),
    })
}

fn manifest_task(run: ContextRun) -> ManifestTask {
    ManifestTask {
        program: run.program,
        args: run.args,
        env: run.env.into_iter().collect(),
    }
}

fn manifest_error(message: String, source: Option<(PathBuf, String)>) -> ContextSection {
    ContextSection {
        id: MANIFEST_PROVIDER_ID.to_string(),
        title: ".phantom.yml".to_string(),
        content: match source {
            Some((root, manifest_source)) => {
                ContextSectionContent::ManifestError(ManifestErrorSection {
                    root,
                    manifest_source,
                    message: bounded_text(&message, MAX_ERROR_BYTES),
                })
            }
            None => ContextSectionContent::Error {
                message: bounded_text(&message, MAX_ERROR_BYTES),
            },
        },
    }
}

pub fn discover_spdeploy(cwd: &Path) -> Option<ContextSection> {
    let candidate = cwd.join(DEPLOY_FILE);
    match candidate.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            return Some(spdeploy_error(format!(
                "Could not inspect deploy.yml: {error}"
            )))
        }
    }

    let root = match cwd.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            return Some(spdeploy_error(format!(
                "Could not resolve working directory: {error}"
            )))
        }
    };
    let config_path = root.join(DEPLOY_FILE);

    match discover_spdeploy_tree(&root, &config_path) {
        Ok((project_name, operations, config_sources)) => Some(ContextSection {
            id: SPDEPLOY_PROVIDER_ID.to_string(),
            title: "spdeploy".to_string(),
            content: ContextSectionContent::Spdeploy(SpdeploySection {
                config_path,
                project_name,
                operations,
                config_sources,
            }),
        }),
        Err(error) => Some(spdeploy_error(error)),
    }
}

fn discover_spdeploy_tree(
    root: &Path,
    config_path: &Path,
) -> Result<(String, Vec<SpdeployOperation>, Vec<ConfigSourceStamp>), String> {
    let mut operations = Vec::new();
    let mut config_sources = Vec::new();
    let mut active_configs = HashSet::new();
    let mut config_count = 0;
    let project_name = collect_spdeploy_config(
        root,
        config_path,
        0,
        &[],
        &mut active_configs,
        &mut config_count,
        &mut operations,
        &mut config_sources,
    )?;
    validate_spdeploy_config_sources(&config_sources)?;
    Ok((project_name, operations, config_sources))
}

#[allow(clippy::too_many_arguments)]
fn collect_spdeploy_config(
    root: &Path,
    config_path: &Path,
    depth: usize,
    breadcrumbs: &[String],
    active_configs: &mut HashSet<PathBuf>,
    config_count: &mut usize,
    operations: &mut Vec<SpdeployOperation>,
    config_sources: &mut Vec<ConfigSourceStamp>,
) -> Result<String, String> {
    if depth > MAX_DEPTH {
        return Err(format!("spdeploy submenu depth exceeds {MAX_DEPTH}"));
    }
    *config_count += 1;
    if *config_count > MAX_CONFIGS {
        return Err(format!("spdeploy config count exceeds {MAX_CONFIGS}"));
    }
    if !active_configs.insert(config_path.to_path_buf()) {
        return Err("spdeploy submenu cycle detected".to_string());
    }

    let result = (|| {
        stamp_config_source(config_path, config_sources)?;
        let source = config_sources
            .iter()
            .find(|stamp| stamp.path == config_path)
            .map(|stamp| stamp.source.as_slice())
            .ok_or_else(|| "spdeploy config source stamp is missing".to_string())?;
        let value: Value = noyalib::from_slice(source)
            .map_err(|error| format!("Could not parse {}: {error}", config_path.display()))?;
        let object = value
            .as_mapping()
            .ok_or_else(|| "spdeploy config must be a YAML mapping".to_string())?;
        let project_name =
            required_bounded_string(object.get("name"), "project name", MAX_NAME_BYTES)?;
        let listed = object
            .get("operation")
            .and_then(Value::as_mapping)
            .ok_or_else(|| "spdeploy config has no operation mapping".to_string())?;

        for (name, operation) in listed {
            validate_tool_text(name, "operation name", MAX_NAME_BYTES)?;
            let operation = operation
                .as_mapping()
                .ok_or_else(|| "spdeploy operation must be an object".to_string())?;
            let description = optional_bounded_string(
                operation.get("description"),
                "operation description",
                MAX_DESCRIPTION_BYTES,
            )?;
            let stages = operation
                .get("stage")
                .and_then(Value::as_sequence)
                .ok_or_else(|| format!("spdeploy operation '{name}' has no stages array"))?;

            if let Some(child) = submenu_path(stages)? {
                let child_path = resolve_child_config(root, config_path, &child)?;
                let mut child_breadcrumbs = breadcrumbs.to_vec();
                child_breadcrumbs.push(name.clone());
                collect_spdeploy_config(
                    root,
                    &child_path,
                    depth + 1,
                    &child_breadcrumbs,
                    active_configs,
                    config_count,
                    operations,
                    config_sources,
                )?;
                continue;
            }

            if operations.len() >= MAX_OPERATIONS {
                return Err(format!("spdeploy operation count exceeds {MAX_OPERATIONS}"));
            }
            operations.push(SpdeployOperation {
                name: name.clone(),
                breadcrumbs: breadcrumbs.to_vec(),
                description,
                config_path: config_path.to_path_buf(),
            });
        }
        Ok(project_name)
    })();

    active_configs.remove(config_path);
    result
}

fn submenu_path(stages: &[Value]) -> Result<Option<String>, String> {
    if stages.len() != 1 {
        return Ok(None);
    }
    let Some(stage) = stages[0].as_mapping() else {
        return Err("spdeploy stage must be an object".to_string());
    };
    if stage.get("type").and_then(Value::as_str) != Some("deploy")
        || stage.get("operation").is_some_and(|value| !value.is_null())
    {
        return Ok(None);
    }
    let path = required_bounded_string(stage.get("path"), "submenu path", MAX_PATH_BYTES)?;
    Ok(Some(path))
}

fn resolve_child_config(root: &Path, parent: &Path, child: &str) -> Result<PathBuf, String> {
    if child.contains('\0') {
        return Err("spdeploy submenu path contains a NUL byte".to_string());
    }
    let child = Path::new(child);
    let candidate = if child.is_absolute() {
        child.to_path_buf()
    } else {
        parent.parent().unwrap_or(root).join(child)
    };
    let candidate = normalize_path(&candidate)?;
    if !candidate.starts_with(root) {
        return Err(format!(
            "spdeploy submenu '{}' resolves outside the active directory",
            child.display()
        ));
    }
    Ok(candidate)
}

fn normalize_path(path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err("spdeploy config path must be absolute".to_string());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("spdeploy config path escapes its filesystem root".to_string());
                }
            }
            Component::Normal(name) => normalized.push(name),
        }
    }
    Ok(normalized)
}

fn stamp_config_source(
    config_path: &Path,
    sources: &mut Vec<ConfigSourceStamp>,
) -> Result<(), String> {
    if sources.iter().any(|stamp| stamp.path == config_path) {
        return verify_config_source(config_path, sources);
    }
    let used = sources.iter().try_fold(0usize, |total, stamp| {
        total
            .checked_add(stamp.source.len())
            .ok_or_else(|| "spdeploy config sources are too large".to_string())
    })?;
    let remaining = MAX_CONFIG_SOURCE_BYTES
        .checked_sub(used)
        .ok_or_else(|| "spdeploy config sources are too large".to_string())?;
    let source = read_file_bounded(config_path, remaining)?;
    sources.push(ConfigSourceStamp {
        path: config_path.to_path_buf(),
        source,
    });
    Ok(())
}

/// Re-read every bounded config source used by discovery. App calls this at
/// dispatch so a changed root or nested config cannot reuse stale operations.
pub fn verify_spdeploy_sources(section: &SpdeploySection) -> Result<(), String> {
    let stamped = |path: &Path| {
        section
            .config_sources
            .iter()
            .any(|stamp| stamp.path == path)
    };
    if !stamped(&section.config_path)
        || section
            .operations
            .iter()
            .any(|operation| !stamped(&operation.config_path))
    {
        return Err("spdeploy config source stamps are incomplete".to_string());
    }
    validate_spdeploy_config_sources(&section.config_sources)
}

fn validate_spdeploy_config_sources(sources: &[ConfigSourceStamp]) -> Result<(), String> {
    if sources.is_empty() || sources.len() > MAX_CONFIGS {
        return Err("spdeploy config source stamps are incomplete".to_string());
    }
    let total = sources.iter().try_fold(0usize, |total, stamp| {
        total
            .checked_add(stamp.source.len())
            .ok_or_else(|| "spdeploy config sources are too large".to_string())
    })?;
    if total > MAX_CONFIG_SOURCE_BYTES {
        return Err(format!(
            "spdeploy config sources exceed {MAX_CONFIG_SOURCE_BYTES} bytes"
        ));
    }
    let mut paths = HashSet::new();
    for stamp in sources {
        if !paths.insert(&stamp.path) {
            return Err("spdeploy config source stamps contain duplicate paths".to_string());
        }
        verify_config_source(&stamp.path, sources)?;
    }
    Ok(())
}

fn verify_config_source(config_path: &Path, sources: &[ConfigSourceStamp]) -> Result<(), String> {
    let stamp = sources
        .iter()
        .find(|stamp| stamp.path == config_path)
        .ok_or_else(|| "spdeploy config source stamp is missing".to_string())?;
    let current = read_file_bounded(config_path, stamp.source.len())?;
    if current != stamp.source {
        return Err(format!(
            "{} changed since spdeploy discovery; refresh and try again",
            config_path.display()
        ));
    }
    Ok(())
}

fn read_file_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    read_bounded_regular_file(path, max_bytes)
        .map_err(|error| format!("Could not securely read {}: {error}", path.display()))
}

fn required_bounded_string(
    value: Option<&Value>,
    field: &str,
    max: usize,
) -> Result<String, String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("spdeploy {field} must be a string"))?;
    validate_tool_text(value, field, max)?;
    Ok(value.to_string())
}

fn optional_bounded_string(
    value: Option<&Value>,
    field: &str,
    max: usize,
) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => required_bounded_string(Some(value), field, max).map(Some),
    }
}

fn validate_tool_text(value: &str, field: &str, max: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("spdeploy {field} cannot be empty"));
    }
    if value.len() > max {
        return Err(format!("spdeploy {field} exceeds {max} bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("spdeploy {field} contains control characters"));
    }
    Ok(())
}

fn spdeploy_error(message: String) -> ContextSection {
    ContextSection {
        id: SPDEPLOY_PROVIDER_ID.to_string(),
        title: "spdeploy".to_string(),
        content: ContextSectionContent::Error {
            message: bounded_text(&message, MAX_ERROR_BYTES),
        },
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.trim().to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", value[..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "phantom-context-actions-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_deploy_file_produces_no_section() {
        let temp = TestDir::new();
        assert_eq!(discover_spdeploy(temp.path()), None);
    }

    #[test]
    fn recent_directories_provider_uses_combined_history_selection() {
        let temp = TestDir::new();
        let mut config = ContextActionsConfig::default();
        config
            .record_directory_visit(Path::new("/projects/frequent"), 1)
            .unwrap();
        config
            .record_directory_visit(Path::new("/projects/frequent"), 2)
            .unwrap();
        config
            .record_directory_visit(Path::new("/projects/recent"), 20)
            .unwrap();

        let snapshot = discover_context(temp.path(), &config, &[]);
        let section = snapshot
            .sections
            .iter()
            .find(|section| section.id == RECENT_DIRECTORIES_PLUGIN_ID)
            .unwrap();
        let ContextSectionContent::RecentDirectories(recent) = &section.content else {
            panic!("expected recent directories section");
        };

        assert_eq!(recent.directories.len(), 2);
        assert_eq!(recent.directories[0].path, Path::new("/projects/frequent"));
        assert_eq!(recent.directories[1].path, Path::new("/projects/recent"));
    }

    #[test]
    fn manifest_discovery_reports_and_recognizes_exact_trust() {
        let temp = TestDir::new();
        fs::create_dir(temp.path().join("api")).unwrap();
        let source = r#"version: 1
name: Test project
tabs:
  - id: api
    title: API
    cwd: api
    run:
      program: cargo
      args: [run]
      env:
        RUST_LOG: info
"#;
        fs::write(temp.path().join(CONTEXT_MANIFEST_FILE), source).unwrap();

        let section = discover_manifest(temp.path(), &[]).unwrap();
        let ContextSectionContent::Manifest(manifest) = section.content else {
            panic!("expected manifest section");
        };
        assert_eq!(manifest.trust, ManifestTrustState::NeedsTrust);
        assert_eq!(manifest.manifest_source, source);
        assert_eq!(
            manifest.tabs[0].cwd,
            temp.path().join("api").canonicalize().unwrap()
        );
        assert_eq!(
            manifest.tabs[0].task.as_ref().unwrap().env,
            vec![("RUST_LOG".to_string(), "info".to_string())]
        );

        let trusted = trust_context_manifest(temp.path(), source.to_string()).unwrap();
        let section = discover_manifest(temp.path(), &[trusted]).unwrap();
        let ContextSectionContent::Manifest(manifest) = section.content else {
            panic!("expected manifest section");
        };
        assert_eq!(manifest.trust, ManifestTrustState::Trusted);
    }

    #[test]
    fn invalid_manifest_keeps_its_source_for_editing() {
        let temp = TestDir::new();
        let source = "version: [unterminated";
        fs::write(temp.path().join(CONTEXT_MANIFEST_FILE), source).unwrap();

        let section = discover_manifest(temp.path(), &[]).unwrap();
        let ContextSectionContent::ManifestError(error) = section.content else {
            panic!("expected editable manifest error");
        };
        assert_eq!(error.root, temp.path().canonicalize().unwrap());
        assert_eq!(error.manifest_source, source);
        assert!(error.message.contains("invalid .phantom.yml"));
    }

    #[test]
    fn always_active_directories_precede_context_dependent_sections() {
        let temp = TestDir::new();
        let source =
            "version: 1\nname: Test project\ntabs:\n  - id: shell\n    title: Shell\n    cwd: .\n";
        fs::write(temp.path().join(CONTEXT_MANIFEST_FILE), source).unwrap();
        let mut config = ContextActionsConfig::default();
        config
            .record_directory_visit(Path::new("/projects/alpha"), 1)
            .unwrap();

        let snapshot = discover_context(temp.path(), &config, &[]);
        let ids: Vec<_> = snapshot
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect();

        assert_eq!(ids, [RECENT_DIRECTORIES_PLUGIN_ID, MANIFEST_PROVIDER_ID]);
    }

    #[test]
    fn changed_manifest_source_invalidates_trust() {
        let temp = TestDir::new();
        fs::create_dir(temp.path().join("one")).unwrap();
        fs::create_dir(temp.path().join("two")).unwrap();
        let original =
            "version: 1\nname: Project\ntabs:\n  - id: shell\n    title: Shell\n    cwd: one\n";
        let changed =
            "version: 1\nname: Project\ntabs:\n  - id: shell\n    title: Shell\n    cwd: two\n";
        fs::write(temp.path().join(CONTEXT_MANIFEST_FILE), original).unwrap();
        let trusted = trust_context_manifest(temp.path(), original.to_string()).unwrap();
        fs::write(temp.path().join(CONTEXT_MANIFEST_FILE), changed).unwrap();

        let section = discover_manifest(temp.path(), &[trusted]).unwrap();
        let ContextSectionContent::Manifest(manifest) = section.content else {
            panic!("expected manifest section");
        };
        assert_eq!(manifest.trust, ManifestTrustState::NeedsTrust);
        assert_eq!(manifest.manifest_source, changed);
    }

    #[test]
    fn disabled_context_actions_skip_all_providers() {
        let temp = TestDir::new();
        fs::write(temp.path().join(DEPLOY_FILE), "fixture").unwrap();
        let config = ContextActionsConfig {
            enabled: false,
            ..ContextActionsConfig::default()
        };

        let snapshot = discover_context(temp.path(), &config, &[]);
        assert!(snapshot.sections.is_empty());
    }

    #[test]
    fn disabled_spdeploy_plugin_skips_yaml_parsing() {
        let temp = TestDir::new();
        fs::write(temp.path().join(DEPLOY_FILE), "fixture").unwrap();
        let mut config = ContextActionsConfig::default();
        config.plugin_mut(SPDEPLOY_PROVIDER_ID).unwrap().enabled = false;

        let snapshot = discover_context(temp.path(), &config, &[]);
        assert!(snapshot.sections.is_empty());
    }

    #[test]
    fn discovers_only_the_minimum_leaf_operation_fields_from_yaml() {
        let temp = TestDir::new();
        let source = r#"name: Soulfire
vars:
  ignored: value
operation:
  deploy:
    description: Ship it
    default: true
    ignored: anything
    stage:
      - type: script
        path: ship.sh
        args: [ignored]
"#;
        fs::write(temp.path().join(DEPLOY_FILE), source).unwrap();

        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Spdeploy(section) = section.content else {
            panic!("expected spdeploy section");
        };
        assert_eq!(section.project_name, "Soulfire");
        assert_eq!(section.operations.len(), 1);
        assert_eq!(section.config_sources.len(), 1);
        assert_eq!(section.config_sources[0].source, source.as_bytes());
        assert_eq!(section.operations[0].name, "deploy");
        assert!(section.operations[0].breadcrumbs.is_empty());
        assert_eq!(
            section.operations[0].description.as_deref(),
            Some("Ship it")
        );
    }

    #[test]
    fn recursively_flattens_submenu_operations() {
        let temp = TestDir::new();
        fs::write(
            temp.path().join(DEPLOY_FILE),
            "name: Root\noperation:\n  service:\n    stage:\n      - type: deploy\n        path: child.yml\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("child.yml"),
            "name: Child\noperation:\n  release:\n    description: Release service\n    stage:\n      - type: script\n        path: release.sh\n",
        )
        .unwrap();

        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Spdeploy(section) = section.content else {
            panic!("expected spdeploy section");
        };
        assert_eq!(section.operations.len(), 1);
        assert_eq!(section.operations[0].breadcrumbs, ["service"]);
        assert!(section.operations[0].config_path.ends_with("child.yml"));
        assert_eq!(section.config_sources.len(), 2);
    }

    #[test]
    fn malformed_yaml_is_reported_inside_the_provider_section() {
        let temp = TestDir::new();
        fs::write(temp.path().join(DEPLOY_FILE), "name: [unterminated").unwrap();

        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Error { message } = section.content else {
            panic!("expected error section");
        };
        assert!(message.contains("Could not parse"));
    }

    #[cfg(unix)]
    #[test]
    fn spdeploy_discovery_rejects_symlinked_config_sources() {
        use std::os::unix::fs::symlink;

        let temp = TestDir::new();
        let outside = TestDir::new();
        let source = outside.path().join("deploy.yml");
        fs::write(
            &source,
            "name: Project\noperation:\n  deploy:\n    stage:\n      - type: script\n        path: ship.sh\n",
        )
        .unwrap();
        symlink(&source, temp.path().join(DEPLOY_FILE)).unwrap();

        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Error { message } = section.content else {
            panic!("expected error section");
        };
        assert!(message.contains("securely read"), "{message}");
    }

    #[test]
    fn spdeploy_discovery_rejects_sources_beyond_the_total_limit() {
        let temp = TestDir::new();
        fs::write(
            temp.path().join(DEPLOY_FILE),
            vec![b'x'; MAX_CONFIG_SOURCE_BYTES + 1],
        )
        .unwrap();

        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Error { message } = section.content else {
            panic!("expected error section");
        };
        assert!(message.contains("byte limit"), "{message}");
    }

    #[test]
    fn dispatch_verification_rejects_config_changed_after_discovery() {
        let temp = TestDir::new();
        let config_path = temp.path().join(DEPLOY_FILE);
        fs::write(
            &config_path,
            "name: Project\noperation:\n  deploy:\n    stage:\n      - type: script\n        path: ship.sh\n",
        )
        .unwrap();
        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Spdeploy(section) = section.content else {
            panic!("expected spdeploy section");
        };
        verify_spdeploy_sources(&section).unwrap();

        fs::write(config_path, "name: changed\noperation: {}\n").unwrap();
        let error = verify_spdeploy_sources(&section).unwrap_err();
        assert!(error.contains("changed since spdeploy discovery"));
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_verification_rejects_a_source_replaced_by_a_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TestDir::new();
        let config_path = temp.path().join(DEPLOY_FILE);
        let source = "name: Project\noperation:\n  deploy:\n    stage:\n      - type: script\n        path: ship.sh\n";
        fs::write(&config_path, source).unwrap();
        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Spdeploy(section) = section.content else {
            panic!("expected spdeploy section");
        };
        fs::remove_file(&config_path).unwrap();
        let replacement = temp.path().join("replacement.yml");
        fs::write(&replacement, source).unwrap();
        symlink(&replacement, &config_path).unwrap();

        let error = verify_spdeploy_sources(&section).unwrap_err();

        assert!(error.contains("securely read"), "{error}");
    }

    #[test]
    fn explicit_deploy_operation_is_a_leaf_not_a_submenu() {
        let temp = TestDir::new();
        fs::write(
            temp.path().join(DEPLOY_FILE),
            "name: Root\noperation:\n  deploy-child:\n    stage:\n      - type: deploy\n        path: child.yml\n        operation: release\n",
        )
        .unwrap();

        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Spdeploy(section) = section.content else {
            panic!("expected spdeploy section");
        };
        assert_eq!(section.operations.len(), 1);
        assert_eq!(section.operations[0].name, "deploy-child");
    }

    #[test]
    fn rejects_submenus_that_escape_the_active_directory() {
        let temp = TestDir::new();
        let outside = TestDir::new();
        fs::write(outside.path().join("child.yml"), "child").unwrap();
        let relative = format!(
            "../{}/child.yml",
            outside.path().file_name().unwrap().to_string_lossy()
        );
        fs::write(
            temp.path().join(DEPLOY_FILE),
            format!(
                "name: Root\noperation:\n  outside:\n    stage:\n      - type: deploy\n        path: {relative}\n"
            ),
        )
        .unwrap();

        let section = discover_spdeploy(temp.path()).unwrap();
        let ContextSectionContent::Error { message } = section.content else {
            panic!("expected error section");
        };
        assert!(message.contains("outside the active directory"));
    }
}
