use std::fmt;
use std::io::{self, Read};
use std::path::{Component, Path};

#[derive(Debug)]
pub enum BoundedReadError {
    Io(io::Error),
    InvalidPath,
    NotRegular,
    TooLarge { max_bytes: usize },
    UnsupportedPlatform,
}

impl BoundedReadError {
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Self::Io(error) => Some(error.kind()),
            _ => None,
        }
    }
}

impl fmt::Display for BoundedReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::InvalidPath => write!(formatter, "path must be absolute and contain no '..'"),
            Self::NotRegular => write!(formatter, "not a regular file"),
            Self::TooLarge { max_bytes } => {
                write!(formatter, "file exceeds the {max_bytes}-byte limit")
            }
            Self::UnsupportedPlatform => {
                write!(
                    formatter,
                    "secure bounded reads are unavailable on this platform"
                )
            }
        }
    }
}

impl std::error::Error for BoundedReadError {}

impl From<io::Error> for BoundedReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Read one regular file through a retained descriptor without following any
/// symlink in its absolute path. The read itself, rather than prior metadata,
/// enforces `max_bytes`, so a growing file cannot exceed the allocation bound.
pub fn read_bounded_regular_file(
    path: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, BoundedReadError> {
    read_bounded_regular_file_with(path, max_bytes, |_| {})
}

fn read_bounded_regular_file_with(
    path: &Path,
    max_bytes: usize,
    after_open: impl FnOnce(&std::fs::File),
) -> Result<Vec<u8>, BoundedReadError> {
    let file = open_regular_file(path)?;
    after_open(&file);
    let read_limit = u64::try_from(max_bytes)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or(BoundedReadError::TooLarge { max_bytes })?;
    let mut source = Vec::new();
    file.take(read_limit).read_to_end(&mut source)?;
    if source.len() > max_bytes {
        return Err(BoundedReadError::TooLarge { max_bytes });
    }
    Ok(source)
}

#[cfg(unix)]
fn open_regular_file(path: &Path) -> Result<std::fs::File, BoundedReadError> {
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;

    fn component(name: &OsStr) -> Result<CString, BoundedReadError> {
        CString::new(name.as_bytes()).map_err(|_| BoundedReadError::InvalidPath)
    }

    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(BoundedReadError::InvalidPath);
    }
    let mut names = Vec::new();
    for path_component in components {
        match path_component {
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(BoundedReadError::InvalidPath)
            }
        }
    }
    let file_name = names.pop().ok_or(BoundedReadError::InvalidPath)?;
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open("/")?;
    for name in names {
        let name = component(name)?;
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error().into());
        }
        directory = unsafe { std::fs::File::from_raw_fd(descriptor) };
    }

    let file_name = component(file_name)?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            file_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    if !file.metadata()?.file_type().is_file() {
        return Err(BoundedReadError::NotRegular);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_file(_path: &Path) -> Result<std::fs::File, BoundedReadError> {
    Err(BoundedReadError::UnsupportedPlatform)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "phantom-bounded-read-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn accepts_the_exact_limit_and_rejects_one_extra_byte() {
        let temp = TestDir::new();
        let path = temp.0.join("input");
        fs::write(&path, b"1234").unwrap();
        assert_eq!(read_bounded_regular_file(&path, 4).unwrap(), b"1234");
        assert!(matches!(
            read_bounded_regular_file(&path, 3),
            Err(BoundedReadError::TooLarge { max_bytes: 3 })
        ));
    }

    #[test]
    fn reads_the_opened_file_when_the_path_is_swapped() {
        let temp = TestDir::new();
        let path = temp.0.join("input");
        let old_path = temp.0.join("opened");
        fs::write(&path, b"trusted").unwrap();

        let source = read_bounded_regular_file_with(&path, 16, |_| {
            fs::rename(&path, &old_path).unwrap();
            fs::write(&path, b"replacement").unwrap();
        })
        .unwrap();

        assert_eq!(source, b"trusted");
    }

    #[test]
    fn growth_after_open_is_still_bounded() {
        let temp = TestDir::new();
        let path = temp.0.join("input");
        fs::write(&path, b"1234").unwrap();

        let result = read_bounded_regular_file_with(&path, 4, |_| {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"5").unwrap();
        });

        assert!(matches!(
            result,
            Err(BoundedReadError::TooLarge { max_bytes: 4 })
        ));
    }

    #[test]
    fn rejects_symlinks_and_special_files() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let temp = TestDir::new();
        let target = temp.0.join("target");
        let link = temp.0.join("link");
        fs::write(&target, b"trusted").unwrap();
        symlink(&target, &link).unwrap();
        assert!(read_bounded_regular_file(&link, 16).is_err());

        let real_dir = temp.0.join("real");
        let linked_dir = temp.0.join("linked-dir");
        fs::create_dir(&real_dir).unwrap();
        fs::write(real_dir.join("input"), b"trusted").unwrap();
        symlink(&real_dir, &linked_dir).unwrap();
        assert!(read_bounded_regular_file(&linked_dir.join("input"), 16).is_err());

        assert!(matches!(
            read_bounded_regular_file(&temp.0, 16),
            Err(BoundedReadError::NotRegular)
        ));

        let fifo = temp.0.join("fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            read_bounded_regular_file(&fifo, 16),
            Err(BoundedReadError::NotRegular)
        ));
    }
}
