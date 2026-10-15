use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use rusqlite::{
    Connection, Error as SqlError, ErrorCode as SqlErrorCode, Transaction, TransactionBehavior,
};

use crate::error::{PodError, Result};
use crate::paths::Paths;

pub mod rows;
pub mod schema;
pub mod signal;

/// What [`Store::probe`] found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Probe {
    pub version: u32,
    pub integrity_ok: bool,
    pub writable: bool,
}

pub struct Store {
    connection: Mutex<Connection>,
    signal_path: PathBuf,
}

pub struct Tx<'a>(pub &'a Transaction<'a>);

impl Store {
    pub fn open(paths: &Paths) -> Result<Store> {
        // The state root (`~/.local/state/pod` or `POD_STATE_DIR`) is pod's own
        // directory, so a root left `0755` by an earlier tool is tightened too
        // (spec 15); its parents keep their mode.
        if let Some(root) = paths.state_dir.parent() {
            create_private_dir(root)?;
        }
        create_private_dir(&paths.state_dir)?;
        create_private_dir(&paths.board_dir)?;
        create_private_file(&paths.db_path)?;
        let connection = Connection::open(&paths.db_path).map_err(|error| {
            PodError::store(format!(
                "could not open {}: {error}",
                paths.db_path.display()
            ))
        })?;
        set_private_file(&paths.db_path)?;
        // The busy handler must be in place before the first statement, and the
        // WAL switch and the migration are retried: a second `pod` opening a fresh
        // store at the same moment can see SQLITE_BUSY without the handler running.
        connection.busy_timeout(Duration::from_millis(250))?;
        retry_busy(|| connection.pragma_update(None, "journal_mode", "WAL"))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let transaction = begin_immediate(&connection)?;
        // Read inside the write transaction so two first-time openers never both
        // run the same migration.
        let version =
            transaction.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?;
        if version > schema::LATEST_VERSION {
            return Err(PodError::store(format!(
                "store user_version {version} is newer than supported version {}",
                schema::LATEST_VERSION
            )));
        }
        schema::migrate(&transaction, version)?;
        transaction.commit()?;
        Ok(Store {
            connection: Mutex::new(connection),
            signal_path: paths.signal_path.clone(),
        })
    }

    pub fn tx<T>(&self, f: impl FnOnce(&Tx) -> Result<T>) -> Result<T> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PodError::store("store mutex poisoned"))?;
        let transaction = begin_immediate(&connection)?;
        let value = f(&Tx(&transaction))?;
        transaction.commit()?;
        drop(connection);
        // The commit stands even if the touch fails; reporting an error here would
        // invite a retry of a mutation that already happened. Waiters still see the
        // commit through their `data_version` poll.
        let _ = signal::touch(&self.signal_path);
        Ok(value)
    }

    pub fn read<T>(&self, f: impl FnOnce(&Tx) -> Result<T>) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| PodError::store("store mutex poisoned"))?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let value = f(&Tx(&transaction))?;
        transaction.commit()?;
        Ok(value)
    }

    pub fn user_version(&self) -> Result<u32> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PodError::store("store mutex poisoned"))?;
        Ok(connection.pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// Reads an existing store's schema version and integrity, and proves
    /// it writable, without migrating it: a newer `pod doctor` must not
    /// upgrade a store that older builds on the machine still use.
    pub fn probe(paths: &Paths) -> Result<Probe> {
        use rusqlite::OpenFlags;
        let connection =
            Connection::open_with_flags(&paths.db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                .map_err(|error| {
                    PodError::store(format!(
                        "could not open {}: {error}",
                        paths.db_path.display()
                    ))
                })?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        let writable = begin_immediate(&connection)
            .and_then(|transaction| Ok(transaction.rollback()?))
            .is_ok();
        Ok(Probe {
            version,
            integrity_ok: integrity == "ok",
            writable,
        })
    }

    pub fn integrity_ok(&self) -> Result<bool> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PodError::store("store mutex poisoned"))?;
        let result: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }
}

/// Runs `f`, retrying up to 5 times 50 ms apart while SQLite reports busy.
fn retry_busy<T>(mut f: impl FnMut() -> rusqlite::Result<T>) -> Result<T> {
    let mut attempts = 0;
    loop {
        match f() {
            Ok(value) => return Ok(value),
            Err(error) if is_busy(&error) && attempts < 5 => {
                attempts += 1;
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Begins an IMMEDIATE transaction with the bounded busy retry. The caller
/// holds the connection exclusively (owned, or behind the store mutex).
fn begin_immediate(connection: &Connection) -> Result<Transaction<'_>> {
    retry_busy(|| Transaction::new_unchecked(connection, TransactionBehavior::Immediate))
}

fn is_busy(error: &SqlError) -> bool {
    matches!(error, SqlError::SqliteFailure(failure, _) if matches!(failure.code, SqlErrorCode::DatabaseBusy | SqlErrorCode::DatabaseLocked))
}

/// Creates `path` and any missing parents as `0700`, then forces `0700` on
/// `path` itself. Existing parents (for example `~/.local/state`) keep their mode.
fn create_private_dir(path: &std::path::Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        PodError::store(format!("could not create {}: {error}", path.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            PodError::store(format!(
                "could not set permissions on {}: {error}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

/// Creates the database file as `0600` before SQLite opens it, so it is never
/// readable by others, not even between creation and the `chmod`.
fn create_private_file(path: &std::path::Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map(drop)
        .map_err(|error| PodError::store(format!("could not create {}: {error}", path.display())))
}

fn set_private_file(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            PodError::store(format!(
                "could not set permissions on {}: {error}",
                path.display()
            ))
        })?;
    }
    Ok(())
}
