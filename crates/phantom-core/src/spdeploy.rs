use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

use noyalib::{Mapping, Value};
use serde::{Deserialize, Serialize};

use crate::{read_bounded_regular_file, AppError, AppResult};

pub const SPDEPLOY_CONFIG_FILE: &str = "deploy.yml";
pub const MAX_SPDEPLOY_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_SPDEPLOY_CONFIGS: usize = 32;
const MAX_DEPTH: usize = 8;
const MAX_OPERATIONS: usize = 256;
const MAX_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 1024;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedSpdeploySource {
    pub relative_path: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedSpdeployProject {
    pub root: String,
    pub sources: Vec<TrustedSpdeploySource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpdeployOperation {
    pub name: String,
    pub breadcrumbs: Vec<String>,
    pub description: Option<String>,
    pub config_relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpdeployGraph {
    pub root: PathBuf,
    pub project_name: String,
    pub operations: Vec<SpdeployOperation>,
    pub sources: Vec<TrustedSpdeploySource>,
}

impl TrustedSpdeployProject {
    pub fn validate(&self) -> AppResult<()> {
        validate_root(&self.root)?;
        let parsed = parse_source_graph(&self.sources)?;
        if parsed.sources != self.sources {
            return invalid("trusted spdeploy sources do not match their transitive config graph");
        }
        Ok(())
    }

    pub fn matches_graph(&self, graph: &SpdeployGraph) -> bool {
        Path::new(&self.root) == graph.root && self.sources == graph.sources
    }
}

pub fn load_spdeploy_graph(root: &Path) -> AppResult<Option<SpdeployGraph>> {
    let candidate = root.join(SPDEPLOY_CONFIG_FILE);
    match candidate.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let root = root.canonicalize()?;
    if !root.is_dir() || root.to_str().is_none() {
        return invalid("spdeploy project root must be a UTF-8 directory");
    }
    let mut sources = BTreeMap::new();
    collect_fs_sources(&root, SPDEPLOY_CONFIG_FILE, 0, &mut sources)?;
    let sources = sources
        .into_iter()
        .map(|(relative_path, source)| TrustedSpdeploySource {
            relative_path,
            source,
        })
        .collect::<Vec<_>>();
    let parsed = parse_source_graph(&sources)?;
    Ok(Some(SpdeployGraph {
        root,
        project_name: parsed.project_name,
        operations: parsed.operations,
        sources: parsed.sources,
    }))
}

pub fn trust_spdeploy_graph(graph: &SpdeployGraph) -> AppResult<TrustedSpdeployProject> {
    let trusted = TrustedSpdeployProject {
        root: graph
            .root
            .to_str()
            .ok_or_else(|| AppError::InvalidConfig("spdeploy root must be UTF-8".into()))?
            .to_string(),
        sources: graph.sources.clone(),
    };
    trusted.validate()?;
    Ok(trusted)
}

pub fn verify_spdeploy_graph(graph: &SpdeployGraph) -> AppResult<()> {
    let current = load_spdeploy_graph(&graph.root)?.ok_or_else(|| {
        AppError::InvalidConfig("deploy.yml no longer exists; review and trust it again".into())
    })?;
    if current.sources != graph.sources || current.operations != graph.operations {
        return invalid("spdeploy configuration changed; review and trust it again");
    }
    Ok(())
}

struct ParsedGraph {
    project_name: String,
    operations: Vec<SpdeployOperation>,
    sources: Vec<TrustedSpdeploySource>,
}

// `type: deploy` paths on non-submenu operations are operation-call edges,
// not includes. Same-file sibling calls (`path: deploy.yml` + `operation:`)
// and mutual calls across files are valid; revisiting an already-collected
// source is a no-op. Submenu-only operations are ignored: they are not listed
// and their targets are not part of this directory's trust graph.
fn collect_fs_sources(
    root: &Path,
    relative_path: &str,
    depth: usize,
    sources: &mut BTreeMap<String, String>,
) -> AppResult<()> {
    if depth > MAX_DEPTH {
        return invalid(format!("spdeploy config depth exceeds {MAX_DEPTH}"));
    }
    if sources.contains_key(relative_path) {
        return Ok(());
    }
    if sources.len() >= MAX_SPDEPLOY_CONFIGS {
        return invalid(format!(
            "spdeploy config count exceeds {MAX_SPDEPLOY_CONFIGS}"
        ));
    }
    let used = sources.values().try_fold(0usize, |total, source| {
        total
            .checked_add(source.len())
            .ok_or_else(|| AppError::InvalidConfig("spdeploy sources are too large".into()))
    })?;
    let remaining = MAX_SPDEPLOY_SOURCE_BYTES
        .checked_sub(used)
        .ok_or_else(|| AppError::InvalidConfig("spdeploy sources are too large".into()))?;
    let path = root.join(relative_path);
    let bytes = read_bounded_regular_file(&path, remaining).map_err(|error| {
        AppError::InvalidConfig(format!("could not read {}: {error}", path.display()))
    })?;
    let source = String::from_utf8(bytes)
        .map_err(|_| AppError::InvalidConfig(format!("{} must be valid UTF-8", path.display())))?;
    let value = parse_yaml(relative_path, &source)?;
    let children = deploy_children(&value, relative_path)?;
    sources.insert(relative_path.to_string(), source);
    for child in children {
        collect_fs_sources(root, &child, depth + 1, sources)?;
    }
    Ok(())
}

fn parse_source_graph(sources: &[TrustedSpdeploySource]) -> AppResult<ParsedGraph> {
    validate_sources(sources)?;
    let source_map = sources
        .iter()
        .map(|source| (source.relative_path.as_str(), source.source.as_str()))
        .collect::<BTreeMap<_, _>>();
    let root_value = parse_yaml(SPDEPLOY_CONFIG_FILE, source_map[SPDEPLOY_CONFIG_FILE])?;
    let project_name = config_name(&root_value)?;
    let mut required = HashSet::new();
    collect_required(SPDEPLOY_CONFIG_FILE, &source_map, 0, &mut required)?;
    if required.len() != sources.len()
        || sources
            .iter()
            .any(|source| !required.contains(source.relative_path.as_str()))
    {
        return invalid("trusted spdeploy sources contain missing or unrelated configs");
    }
    let mut operations = Vec::new();
    collect_root_operations(&source_map, &mut operations)?;
    Ok(ParsedGraph {
        project_name,
        operations,
        sources: sources.to_vec(),
    })
}

fn collect_required(
    relative_path: &str,
    sources: &BTreeMap<&str, &str>,
    depth: usize,
    required: &mut HashSet<String>,
) -> AppResult<()> {
    if depth > MAX_DEPTH {
        return invalid(format!("spdeploy config depth exceeds {MAX_DEPTH}"));
    }
    if required.contains(relative_path) {
        return Ok(());
    }
    let source = sources.get(relative_path).ok_or_else(|| {
        AppError::InvalidConfig(format!(
            "trusted spdeploy source '{relative_path}' is missing"
        ))
    })?;
    let value = parse_yaml(relative_path, source)?;
    required.insert(relative_path.to_string());
    for child in deploy_children(&value, relative_path)? {
        collect_required(&child, sources, depth + 1, required)?;
    }
    Ok(())
}

fn collect_root_operations(
    sources: &BTreeMap<&str, &str>,
    operations: &mut Vec<SpdeployOperation>,
) -> AppResult<()> {
    let source = sources.get(SPDEPLOY_CONFIG_FILE).ok_or_else(|| {
        AppError::InvalidConfig(format!(
            "trusted spdeploy source '{SPDEPLOY_CONFIG_FILE}' is missing"
        ))
    })?;
    let value = parse_yaml(SPDEPLOY_CONFIG_FILE, source)?;
    for (name, operation) in operation_mapping(&value)? {
        validate_text(name, "operation name", MAX_NAME_BYTES)?;
        let operation = operation.as_mapping().ok_or_else(|| {
            AppError::InvalidConfig("spdeploy operation must be an object".into())
        })?;
        let stages = operation
            .get("stage")
            .and_then(Value::as_sequence)
            .ok_or_else(|| {
                AppError::InvalidConfig(format!("spdeploy operation '{name}' has no stages array"))
            })?;
        if is_submenu(stages)? {
            continue;
        }
        if operations.len() >= MAX_OPERATIONS {
            return invalid(format!("spdeploy operation count exceeds {MAX_OPERATIONS}"));
        }
        operations.push(SpdeployOperation {
            name: name.clone(),
            breadcrumbs: Vec::new(),
            description: optional_string(
                operation.get("description"),
                "operation description",
                MAX_DESCRIPTION_BYTES,
            )?,
            config_relative_path: SPDEPLOY_CONFIG_FILE.to_string(),
        });
    }
    Ok(())
}

fn deploy_children(value: &Value, declaring_path: &str) -> AppResult<Vec<String>> {
    let mut children = Vec::new();
    for (_, operation) in operation_mapping(value)? {
        let operation = operation.as_mapping().ok_or_else(|| {
            AppError::InvalidConfig("spdeploy operation must be an object".into())
        })?;
        let stages = operation
            .get("stage")
            .and_then(Value::as_sequence)
            .ok_or_else(|| {
                AppError::InvalidConfig("spdeploy operation has no stages array".into())
            })?;
        if is_submenu(stages)? {
            continue;
        }
        collect_stage_children(stages, declaring_path, &mut children)?;
    }
    children.sort();
    children.dedup();
    Ok(children)
}

fn collect_stage_children(
    stages: &[Value],
    declaring_path: &str,
    children: &mut Vec<String>,
) -> AppResult<()> {
    for stage in stages {
        let stage = stage
            .as_mapping()
            .ok_or_else(|| AppError::InvalidConfig("spdeploy stage must be an object".into()))?;
        match stage.get("type").and_then(Value::as_str) {
            Some("deploy") => {
                let path =
                    required_string(stage.get("path"), "deploy config path", MAX_PATH_BYTES)?;
                reject_dynamic_path(&path)?;
                children.push(resolve_relative_config(declaring_path, &path)?);
            }
            Some("parallel") => {
                let nested = stage
                    .get("stages")
                    .and_then(Value::as_sequence)
                    .ok_or_else(|| {
                        AppError::InvalidConfig(
                            "parallel spdeploy stage has no stages array".into(),
                        )
                    })?;
                collect_stage_children(nested, declaring_path, children)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_submenu(stages: &[Value]) -> AppResult<bool> {
    if stages.len() != 1 {
        return Ok(false);
    }
    let Some(stage) = stages[0].as_mapping() else {
        return invalid("spdeploy stage must be an object");
    };
    Ok(stage.get("type").and_then(Value::as_str) == Some("deploy")
        && !stage.get("operation").is_some_and(|value| !value.is_null()))
}

fn reject_dynamic_path(path: &str) -> AppResult<()> {
    if path.contains("{{") || path.contains("}}") {
        return invalid("dynamic spdeploy config paths cannot be trusted");
    }
    Ok(())
}

fn resolve_relative_config(declaring_path: &str, child: &str) -> AppResult<String> {
    if child.contains('\0') || Path::new(child).is_absolute() {
        return invalid("spdeploy config path must be relative and contain no NUL byte");
    }
    let parent = Path::new(declaring_path).parent().unwrap_or(Path::new(""));
    normalize_relative_path(&parent.join(child))
}

fn normalize_relative_path(path: &Path) -> AppResult<String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return invalid("spdeploy config path resolves outside the project root");
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return invalid("spdeploy config path must be relative")
            }
        }
    }
    let text = normalized.to_str().ok_or_else(|| {
        AppError::InvalidConfig("spdeploy config path must be valid UTF-8".into())
    })?;
    validate_text(text, "config path", MAX_PATH_BYTES)?;
    Ok(text.to_string())
}

fn validate_sources(sources: &[TrustedSpdeploySource]) -> AppResult<()> {
    if sources.is_empty() || sources.len() > MAX_SPDEPLOY_CONFIGS {
        return invalid(format!(
            "trusted spdeploy sources must contain 1 to {MAX_SPDEPLOY_CONFIGS} configs"
        ));
    }
    let total = sources.iter().try_fold(0usize, |total, source| {
        total
            .checked_add(source.source.len())
            .ok_or_else(|| AppError::InvalidConfig("spdeploy sources are too large".into()))
    })?;
    if total > MAX_SPDEPLOY_SOURCE_BYTES {
        return invalid(format!(
            "trusted spdeploy sources exceed {MAX_SPDEPLOY_SOURCE_BYTES} bytes"
        ));
    }
    let mut previous = None;
    for source in sources {
        let normalized = normalize_relative_path(Path::new(&source.relative_path))?;
        if normalized != source.relative_path {
            return invalid("trusted spdeploy source paths must be normalized");
        }
        if previous.is_some_and(|path| path >= source.relative_path.as_str()) {
            return invalid("trusted spdeploy source paths must be strictly sorted and unique");
        }
        previous = Some(source.relative_path.as_str());
    }
    if !sources
        .iter()
        .any(|source| source.relative_path == SPDEPLOY_CONFIG_FILE)
    {
        return invalid("trusted spdeploy sources must include deploy.yml");
    }
    Ok(())
}

fn validate_root(root: &str) -> AppResult<()> {
    validate_text(root, "project root", MAX_PATH_BYTES)?;
    let path = Path::new(root);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return invalid("trusted spdeploy project root must be a normalized absolute path");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
            Component::CurDir | Component::ParentDir => {
                return invalid("trusted spdeploy project root must be normalized")
            }
        }
    }
    if normalized.as_os_str() != path.as_os_str() {
        return invalid("trusted spdeploy project root must be a normalized absolute path");
    }
    Ok(())
}

fn parse_yaml(path: &str, source: &str) -> AppResult<Value> {
    noyalib::from_slice(source.as_bytes())
        .map_err(|error| AppError::InvalidConfig(format!("could not parse {path}: {error}")))
}

fn config_name(value: &Value) -> AppResult<String> {
    let object = value
        .as_mapping()
        .ok_or_else(|| AppError::InvalidConfig("spdeploy config must be a YAML mapping".into()))?;
    required_string(object.get("name"), "project name", MAX_NAME_BYTES)
}

fn operation_mapping(value: &Value) -> AppResult<&Mapping> {
    let object = value
        .as_mapping()
        .ok_or_else(|| AppError::InvalidConfig("spdeploy config must be a YAML mapping".into()))?;
    object
        .get("operation")
        .and_then(Value::as_mapping)
        .ok_or_else(|| AppError::InvalidConfig("spdeploy config has no operation mapping".into()))
}

fn required_string(value: Option<&Value>, field: &str, max: usize) -> AppResult<String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::InvalidConfig(format!("spdeploy {field} must be a string")))?;
    validate_text(value, field, max)?;
    Ok(value.to_string())
}

