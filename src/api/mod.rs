//! afpay's HTTP domain API — the resource model, the OpenAPI/JSON Schema
//! contract that describes it, and the exporter that commits that contract to
//! the repository.
//!
//! `mode::rest` owns the process: config, credential, bind address, operator
//! allowlists. This module owns everything from the router inward.

mod model;
mod rate_limit;
mod schema;
mod server;

use std::path::{Path, PathBuf};

use serde_json::Value;

pub use server::{ApiState, router};

/// Write `openapi.json` and the standalone schema tree into `directory`.
///
/// `scripts/projects.sh docs agent-first-pay` runs this into a staging
/// directory and mirrors the result over the committed `openapi/` tree, and
/// the drift test in `api::schema` fails if the two ever disagree.
pub fn export_contract(directory: &Path, force: bool) -> Result<Value, ExportError> {
    let openapi_path = directory.join("openapi.json");
    let schemas_directory = directory.join("schemas");
    let index_path = schemas_directory.join("index.json");
    let schemas = schema::standalone_schemas();

    let mut targets = vec![openapi_path.clone(), index_path.clone()];
    targets.extend(
        schemas
            .keys()
            .map(|filename| schemas_directory.join(filename)),
    );
    if !force && let Some(existing) = targets.iter().find(|path| path.exists()) {
        return Err(ExportError::Exists(existing.clone()));
    }
    if force && schemas_directory.is_dir() {
        remove_stale_schemas(&schemas_directory, &schemas)?;
    }

    write_json(&openapi_path, &schema::openapi_document())?;
    write_json(&index_path, &schema::schema_index())?;
    for (filename, value) in &schemas {
        write_json(&schemas_directory.join(filename), value)?;
    }
    Ok(serde_json::json!({
        "openapi_path": display_path(&openapi_path),
        "schema_index_path": display_path(&index_path),
        "schema_directory_path": display_path(&schemas_directory),
        "schema_count": schemas.len(),
    }))
}

#[derive(Debug)]
pub enum ExportError {
    Exists(PathBuf),
    Io {
        action: &'static str,
        message: String,
    },
}

impl ExportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Exists(_) => "api_contract_exists",
            Self::Io { .. } => "api_contract_write_failed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Exists(path) => format!(
                "generated API contract already exists: {}",
                display_path(path)
            ),
            Self::Io { action, message } => format!("could not {action}: {message}"),
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Exists(_) => Some("repeat with --force to replace the generated contract files"),
            Self::Io { .. } => None,
        }
    }
}

fn remove_stale_schemas(
    directory: &Path,
    current: &std::collections::BTreeMap<String, Value>,
) -> Result<(), ExportError> {
    let entries = std::fs::read_dir(directory).map_err(|error| ExportError::Io {
        action: "read the schema export directory",
        message: error.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| ExportError::Io {
            action: "read a schema export entry",
            message: error.to_string(),
        })?;
        let path = entry.path();
        let filename = entry.file_name().to_string_lossy().to_string();
        if path.is_file() && filename.ends_with(".schema.json") && !current.contains_key(&filename)
        {
            std::fs::remove_file(&path).map_err(|error| ExportError::Io {
                action: "remove a stale generated schema",
                message: error.to_string(),
            })?;
        }
    }
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<(), ExportError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ExportError::Io {
            action: "create the export directory",
            message: error.to_string(),
        })?;
    }
    let mut text = serde_json::to_string_pretty(value).map_err(|error| ExportError::Io {
        action: "serialize the contract",
        message: error.to_string(),
    })?;
    text.push('\n');
    std::fs::write(path, text).map_err(|error| ExportError::Io {
        action: "write the contract",
        message: error.to_string(),
    })
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::export_contract;

    #[test]
    fn export_refuses_to_replace_without_force() {
        let directory = tempfile::tempdir().expect("create temp dir");
        export_contract(directory.path(), false).expect("first export");
        let error = export_contract(directory.path(), false).expect_err("second export refused");
        assert_eq!(error.code(), "api_contract_exists");
        export_contract(directory.path(), true).expect("forced export");
    }

    /// The exporter and the drift test must read the same bytes, so a stale
    /// schema left behind by a renamed DTO has to be swept on --force.
    #[test]
    fn forced_export_sweeps_schemas_the_source_no_longer_generates() {
        let directory = tempfile::tempdir().expect("create temp dir");
        export_contract(directory.path(), false).expect("first export");
        let stale = directory.path().join("schemas").join("gone.schema.json");
        std::fs::write(&stale, "{}").expect("write stale schema");
        export_contract(directory.path(), true).expect("forced export");
        assert!(!stale.exists());
    }
}
