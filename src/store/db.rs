use crate::provider::PayError;
use crate::store::MigrationLog;
use redb::{Database, ReadableDatabase, TableDefinition};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA_TABLE: TableDefinition<&str, u64> = TableDefinition::new("_schema");
const VERSION_KEY: &str = "version";

pub type Migration<'a> = &'a dyn Fn(&Database) -> Result<(), PayError>;

static MIGRATION_LOG: Mutex<Vec<MigrationLog>> = Mutex::new(Vec::new());

/// Drain all migration log entries accumulated since last drain.
pub fn drain_migration_log() -> Vec<MigrationLog> {
    match MIGRATION_LOG.lock() {
        Ok(mut log) => std::mem::take(&mut *log),
        Err(_) => Vec::new(),
    }
}

fn push_migration_log(entry: MigrationLog) {
    if let Ok(mut log) = MIGRATION_LOG.lock() {
        log.push(entry);
    }
}

pub fn open_database(path: &Path) -> Result<Database, PayError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| PayError::InternalError(format!("mkdir {}: {e}", parent.display())))?;
    }
    let db = if path.exists() {
        Database::open(path)
    } else {
        Database::create(path)
    }
    .map_err(|e| PayError::InternalError(format!("open {}: {e}", path.display())))?;
    Ok(db)
}

pub fn open_and_migrate(
    path: &Path,
    target_version: u64,
    migrations: &[Migration<'_>],
) -> Result<Database, PayError> {
    let db = open_database(path)?;
    let current = read_schema_version(&db)?;

    if current < target_version {
        if migrations.len() < target_version as usize {
            return Err(PayError::InternalError(format!(
                "schema: need {} migrations but only {} provided for {}",
                target_version,
                migrations.len(),
                path.display()
            )));
        }
        for v in current..target_version {
            migrations[v as usize](&db)?;
        }
        write_schema_version(&db, target_version)?;
        push_migration_log(MigrationLog {
            database: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            from_version: current,
            to_version: target_version,
        });
    }

    Ok(db)
}

fn read_schema_version(db: &Database) -> Result<u64, PayError> {
    let read_txn = db
        .begin_read()
        .map_err(|e| PayError::InternalError(format!("schema begin_read: {e}")))?;
    let Ok(table) = read_txn.open_table(SCHEMA_TABLE) else {
        return Ok(0);
    };
    match table
        .get(VERSION_KEY)
        .map_err(|e| PayError::InternalError(format!("schema read version: {e}")))?
    {
        Some(v) => Ok(v.value()),
        None => Ok(0),
    }
}

fn write_schema_version(db: &Database, version: u64) -> Result<(), PayError> {
    let write_txn = db
        .begin_write()
        .map_err(|e| PayError::InternalError(format!("schema begin_write: {e}")))?;
    {
        let mut table = write_txn
            .open_table(SCHEMA_TABLE)
            .map_err(|e| PayError::InternalError(format!("schema open _schema: {e}")))?;
        table
            .insert(VERSION_KEY, version)
            .map_err(|e| PayError::InternalError(format!("schema write version: {e}")))?;
    }
    write_txn
        .commit()
        .map_err(|e| PayError::InternalError(format!("schema commit: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_database_has_version_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.redb");
        let db = open_database(&path).unwrap();
        assert_eq!(read_schema_version(&db).unwrap(), 0);
    }

    #[test]
    fn open_and_migrate_stamps_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stamps.redb");
        let _ = drain_migration_log();
        let db = open_and_migrate(&path, 1, &[&|_db| Ok(())]).unwrap();
        assert_eq!(read_schema_version(&db).unwrap(), 1);
        let log = drain_migration_log();
        let ours: Vec<_> = log.iter().filter(|e| e.database == "stamps.redb").collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0].from_version, 0);
        assert_eq!(ours[0].to_version, 1);
    }

    #[test]
    fn open_and_migrate_skips_when_current() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skip.redb");

        let _db = open_and_migrate(&path, 1, &[&|_db| Ok(())]).unwrap();
        drop(_db);
        let _ = drain_migration_log();

        // Second open — no migration, no log
        let db = open_and_migrate(&path, 1, &[&|_db| Ok(())]).unwrap();
        assert_eq!(read_schema_version(&db).unwrap(), 1);
        let log = drain_migration_log();
        let ours: Vec<_> = log.iter().filter(|e| e.database == "skip.redb").collect();
        assert!(ours.is_empty());
    }

    #[test]
    fn open_and_migrate_runs_sequential_migrations() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.redb");

        let marker: TableDefinition<&str, u64> = TableDefinition::new("_test_marker");

        let db = open_and_migrate(
            &path,
            2,
            &[
                &|db| {
                    let w = db
                        .begin_write()
                        .map_err(|e| PayError::InternalError(e.to_string()))?;
                    {
                        let mut t = w
                            .open_table(TableDefinition::<&str, u64>::new("_test_marker"))
                            .map_err(|e| PayError::InternalError(e.to_string()))?;
                        t.insert("v0_to_v1", 1u64)
                            .map_err(|e| PayError::InternalError(e.to_string()))?;
                    }
                    w.commit()
                        .map_err(|e| PayError::InternalError(e.to_string()))?;
                    Ok(())
                },
                &|db| {
                    let w = db
                        .begin_write()
                        .map_err(|e| PayError::InternalError(e.to_string()))?;
                    {
                        let mut t = w
                            .open_table(TableDefinition::<&str, u64>::new("_test_marker"))
                            .map_err(|e| PayError::InternalError(e.to_string()))?;
                        t.insert("v1_to_v2", 2u64)
                            .map_err(|e| PayError::InternalError(e.to_string()))?;
                    }
                    w.commit()
                        .map_err(|e| PayError::InternalError(e.to_string()))?;
                    Ok(())
                },
            ],
        )
        .unwrap();

        assert_eq!(read_schema_version(&db).unwrap(), 2);

        let r = db.begin_read().unwrap();
        let t = r.open_table(marker).unwrap();
        assert_eq!(t.get("v0_to_v1").unwrap().unwrap().value(), 1);
        assert_eq!(t.get("v1_to_v2").unwrap().unwrap().value(), 2);
    }
}
