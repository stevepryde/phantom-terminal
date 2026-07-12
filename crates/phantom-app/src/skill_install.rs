//! Local installation of the application-bundled Phantom workflow skill.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SKILL_NAME: &str = "phantom-workflows";
const MARKER_FILE: &str = ".phantom-skill";
const MARKER_SCHEMA: &str = "2";
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

struct BundleFile {
    relative_path: &'static str,
    contents: &'static str,
}

const BUNDLE_FILES: &[BundleFile] = &[
    BundleFile {
        relative_path: "SKILL.md",
        contents: include_str!("../assets/skills/phantom-workflows/SKILL.md"),
    },
    BundleFile {
        relative_path: "agents/openai.yaml",
        contents: include_str!("../assets/skills/phantom-workflows/agents/openai.yaml"),
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillTarget {
    All,
    Codex,
    Claude,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallStatus {
    Installed,
    Updated,
    Current,
}

impl InstallStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Installed => "Installed",
            Self::Updated => "Updated",
            Self::Current => "Already current",
        }
    }
}

#[derive(Debug)]
pub struct InstallResult {
    pub target: &'static str,
    pub path: PathBuf,
    pub status: InstallStatus,
}

#[derive(Debug)]
struct PreparedInstall {
    target: &'static str,
    skill_dir: PathBuf,
    status: InstallStatus,
}

type BundleManifest = BTreeMap<String, u64>;

pub fn install_for_target(target: SkillTarget, force: bool) -> Result<Vec<InstallResult>, String> {
    let targets = resolve_target_roots(target)?;
    let prepared = targets
        .into_iter()
        .map(|(name, skills_root)| prepare_install(name, &skills_root, force))
        .collect::<Result<Vec<_>, _>>()?;

    prepared.into_iter().map(perform_install).collect()
}

fn resolve_target_roots(target: SkillTarget) -> Result<Vec<(&'static str, PathBuf)>, String> {
    let mut roots = Vec::with_capacity(2);
    if matches!(target, SkillTarget::All | SkillTarget::Codex) {
        roots.push(("Codex", config_root("CODEX_HOME", ".codex")?.join("skills")));
    }
    if matches!(target, SkillTarget::All | SkillTarget::Claude) {
        roots.push((
            "Claude",
            config_root("CLAUDE_CONFIG_DIR", ".claude")?.join("skills"),
        ));
    }
    Ok(roots)
}

fn config_root(variable: &str, default_name: &str) -> Result<PathBuf, String> {
    if let Some(value) = nonempty_env(variable) {
        return Ok(PathBuf::from(value));
    }
    let home = nonempty_env("HOME").ok_or_else(|| {
        format!("cannot resolve {default_name} because neither {variable} nor HOME is set")
    })?;
    Ok(PathBuf::from(home).join(default_name))
}

fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn prepare_install(
    target: &'static str,
    skills_root: &Path,
    force: bool,
) -> Result<PreparedInstall, String> {
    let skill_dir = skills_root.join(SKILL_NAME);
    let Some(metadata) = symlink_metadata_if_exists(&skill_dir)? else {
        return Ok(PreparedInstall {
            target,
            skill_dir,
            status: InstallStatus::Installed,
        });
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "refusing to replace non-directory or symlink {}",
            skill_dir.display()
        ));
    }

    preflight_owned_paths(&skill_dir)?;
    let embedded = embedded_manifest();
    let Some(managed) = read_marker(&skill_dir.join(MARKER_FILE))? else {
        if force {
            return Ok(PreparedInstall {
                target,
                skill_dir,
                status: InstallStatus::Updated,
            });
        }
        return Err(format!(
            "{} contains an unmanaged or locally modified {SKILL_NAME} skill; rerun with --force to replace Phantom-owned files",
            skill_dir.display()
        ));
    };
    let managed_unchanged = current_owned_files_unchanged(&skill_dir, &managed)?;
    if !managed_unchanged && !force {
        return Err(format!(
            "{} contains an unmanaged or locally modified {SKILL_NAME} skill; rerun with --force to replace Phantom-owned files",
            skill_dir.display()
        ));
    }
    let current = installed_files_match(&skill_dir, &embedded)?;
    if managed_unchanged && current && managed == embedded {
        return Ok(PreparedInstall {
            target,
            skill_dir,
            status: InstallStatus::Current,
        });
    }

    Ok(PreparedInstall {
        target,
        skill_dir,
        status: InstallStatus::Updated,
    })
}

