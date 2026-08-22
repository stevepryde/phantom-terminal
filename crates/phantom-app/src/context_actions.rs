//! Directory-aware action discovery and the typed model consumed by egui.
//!
//! Providers in this module are compiled into Phantom. Discovery is read-only:
//! it inspects bounded local files, but never launches a project task, deploy
//! operation, or discovery subprocess.

use std::path::{Path, PathBuf};

use phantom_core::{
    load_context_manifest_source, load_spdeploy_graph, parse_context_manifest,
    trust_context_manifest, verify_spdeploy_graph, ContextActionsConfig, ContextRun, SpdeployGraph,
    TrustedProject, TrustedSpdeployProject, TrustedSpdeploySource, BUILT_IN_CONTEXT_PLUGIN_IDS,
    CONTEXT_MANIFEST_FILE, RECENT_DIRECTORIES_PLUGIN_ID,
};

pub const MANIFEST_PROVIDER_ID: &str = "phantom-manifest";
pub const SPDEPLOY_PROVIDER_ID: &str = "spdeploy";

#[cfg(test)]
const DEPLOY_FILE: &str = phantom_core::SPDEPLOY_CONFIG_FILE;
#[cfg(test)]
const MAX_CONFIG_SOURCE_BYTES: usize = phantom_core::MAX_SPDEPLOY_SOURCE_BYTES;
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
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub project_name: String,
    pub trust: SpdeployTrustState,
    pub operations: Vec<SpdeployOperation>,
    /// Sorted root-relative full transitive config graph and exact bounded
    /// source used to derive operations and compare persisted trust.
    pub config_sources: Vec<TrustedSpdeploySource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpdeployTrustState {
    NeedsTrust,
    Trusted,
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
    pub config_relative_path: String,
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
    TrustSpdeploy {
        root: PathBuf,
        sources: Vec<TrustedSpdeploySource>,
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
    trusted_spdeploy_projects: &[TrustedSpdeployProject],
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
            SPDEPLOY_PROVIDER_ID => {
                discover_spdeploy_with_trust(&canonical_cwd, trusted_spdeploy_projects)
            }
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

#[cfg(test)]
pub fn discover_spdeploy(cwd: &Path) -> Option<ContextSection> {
    discover_spdeploy_with_trust(cwd, &[])
}

pub fn discover_spdeploy_with_trust(
    cwd: &Path,
    trusted_projects: &[TrustedSpdeployProject],
) -> Option<ContextSection> {
    match load_spdeploy_graph(cwd) {
        Ok(None) => None,
        Ok(Some(graph)) => {
            let trusted = trusted_projects
                .iter()
                .any(|project| project.matches_graph(&graph));
            let config_path = graph.root.join(phantom_core::SPDEPLOY_CONFIG_FILE);
            let operations = graph
                .operations
                .iter()
                .map(|operation| SpdeployOperation {
                    name: operation.name.clone(),
                    breadcrumbs: operation.breadcrumbs.clone(),
                    description: operation.description.clone(),
                    config_path: graph.root.join(&operation.config_relative_path),
                    config_relative_path: operation.config_relative_path.clone(),
                })
                .collect();
            Some(ContextSection {
                id: SPDEPLOY_PROVIDER_ID.to_string(),
                title: "spdeploy".to_string(),
                content: ContextSectionContent::Spdeploy(SpdeploySection {
                    root: graph.root,
                    config_path,
                    project_name: graph.project_name,
                    trust: if trusted {
                        SpdeployTrustState::Trusted
                    } else {
                        SpdeployTrustState::NeedsTrust
                    },
                    operations,
                    config_sources: graph.sources,
                }),
            })
        }
        Err(error) => Some(spdeploy_error(error.to_string())),
    }
}

pub fn verify_spdeploy_sources(section: &SpdeploySection) -> Result<(), String> {
    verify_spdeploy_graph(&section_graph(section)).map_err(|error| error.to_string())
}

pub fn section_graph(section: &SpdeploySection) -> SpdeployGraph {
    SpdeployGraph {
        root: section.root.clone(),
        project_name: section.project_name.clone(),
        operations: section
            .operations
            .iter()
            .map(|operation| phantom_core::SpdeployOperation {
                name: operation.name.clone(),
                breadcrumbs: operation.breadcrumbs.clone(),
                description: operation.description.clone(),
                config_relative_path: operation.config_relative_path.clone(),
            })
            .collect(),
        sources: section.config_sources.clone(),
    }
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

        let snapshot = discover_context(temp.path(), &config, &[], &[]);
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

        let snapshot = discover_context(temp.path(), &config, &[], &[]);
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

        let snapshot = discover_context(temp.path(), &config, &[], &[]);
        assert!(snapshot.sections.is_empty());
    }

    #[test]
    fn disabled_spdeploy_plugin_skips_yaml_parsing() {
        let temp = TestDir::new();
        fs::write(temp.path().join(DEPLOY_FILE), "fixture").unwrap();
        let mut config = ContextActionsConfig::default();
        config.plugin_mut(SPDEPLOY_PROVIDER_ID).unwrap().enabled = false;

        let snapshot = discover_context(temp.path(), &config, &[], &[]);
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
        assert_eq!(section.config_sources[0].source, source);
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
        assert!(message.contains("could not parse"));
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
        assert!(message.contains("could not read"), "{message}");
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
        assert!(error.contains("configuration changed"));
    }

    #[test]
    fn exact_graph_trust_is_recognized_and_source_change_invalidates_it() {
        let temp = TestDir::new();
        let config_path = temp.path().join(DEPLOY_FILE);
        fs::write(
            &config_path,
            "name: Project\noperation:\n  deploy:\n    stage: []\n",
        )
        .unwrap();
        let graph = load_spdeploy_graph(temp.path()).unwrap().unwrap();
        let trusted = phantom_core::trust_spdeploy_graph(&graph).unwrap();
        let section =
            discover_spdeploy_with_trust(temp.path(), std::slice::from_ref(&trusted)).unwrap();
        let ContextSectionContent::Spdeploy(section) = section.content else {
            panic!("expected spdeploy section");
        };
        assert_eq!(section.trust, SpdeployTrustState::Trusted);

        fs::write(
            &config_path,
            "name: Project\noperation:\n  release:\n    stage: []\n",
        )
        .unwrap();
        let changed = discover_spdeploy_with_trust(temp.path(), &[trusted]).unwrap();
        let ContextSectionContent::Spdeploy(changed) = changed.content else {
            panic!("expected spdeploy section");
        };
        assert_eq!(changed.trust, SpdeployTrustState::NeedsTrust);
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

        assert!(error.contains("could not read"), "{error}");
    }

    #[test]
    fn explicit_deploy_operation_is_a_leaf_not_a_submenu() {
        let temp = TestDir::new();
        fs::write(
            temp.path().join(DEPLOY_FILE),
            "name: Root\noperation:\n  deploy-child:\n    stage:\n      - type: deploy\n        path: child.yml\n        operation: release\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("child.yml"),
            "name: Child\noperation:\n  release:\n    stage: []\n",
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
        assert!(message.contains("outside the project root"));
    }
}
