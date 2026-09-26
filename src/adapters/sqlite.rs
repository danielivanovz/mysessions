//! Existing agent databases are opened read-only, with a short lock wait.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

pub(super) fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {} read-only", path.display()))?;
    db.busy_timeout(Duration::from_millis(100))?;
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_live_wal_without_allowing_writes_or_creating_missing_database() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scratch")
            .join(format!("sqlite-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS data (value TEXT); DELETE FROM data; INSERT INTO data VALUES ('live');").unwrap();
        let reader = open(&path).unwrap();
        assert_eq!(
            reader
                .query_row("SELECT value FROM data", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "live"
        );
        assert!(reader.is_readonly(rusqlite::DatabaseName::Main).unwrap());
        assert!(reader.execute("DELETE FROM data", []).is_err());
        assert!(open(&dir.join("missing.sqlite")).is_err());
        assert!(!dir.join("missing.sqlite").exists());
        drop(reader);
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
