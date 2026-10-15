use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{Connection, OpenFlags};

use crate::error::{PodError, Result};

pub fn touch(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| PodError::store(format!("could not touch {}: {error}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                PodError::store(format!(
                    "could not set permissions on {}: {error}",
                    path.display()
                ))
            })?;
    }
    file.write_all(chrono::Utc::now().to_rfc3339().as_bytes())
        .map_err(|error| {
            PodError::store(format!("could not update {}: {error}", path.display()))
        })?;
    Ok(())
}

/// What the signal file and the store looked like at one moment. Take it before
/// querying the store, then wait on it, so a commit landing between the query
/// and the wait still counts as a change instead of becoming the baseline.
pub struct Snapshot {
    path: PathBuf,
    signal: Option<Vec<u8>>,
    store: StoreVersion,
}

/// Records the current state of the signal file at `path` and of the `pod.db`
/// beside it.
/// How often a waiter re-reads the signal file besides watching it.
const SIGNAL_POLL: Duration = Duration::from_millis(25);

pub fn snapshot(path: &Path) -> Snapshot {
    Snapshot {
        path: path.to_path_buf(),
        store: StoreVersion::open(&path.with_file_name(STORE_FILE)),
        signal: data_version(path),
    }
}

/// `snapshot(path).wait(deadline)`: only safe when nothing was read from the
/// store beforehand that the wait is meant to follow up on.
pub fn wait(path: &Path, deadline: Instant) -> Result<bool> {
    snapshot(path).wait(deadline)
}

impl Snapshot {
    fn changed(&self) -> bool {
        data_version(&self.path) != self.signal || self.store.changed()
    }

    /// Waits for the signal file to change, or for a commit to the store beside
    /// it, since the snapshot. Returns `true` on a change (possibly spurious:
    /// callers re-query), `false` at the deadline. Every second it also polls
    /// SQLite's `data_version` (spec 3.2), so a commit whose touch was lost or
    /// whose watch event was dropped still wakes the waiter.
    pub fn wait(&self, deadline: Instant) -> Result<bool> {
        let path = self.path.as_path();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        create_private_dir(parent)?;
        if self.changed() {
            return Ok(true);
        }
        // The watch is the fast path. Creating it can take over 100 ms on
        // macOS under load (FSEvents), so it is set up on a helper thread while
        // this one already polls; the helper keeps it alive until `wait`
        // returns and drops `_stop`. The signal file itself is also re-read
        // every 25 ms, so a slow, dropped, or failed watch only costs latency;
        // the store's `data_version` is polled once a second.
        let (events, receiver) = mpsc::channel::<notify::Result<notify::Event>>();
        let (_stop, stopped) = mpsc::channel::<()>();
        let watched = parent.to_path_buf();
        thread::spawn(move || {
            let sender = events.clone();
            let Ok(mut watcher) = RecommendedWatcher::new(
                move |event| {
                    let _ = sender.send(event);
                },
                Config::default(),
            ) else {
                return;
            };
            if watcher.watch(&watched, RecursiveMode::NonRecursive).is_ok() {
                let _ = stopped.recv();
            }
        });

        let mut next_store_poll = Instant::now() + Duration::from_secs(1);
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            if now >= next_store_poll {
                if self.store.changed() {
                    return Ok(true);
                }
                next_store_poll = now + Duration::from_secs(1);
            }
            if data_version(&self.path) != self.signal {
                return Ok(true);
            }
            let wait_for = (deadline - now).min(SIGNAL_POLL);
            match receiver.recv_timeout(wait_for) {
                // Any event in the directory (the signal file, or `pod.db` and
                // its WAL on a commit whose touch was lost) re-checks both.
                Ok(Ok(_)) if self.changed() => return Ok(true),
                // Unrelated events, watch errors, a timeout, or no watch at
                // all: the polls above decide.
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    thread::sleep(wait_for);
                }
            }
        }
    }
}

/// Creates missing directories as `0700`; existing ones keep their mode.
fn create_private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| PodError::store(format!("could not create {}: {error}", path.display())))
}

fn data_version(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok()
}

/// The store file `crate::paths::resolve` places beside `pod.signal`.
const STORE_FILE: &str = "pod.db";

/// A read-only connection whose `PRAGMA data_version` changes whenever another
/// connection commits. Absent when the store does not exist or cannot be read;
/// the signal file then remains the only source.
struct StoreVersion(Option<(Connection, i64)>);

impl StoreVersion {
    fn open(db_path: &Path) -> StoreVersion {
        if !db_path.exists() {
            return StoreVersion(None);
        }
        let connection = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok();
        StoreVersion(connection.and_then(|connection| {
            let version = Self::read(&connection)?;
            Some((connection, version))
        }))
    }

    fn changed(&self) -> bool {
        self.0.as_ref().is_some_and(|(connection, initial)| {
            Self::read(connection).is_some_and(|version| version != *initial)
        })
    }

    fn read(connection: &Connection) -> Option<i64> {
        connection
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .ok()
    }
}
