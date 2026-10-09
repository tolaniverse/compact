//! Where Compact's state file lives, and how it gets there safely.
//!
//! The core decides *what* is saved ([`compact_core::persist`]); this decides
//! where and how. Two rules matter more than the rest:
//!
//! - **Writes are atomic.** The new state goes to a temporary file that is then
//!   renamed over the old one, so a crash or power cut mid-write leaves the
//!   previous state intact rather than half a file.
//! - **A file this build cannot read is never overwritten.** A corrupt file is
//!   moved aside, not clobbered, and a file from a newer Compact turns saving
//!   off for the session — downgrading should not cost you your tabs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use compact_core::persist::{self, LoadError};
use compact_core::App;

const FILE_NAME: &str = "state";

/// The on-disk home of one profile's state.
pub struct Store {
    path: PathBuf,
    /// The text last written (or read). Saving is skipped when nothing changed,
    /// which is almost every tick.
    last: String,
    /// False when the file on disk must be left alone.
    writable: bool,
}

/// What [`Store::open`] found.
pub struct Opened {
    pub app: App,
    pub store: Store,
    /// Something the user should hear about, e.g. a corrupt file moved aside.
    pub warning: Option<String>,
}

impl Store {
    /// The default location: `~/Library/Application Support/Compact`.
    ///
    /// `COMPACT_STATE_DIR` overrides it, so a test run or a second profile can
    /// point somewhere else without touching the real one.
    ///
    /// Shared with `compact-mcp`, which finds the agent socket in the same
    /// place.
    pub fn default_dir() -> Option<PathBuf> {
        compact_agent::paths::state_dir()
    }

    /// Load the state in `dir`, or start fresh if there is none.
    pub fn open(dir: &Path) -> Opened {
        let path = dir.join(FILE_NAME);
        let fresh = |store: Store, warning: Option<String>| Opened {
            store,
            app: App::fresh(),
            warning,
        };
        let store = Store { path: path.clone(), last: String::new(), writable: true };

        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fresh(store, None),
            Err(e) => {
                // Unreadable is not the same as absent: there may be a perfectly
                // good file we lack permission for. Do not write over it.
                let store = Store { writable: false, ..store };
                return fresh(
                    store,
                    Some(format!("could not read {}: {e}; not saving this session", path.display())),
                );
            }
        };

        match persist::decode(&text) {
            Ok(app) => Opened { app, store: Store { last: text, ..store }, warning: None },
            Err(LoadError::TooNew(v)) => fresh(
                Store { writable: false, ..store },
                Some(format!(
                    "{} was written by a newer Compact (format {v}); \
                     starting empty and not saving, so it is left untouched",
                    path.display()
                )),
            ),
            Err(e) => {
                let aside = path.with_file_name(format!("{FILE_NAME}.corrupt-{}", unix_time()));
                let moved = fs::rename(&path, &aside).is_ok();
                let store = Store { writable: moved, ..store };
                let what = if moved {
                    format!("moved it to {}", aside.display())
                } else {
                    "could not move it aside; not saving this session".to_string()
                };
                fresh(store, Some(format!("{} is unreadable ({e}); {what}", path.display())))
            }
        }
    }

    /// Write `app`'s state if it changed since the last write.
    ///
    /// Errors are returned rather than logged here so the caller can decide how
    /// loudly to complain; a failed save leaves `last` untouched so the next
    /// tick tries again.
    pub fn save(&mut self, app: &App) -> std::io::Result<bool> {
        if !self.writable {
            return Ok(false);
        }
        let text = app.snapshot();
        if text == self.last {
            return Ok(false);
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_file_name(format!("{FILE_NAME}.tmp"));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        self.last = text;
        Ok(true)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_core::command::Action;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("compact-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_missing_file_is_a_first_launch_and_the_first_save_creates_it() {
        let dir = scratch("first");
        let mut o = Store::open(&dir);
        assert!(o.warning.is_none());
        assert!(o.app.bookmarks.is_empty());
        assert!(o.store.save(&o.app).unwrap(), "first save writes");
        assert!(!o.store.save(&o.app).unwrap(), "unchanged state is not rewritten");
        assert!(dir.join("state").exists());
        assert!(!dir.join("state.tmp").exists(), "the temporary is renamed away");
    }

    #[test]
    fn what_is_saved_is_what_opens_next_time() {
        let dir = scratch("roundtrip");
        let mut o = Store::open(&dir);
        o.app.settings.startup = compact_core::settings::Startup::RestoreSession;
        o.app.navigate("example.org");
        o.app.apply(Action::ToggleLayout);
        o.store.save(&o.app).unwrap();

        let again = Store::open(&dir);
        assert_eq!(again.app.snapshot(), o.app.snapshot());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_not_overwritten() {
        let dir = scratch("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("state"), "compact-state\t1\nworkspace\tx\n").unwrap();
        let mut o = Store::open(&dir);
        assert!(o.warning.as_deref().unwrap().contains("moved it to"));
        let kept: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("state.corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1, "the bad file is kept for inspection");
        assert!(o.store.save(&o.app).unwrap(), "and a good one takes its place");
    }

    #[test]
    fn a_newer_file_is_left_alone() {
        let dir = scratch("newer");
        fs::create_dir_all(&dir).unwrap();
        let future = "compact-state\t999\n";
        fs::write(dir.join("state"), future).unwrap();
        let mut o = Store::open(&dir);
        assert!(o.warning.is_some());
        assert!(!o.store.save(&o.app).unwrap());
        assert_eq!(fs::read_to_string(dir.join("state")).unwrap(), future);
    }
}