fn preflight_owned_paths(skill_dir: &Path) -> Result<(), String> {
    for relative in BUNDLE_FILES.iter().map(|file| file.relative_path) {
        let path = owned_path(skill_dir, relative)?;
        validate_owned_ancestors(skill_dir, &path)?;
        if let Some(metadata) = symlink_metadata_if_exists(&path)? {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "refusing to replace non-file or symlink {}",
                    path.display()
                ));
            }
        }
    }
    let marker = skill_dir.join(MARKER_FILE);
    if let Some(metadata) = symlink_metadata_if_exists(&marker)? {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "refusing to replace non-file or symlink {}",
                marker.display()
            ));
        }
    }
    Ok(())
}

fn perform_install(prepared: PreparedInstall) -> Result<InstallResult, String> {
    if prepared.status != InstallStatus::Current {
        fs::create_dir_all(&prepared.skill_dir).map_err(|error| {
            format!(
                "could not create skill directory {}: {error}",
                prepared.skill_dir.display()
            )
        })?;
        preflight_owned_paths(&prepared.skill_dir)?;

        for file in BUNDLE_FILES {
            let path = owned_path(&prepared.skill_dir, file.relative_path)?;
            create_owned_parent_dirs(&prepared.skill_dir, &path)?;
            atomic_write(&path, file.contents.as_bytes())?;
        }
        let marker = marker_source(&embedded_manifest());
        atomic_write(&prepared.skill_dir.join(MARKER_FILE), marker.as_bytes())?;
    }

    Ok(InstallResult {
        target: prepared.target,
        path: prepared.skill_dir,
        status: prepared.status,
    })
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(metadata) = symlink_metadata_if_exists(path)? {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "refusing to replace non-file or symlink {}",
                path.display()
            ));
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has an invalid file name", path.display()))?;
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.phantom-{}-{sequence}.tmp",
        std::process::id()
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("could not create {}: {error}", temporary.display()))?;
        file.write_all(contents)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path).map_err(|error| {
            format!(
                "could not install {} from {}: {error}",
                path.display(),
                temporary.display()
            )
        })
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn read_marker(path: &Path) -> Result<Option<BundleManifest>, String> {
    let Some(metadata) = symlink_metadata_if_exists(path)? else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "refusing to read non-file or symlink {}",
            path.display()
        ));
    }
    let source = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut schema = None;
    let mut manifest = BundleManifest::new();
    for line in source.lines() {
        if let Some(value) = line.strip_prefix("schema=") {
            schema = Some(value);
        } else if let Some(value) = line.strip_prefix("file=") {
            let Some((hash, relative)) = value.split_once('\t') else {
                return Ok(None);
            };
            let Some(hash) = u64::from_str_radix(hash, 16).ok() else {
                return Ok(None);
            };
            if manifest.insert(relative.to_string(), hash).is_some() {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
    }
    if schema != Some(MARKER_SCHEMA) || manifest.is_empty() {
        return Ok(None);
    }
    Ok(Some(manifest))
}

fn embedded_manifest() -> BundleManifest {
    BUNDLE_FILES
        .iter()
        .map(|file| {
            (
                file.relative_path.to_string(),
                hash_bytes(file.contents.as_bytes()),
            )
        })
        .collect()
}

fn installed_files_match(skill_dir: &Path, manifest: &BundleManifest) -> Result<bool, String> {
    for (relative, expected_hash) in manifest {
        let path = owned_path(skill_dir, relative)?;
        validate_owned_ancestors(skill_dir, &path)?;
        let Some(metadata) = symlink_metadata_if_exists(&path)? else {
            return Ok(false);
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "refusing to read non-file or symlink {}",
                path.display()
            ));
        }
        let contents = fs::read(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if hash_bytes(&contents) != *expected_hash {
            return Ok(false);
        }
    }
    Ok(true)
}

