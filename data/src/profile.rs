//! Profile paths and lifetime ownership, independent of the UI and market data.
use std::{
    ffi::OsString,
    fs::{File, TryLockError},
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

struct Profile {
    root: PathBuf,
    explicit: bool,
}

static PROFILE: OnceLock<Profile> = OnceLock::new();

/// Resolve the profile once, before logging, state loading or background cleanup.
pub fn initialize(args: impl IntoIterator<Item = OsString>) -> io::Result<PathBuf> {
    let cli_path = parse_args(args)?;
    let configured =
        cli_path.or_else(|| std::env::var_os("FLOWSURFACE_DATA_PATH").map(PathBuf::from));
    let explicit = configured.is_some();
    let root = resolve_root(configured, default_root(), &std::env::current_dir()?)?;
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    PROFILE
        .set(Profile {
            root: root.clone(),
            explicit,
        })
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "profile already initialized"))?;
    Ok(root)
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Option<PathBuf>> {
    let mut args = args.into_iter();
    let mut root = None;
    while let Some(arg) = args.next() {
        if arg != "--profile-dir" || root.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Usage: flowsurface [--profile-dir <directory>]",
            ));
        }
        let path = args
            .next()
            .filter(|value| !value.is_empty() && value != "--profile-dir")
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--profile-dir requires a directory",
                )
            })?;
        root = Some(PathBuf::from(path));
    }
    Ok(root)
}

fn default_root() -> PathBuf {
    dirs_next::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowsurface")
}

fn resolve_root(configured: Option<PathBuf>, default: PathBuf, cwd: &Path) -> io::Result<PathBuf> {
    let root = configured.unwrap_or(default);
    if root.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile directory must not be empty",
        ));
    }
    Ok(if root.is_absolute() {
        root
    } else {
        cwd.join(root)
    })
}