fn optional_string(value: Option<&Value>, field: &str, max: usize) -> AppResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(value) if value.is_null() => Ok(None),
        Some(value) => required_string(Some(value), field, max).map(Some),
    }
}

fn validate_text(value: &str, field: &str, max: usize) -> AppResult<()> {
    if value.is_empty() {
        return invalid(format!("spdeploy {field} cannot be empty"));
    }
    if value.len() > max {
        return invalid(format!("spdeploy {field} exceeds {max} bytes"));
    }
    if value.chars().any(char::is_control) {
        return invalid(format!("spdeploy {field} contains control characters"));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> AppResult<T> {
    Err(AppError::InvalidConfig(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "phantom-spdeploy-trust-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn graph_includes_explicit_and_parallel_deploy_configs() {
        let temp = TempDir::new();
        fs::create_dir_all(temp.0.join("services")).unwrap();
        fs::write(temp.0.join("deploy.yml"), "name: root\noperation:\n  deploy:\n    stage:\n      - type: parallel\n        stages:\n          - type: deploy\n            path: services/api.yml\n            operation: deploy\n      - type: deploy\n        path: services/ui.yml\n        operation: deploy\n").unwrap();
        for file in ["api.yml", "ui.yml"] {
            fs::write(
                temp.0.join("services").join(file),
                "name: child\noperation:\n  deploy:\n    stage: []\n",
            )
            .unwrap();
        }
        let graph = load_spdeploy_graph(&temp.0).unwrap().unwrap();
        assert_eq!(
            graph
                .sources
                .iter()
                .map(|source| source.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["deploy.yml", "services/api.yml", "services/ui.yml"]
        );
        assert_eq!(graph.operations.len(), 1);
    }

    #[test]
    fn changed_nested_source_invalidates_graph() {
        let temp = TempDir::new();
        fs::write(temp.0.join("deploy.yml"), "name: root\noperation:\n  child:\n    stage:\n      - type: deploy\n        path: child.yml\n        operation: deploy\n").unwrap();
        fs::write(
            temp.0.join("child.yml"),
            "name: child\noperation:\n  deploy:\n    stage: []\n",
        )
        .unwrap();
        let graph = load_spdeploy_graph(&temp.0).unwrap().unwrap();
        fs::write(
            temp.0.join("child.yml"),
            "name: child\noperation:\n  changed:\n    stage: []\n",
        )
        .unwrap();
        assert!(verify_spdeploy_graph(&graph).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_nested_source_is_rejected() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new();
        fs::write(temp.0.join("deploy.yml"), "name: root\noperation:\n  child:\n    stage:\n      - type: deploy\n        path: child.yml\n        operation: deploy\n").unwrap();
        fs::write(
            temp.0.join("actual.yml"),
            "name: child\noperation:\n  deploy:\n    stage: []\n",
        )
        .unwrap();
        symlink(temp.0.join("actual.yml"), temp.0.join("child.yml")).unwrap();
        assert!(load_spdeploy_graph(&temp.0).is_err());
    }

    #[test]
    fn same_file_operation_calls_are_not_config_cycles() {
        let temp = TempDir::new();
        fs::write(
            temp.0.join("deploy.yml"),
            "name: project\noperation:\n  deploy:\n    stage:\n      - type: deploy\n        path: deploy.yml\n        operation: env\n      - type: deploy\n        path: deploy.yml\n        operation: compose\n  env:\n    stage:\n      - type: script\n        path: env.sh\n  compose:\n    stage:\n      - type: script\n        path: compose.sh\n",
        )
        .unwrap();
        let graph = load_spdeploy_graph(&temp.0).unwrap().unwrap();
        assert_eq!(
            graph
                .sources
                .iter()
                .map(|source| source.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["deploy.yml"]
        );
        assert_eq!(
            graph
                .operations
                .iter()
                .map(|operation| operation.name.as_str())
                .collect::<Vec<_>>(),
            ["deploy", "env", "compose"]
        );
        trust_spdeploy_graph(&graph).unwrap();
    }

    #[test]
    fn mutual_operation_calls_across_files_are_not_config_cycles() {
        let temp = TempDir::new();
        fs::write(
            temp.0.join("deploy.yml"),
            "name: root\noperation:\n  call_child:\n    stage:\n      - type: deploy\n        path: child.yml\n        operation: work\n",
        )
        .unwrap();
        fs::write(
            temp.0.join("child.yml"),
            "name: child\noperation:\n  work:\n    stage:\n      - type: script\n        path: work.sh\n  call_parent:\n    stage:\n      - type: deploy\n        path: deploy.yml\n        operation: call_child\n",
        )
        .unwrap();
        let graph = load_spdeploy_graph(&temp.0).unwrap().unwrap();
        assert_eq!(
            graph
                .sources
                .iter()
                .map(|source| source.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["child.yml", "deploy.yml"]
        );
        trust_spdeploy_graph(&graph).unwrap();
    }

    #[test]
    fn submenu_only_configs_are_not_listed_or_collected() {
        let temp = TempDir::new();
        fs::write(
            temp.0.join("deploy.yml"),
            "name: root\noperation:\n  down:\n    stage:\n      - type: deploy\n        path: child.yml\n",
        )
        .unwrap();
        fs::write(
            temp.0.join("child.yml"),
            "name: child\noperation:\n  up:\n    stage:\n      - type: deploy\n        path: deploy.yml\n",
        )
        .unwrap();
        let graph = load_spdeploy_graph(&temp.0).unwrap().unwrap();
        assert!(graph.operations.is_empty());
        assert_eq!(
            graph
                .sources
                .iter()
                .map(|source| source.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["deploy.yml"]
        );
    }
}