fn current_owned_files_unchanged(
    skill_dir: &Path,
    managed: &BundleManifest,
) -> Result<bool, String> {
    for file in BUNDLE_FILES {
        let path = owned_path(skill_dir, file.relative_path)?;
        validate_owned_ancestors(skill_dir, &path)?;
        match managed.get(file.relative_path) {
            Some(expected_hash) => {
                let Some(metadata) = symlink_metadata_if_exists(&path)? else {
                    return Ok(false);
                };
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(format!(
                        "refusing to read non-file or symlink {}",
                        path.display()
                    ));
                }
                let contents = fs::read(&path)
                    .map_err(|error| format!("could not read {}: {error}", path.display()))?;
                if hash_bytes(&contents) != *expected_hash {
                    return Ok(false);
                }
            }
            None if symlink_metadata_if_exists(&path)?.is_some() => return Ok(false),
            None => {}
        }
    }
    Ok(true)
}

fn marker_source(manifest: &BundleManifest) -> String {
    let mut source = format!("schema={MARKER_SCHEMA}\n");
    for (relative, hash) in manifest {
        source.push_str(&format!("file={hash:016x}\t{relative}\n"));
    }
    source
}

fn owned_path(skill_dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative.is_empty()
        || relative == MARKER_FILE
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("invalid owned skill path '{relative}'"));
    }
    Ok(skill_dir.join(relative_path))
}

fn validate_owned_ancestors(skill_dir: &Path, path: &Path) -> Result<(), String> {
    let relative = path
        .strip_prefix(skill_dir)
        .map_err(|_| format!("{} escapes the skill directory", path.display()))?;
    let mut current = skill_dir.to_path_buf();
    for component in relative
        .components()
        .take(relative.components().count().saturating_sub(1))
    {
        current.push(component.as_os_str());
        let Some(metadata) = symlink_metadata_if_exists(&current)? else {
            return Ok(());
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "refusing to use non-directory or symlink {}",
                current.display()
            ));
        }
    }
    Ok(())
}

fn create_owned_parent_dirs(skill_dir: &Path, path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let relative = parent
        .strip_prefix(skill_dir)
        .map_err(|_| format!("{} escapes the skill directory", path.display()))?;
    let mut current = skill_dir.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match symlink_metadata_if_exists(&current)? {
            Some(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!(
                    "refusing to use non-directory or symlink {}",
                    current.display()
                ));
            }
            Some(_) => {}
            None => fs::create_dir(&current)
                .map_err(|error| format!("could not create {}: {error}", current.display()))?,
        }
    }
    Ok(())
}