pub fn data_path(name: Option<&str>) -> PathBuf {
    let root = PROFILE
        .get()
        .map(|profile| profile.root.clone())
        .unwrap_or_else(|| {
            std::env::var_os("FLOWSURFACE_DATA_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(default_root)
        });
    name.map_or_else(|| root.clone(), |name| root.join(name))
}

/// Explicit profiles keep logs inside the profile, even if a legacy log override
/// is inherited by a launcher. Default-profile behavior stays compatible.
pub fn is_explicit() -> bool {
    PROFILE.get().is_some_and(|profile| profile.explicit)
}

pub fn label() -> Option<String> {
    PROFILE
        .get()
        .filter(|profile| profile.explicit)
        .and_then(|profile| profile.root.file_name())
        .map(|name| name.to_string_lossy().into_owned())
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error(
        "This profile is already open: {0}. Close its existing terminal normally, or choose a different --profile-dir."
    )]
    AlreadyOpen(PathBuf),
    #[error("Cannot lock profile {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

/// The file stays on disk; ownership is the OS lock, never the file's existence.
/// Do not unlink it on drop: a racing process could lock a different file.
pub struct ProfileLock {
    _file: File,
}

impl ProfileLock {
    pub fn acquire(root: &Path) -> Result<Self, LockError> {
        let io_error = |source| LockError::Io {
            path: root.to_path_buf(),
            source,
        };
        std::fs::create_dir_all(root).map_err(io_error)?;
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("profile.lock"))
            .map_err(io_error)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(LockError::AlreadyOpen(root.to_path_buf())),
            Err(TryLockError::Error(error)) => Err(io_error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("flowdepth-profile-{}-{stamp}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn profile_arguments_and_relative_roots_are_explicit() {
        let root = parse_args(["--profile-dir", "test profile"].map(OsString::from)).unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            resolve_root(root, PathBuf::from("unused"), &cwd).unwrap(),
            cwd.join("test profile")
        );
        assert!(parse_args(["--profile-dir"].map(OsString::from)).is_err());
        assert!(parse_args(["--profile-dir", ""].map(OsString::from)).is_err());
        assert!(
            parse_args(["--profile-dir", "a", "--profile-dir", "b"].map(OsString::from)).is_err()
        );
        assert!(parse_args(["--typo"].map(OsString::from)).is_err());
        assert!(resolve_root(Some(PathBuf::new()), cwd.clone(), &cwd).is_err());
        assert_eq!(resolve_root(None, cwd.clone(), &cwd).unwrap(), cwd);
    }

    #[test]
    fn independent_profiles_can_run_together_and_reopen_after_drop() {
        let scratch = Scratch::new();
        let first = scratch.0.join("one");
        let second = scratch.0.join("two");
        let lock = ProfileLock::acquire(&first).unwrap();
        let other = ProfileLock::acquire(&second).unwrap();
        assert!(matches!(
            ProfileLock::acquire(&first),
            Err(LockError::AlreadyOpen(_))
        ));
        drop(lock);
        assert!(first.join("profile.lock").exists());
        let reopened = ProfileLock::acquire(&first).unwrap();
        drop((other, reopened));
    }

    #[test]
    fn second_process_cannot_touch_profile_and_process_exit_releases_lock() {
        let scratch = Scratch::new();
        let lock = ProfileLock::acquire(&scratch.0).unwrap();
        for file in ["saved-state.json", "market.redb", "flowsurface-current.log"] {
            std::fs::write(scratch.0.join(file), "unchanged").unwrap();
        }
        run_child(&scratch.0, true);
        for file in ["saved-state.json", "market.redb", "flowsurface-current.log"] {
            assert_eq!(
                std::fs::read_to_string(scratch.0.join(file)).unwrap(),
                "unchanged"
            );
        }
        drop(lock);
        run_child(&scratch.0, false);
        let _reopened = ProfileLock::acquire(&scratch.0).unwrap();
    }

    fn run_child(root: &Path, expect_busy: bool) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "profile::tests::lock_child",
                "--ignored",
                "--nocapture",
            ])
            .env("FLOWDEPTH_LOCK_TEST_ROOT", root)
            .env(
                "FLOWDEPTH_LOCK_TEST_BUSY",
                if expect_busy { "1" } else { "0" },
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn initialized_profiles_isolate_state_cache_and_logs() {
        let scratch = Scratch::new();
        let original = scratch.0.join("original");
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("sentinel"), "untouched").unwrap();
        for cli in [true, false] {
            let target = scratch.0.join(if cli { "cli" } else { "env" });
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "profile::tests::initialization_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("FLOWDEPTH_PROFILE_TEST_ROOT", &target)
                .env("FLOWDEPTH_PROFILE_TEST_CLI", if cli { "1" } else { "0" })
                .env(
                    "FLOWSURFACE_DATA_PATH",
                    if cli { &original } else { &target },
                )
                .env("FLOWSURFACE_LOG_DIR", &original)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(target.join("saved-state.json").is_file());
            assert!(target.join("market_data/cache/probe").is_file());
            assert!(target.join("logs/flowsurface-current.log").is_file());
        }
        assert_eq!(std::fs::read_dir(&original).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(original.join("sentinel")).unwrap(),
            "untouched"
        );
    }

    #[test]
    #[ignore = "subprocess helper; exercised by initialized_profiles test"]
    fn initialization_child() {
        let expected = PathBuf::from(std::env::var_os("FLOWDEPTH_PROFILE_TEST_ROOT").unwrap());
        let args = if std::env::var("FLOWDEPTH_PROFILE_TEST_CLI").unwrap() == "1" {
            vec![
                OsString::from("--profile-dir"),
                expected.clone().into_os_string(),
            ]
        } else {
            vec![]
        };
        let root = initialize(args).unwrap();
        let _lock = ProfileLock::acquire(&root).unwrap();
        assert_eq!(root, expected.canonicalize().unwrap());
        assert_eq!(
            label(),
            expected
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        );
        crate::save_saved_state_atomic(&crate::State::default()).unwrap();
        let cache = crate::data_path(Some("market_data/cache"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("probe"), "cache").unwrap();
        std::fs::write(crate::log::path().unwrap(), "log").unwrap();
    }

    #[test]
    #[ignore = "subprocess helper; exercised by second_process test"]
    fn lock_child() {
        let root = PathBuf::from(std::env::var_os("FLOWDEPTH_LOCK_TEST_ROOT").unwrap());
        let result = ProfileLock::acquire(&root);
        if std::env::var("FLOWDEPTH_LOCK_TEST_BUSY").unwrap() == "1" {
            assert!(matches!(result, Err(LockError::AlreadyOpen(_))));
        } else {
            let _lock = result.unwrap();
            // Exit without Rust destructors: OS must release the lock itself.
            std::process::exit(0);
        }
    }
}