fn hash_bytes(contents: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in contents {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn symlink_metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not inspect {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "phantom-skill-install-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn prepare(root: &Path, force: bool) -> PreparedInstall {
        prepare_install("Test", root, force).unwrap()
    }

    #[test]
    fn installs_updates_and_detects_current_bundle() {
        let temp = TestDir::new();
        let installed = perform_install(prepare(&temp.0, false)).unwrap();
        assert_eq!(installed.status, InstallStatus::Installed);
        assert!(installed.path.join("SKILL.md").is_file());
        assert!(installed.path.join("agents/openai.yaml").is_file());

        assert_eq!(prepare(&temp.0, false).status, InstallStatus::Current);

        fs::write(installed.path.join("SKILL.md"), "local change").unwrap();
        assert!(prepare_install("Test", &temp.0, false).is_err());
        fs::write(installed.path.join("unrelated.txt"), "preserve me").unwrap();
        let updated = perform_install(prepare(&temp.0, true)).unwrap();
        assert_eq!(updated.status, InstallStatus::Updated);
        assert_eq!(
            fs::read_to_string(installed.path.join("unrelated.txt")).unwrap(),
            "preserve me"
        );
        assert_eq!(prepare(&temp.0, false).status, InstallStatus::Current);
    }

    #[test]
    fn unmanaged_collision_requires_force() {
        let temp = TestDir::new();
        let skill_dir = temp.0.join(SKILL_NAME);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "unmanaged").unwrap();
        assert!(prepare_install("Test", &temp.0, false).is_err());
        assert_eq!(prepare(&temp.0, true).status, InstallStatus::Updated);
    }

    #[test]
    fn unchanged_managed_bundle_adds_new_files_and_retires_old_marker_entries() {
        let temp = TestDir::new();
        let installed = perform_install(prepare(&temp.0, false)).unwrap();
        fs::remove_file(installed.path.join("agents/openai.yaml")).unwrap();
        fs::create_dir_all(installed.path.join("references")).unwrap();
        fs::write(installed.path.join("references/old.md"), "old bundled file").unwrap();

        let mut old_manifest = embedded_manifest();
        old_manifest.remove("agents/openai.yaml");
        old_manifest.insert(
            "references/old.md".to_string(),
            hash_bytes(b"old bundled file"),
        );
        fs::write(
            installed.path.join(MARKER_FILE),
            marker_source(&old_manifest),
        )
        .unwrap();

        let prepared = prepare(&temp.0, false);
        assert_eq!(prepared.status, InstallStatus::Updated);
        perform_install(prepared).unwrap();
        assert!(installed.path.join("agents/openai.yaml").is_file());
        assert_eq!(
            fs::read_to_string(installed.path.join("references/old.md")).unwrap(),
            "old bundled file"
        );
        assert_eq!(prepare(&temp.0, false).status, InstallStatus::Current);
    }

    #[test]
    fn byte_identical_unmanaged_bundle_still_requires_force() {
        let temp = TestDir::new();
        let installed = perform_install(prepare(&temp.0, false)).unwrap();
        fs::remove_file(installed.path.join(MARKER_FILE)).unwrap();
        assert!(prepare_install("Test", &temp.0, false).is_err());
        assert_eq!(prepare(&temp.0, true).status, InstallStatus::Updated);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_skill_and_owned_file_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TestDir::new();
        let outside = temp.0.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, temp.0.join(SKILL_NAME)).unwrap();
        assert!(prepare_install("Test", &temp.0, true).is_err());

        fs::remove_file(temp.0.join(SKILL_NAME)).unwrap();
        let installed = perform_install(prepare(&temp.0, false)).unwrap();
        fs::remove_file(installed.path.join("SKILL.md")).unwrap();
        let target = outside.join("target");
        fs::write(&target, "untouched").unwrap();
        symlink(&target, installed.path.join("SKILL.md")).unwrap();
        assert!(prepare_install("Test", &temp.0, true).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "untouched");
    }

    #[cfg(unix)]
    #[test]
    fn retired_marker_paths_never_follow_or_remove_ancestor_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TestDir::new();
        let installed = perform_install(prepare(&temp.0, false)).unwrap();
        let outside = temp.0.join("outside-retired");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("old.md"), "outside stays").unwrap();
        symlink(&outside, installed.path.join("references")).unwrap();

        let mut old_manifest = embedded_manifest();
        old_manifest.insert(
            "references/old.md".to_string(),
            hash_bytes(b"outside stays"),
        );
        fs::write(
            installed.path.join(MARKER_FILE),
            marker_source(&old_manifest),
        )
        .unwrap();

        perform_install(prepare(&temp.0, false)).unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("old.md")).unwrap(),
            "outside stays"
        );
    }
}
