use crate::args::{DataOp, DataOpKind};
use crate::config::VERSION;
use crate::output_fmt;
use crate::store::wallet::WalletMetadata;
use crate::store::{self, PayStore};
use crate::types::{Network, Output, RuntimeConfig, Trace};
use agent_first_data::OutputFormat;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub async fn run_data(op: DataOp) {
    let DataOp {
        kind,
        data_dir,
        output: fmt,
    } = op;
    let data_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);

    match kind {
        DataOpKind::GlobalBackup {
            output_path,
            extra_dirs,
        } => {
            let stamp = utc_stamp();
            let path = output_path.unwrap_or_else(|| format!("./afpay-global-{stamp}.tar.zst"));
            let start = Instant::now();
            match do_global_backup(&data_dir, &path, &stamp, &extra_dirs) {
                Ok(()) => {
                    let out = Output::DataBackedUp {
                        data_dir,
                        path,
                        created_at_utc: stamp,
                        trace: Trace::from_duration(start.elapsed().as_millis() as u64),
                    };
                    emit_output(&out, fmt);
                }
                Err(e) => {
                    emit_cli_error(&e, fmt);
                    std::process::exit(1);
                }
            }
        }
        DataOpKind::GlobalRestore {
            archive_path,
            overwrite,
            pg_url_secret,
            extra_dirs,
        } => {
            let start = Instant::now();
            match do_global_restore(
                &data_dir,
                &archive_path,
                overwrite,
                pg_url_secret.as_deref(),
                &extra_dirs,
            ) {
                Ok(()) => {
                    let out = Output::DataRestored {
                        data_dir,
                        path: archive_path,
                        trace: Trace::from_duration(start.elapsed().as_millis() as u64),
                    };
                    emit_output(&out, fmt);
                }
                Err(e) => {
                    emit_cli_error(&e, fmt);
                    std::process::exit(1);
                }
            }
        }
        DataOpKind::NetworkBackup {
            network,
            output_path,
            wallet,
        } => {
            let stamp = utc_stamp();
            let path = output_path.unwrap_or_else(|| format!("./afpay-{network}-{stamp}.tar.zst"));
            let start = Instant::now();
            match do_network_backup(&data_dir, network, &path, &stamp, wallet.as_deref()) {
                Ok(()) => {
                    let out = Output::NetworkDataBackedUp {
                        network: network.to_string(),
                        data_dir,
                        path,
                        created_at_utc: stamp,
                        trace: Trace::from_duration(start.elapsed().as_millis() as u64),
                    };
                    emit_output(&out, fmt);
                }
                Err(e) => {
                    emit_cli_error(&e, fmt);
                    std::process::exit(1);
                }
            }
        }
        DataOpKind::NetworkRestore {
            network,
            archive_path,
            overwrite,
            pg_url_secret,
        } => {
            let start = Instant::now();
            match do_network_restore(
                &data_dir,
                network,
                &archive_path,
                overwrite,
                pg_url_secret.as_deref(),
            ) {
                Ok(()) => {
                    let out = Output::NetworkDataRestored {
                        network: network.to_string(),
                        data_dir,
                        path: archive_path,
                        trace: Trace::from_duration(start.elapsed().as_millis() as u64),
                    };
                    emit_output(&out, fmt);
                }
                Err(e) => {
                    emit_cli_error(&e, fmt);
                    std::process::exit(1);
                }
            }
        }
    }
}

// ─── global backup ────────────────────────────────────────────────────────────

pub(crate) fn do_global_backup(
    data_dir: &str,
    archive_path: &str,
    stamp: &str,
    extra_dirs: &[(String, String)],
) -> Result<(), String> {
    // Run pg_dump before opening the output file so we know HAS_PGDUMP upfront.
    let pg_dump_bytes = try_pg_dump(data_dir)?;

    let archive_p = Path::new(archive_path);
    if let Some(parent) = archive_p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create output directory: {e}"))?;
        }
    }

    let file =
        std::fs::File::create(archive_path).map_err(|e| format!("create archive file: {e}"))?;
    let enc = zstd::Encoder::new(file, 3).map_err(|e| format!("init zstd encoder: {e}"))?;
    let enc = enc.auto_finish();
    let mut tar = tar::Builder::new(enc);

    // manifest.env
    let extra_labels: Vec<&str> = extra_dirs.iter().map(|(l, _)| l.as_str()).collect();
    let manifest = build_global_manifest(stamp, data_dir, pg_dump_bytes.is_some(), &extra_labels);
    append_bytes(&mut tar, "manifest.env", manifest.as_bytes(), 0o644)?;

    // data/ — full afpay data_dir
    tar.append_dir_all("data", data_dir)
        .map_err(|e| format!("archive data directory '{data_dir}': {e}"))?;

    // extra/<label>/ — additional directories (e.g. phoenixd)
    for (label, path) in extra_dirs {
        validate_extra_label(label)?;
        if !Path::new(path).exists() {
            return Err(format!("extra-dir '{label}' not found: {path}"));
        }
        tar.append_dir_all(format!("extra/{label}"), path)
            .map_err(|e| format!("archive extra dir '{label}' ({path}): {e}"))?;
    }

    // pgdump.sql — PostgreSQL dump (permissions 0o600: contains credentials)
    if let Some(bytes) = pg_dump_bytes {
        append_bytes(&mut tar, "pgdump.sql", &bytes, 0o600)?;
    }

    tar.finish().map_err(|e| format!("finalize archive: {e}"))?;
    Ok(())
}

// ─── global restore ───────────────────────────────────────────────────────────

/// Restore from a global backup archive.
///
/// - `overwrite = true`  → clear data_dir first (full replacement)
/// - `overwrite = false` → extract on top (merge: overwrite conflicts, keep others)
/// - `pg_url`            → override PG URL; falls back to restored config.toml
/// - `extra_dirs`        → `[(label, target_path)]` for extra/ entries
pub(crate) fn do_global_restore(
    data_dir: &str,
    archive_path: &str,
    overwrite: bool,
    pg_url: Option<&str>,
    extra_dirs: &[(String, String)],
) -> Result<(), String> {
    if !Path::new(archive_path).is_file() {
        return Err(format!("archive not found: {archive_path}"));
    }

    // Pass 1: validate manifest, collect metadata
    let manifest_content = read_manifest(archive_path)?;
    if !manifest_content.contains("BACKUP_KIND=afpay-global") {
        return Err("archive is not an afpay-global backup (unexpected BACKUP_KIND)".to_string());
    }
    let has_pgdump = manifest_content.contains("HAS_PGDUMP=true");

    let data_path = Path::new(data_dir);

    // Overwrite mode: clear data_dir first
    if overwrite && data_path.exists() {
        clear_dir(data_path)?;
    }
    if !data_path.exists() {
        std::fs::create_dir_all(data_path).map_err(|e| format!("create data directory: {e}"))?;
    }

    // Pass 2: extract data/ and extra/ entries; capture pgdump.sql
    let mut pg_sql: Option<Vec<u8>> = None;
    {
        let file = std::fs::File::open(archive_path).map_err(|e| format!("open archive: {e}"))?;
        let dec = zstd::Decoder::new(file).map_err(|e| format!("init zstd decoder: {e}"))?;
        let mut archive = tar::Archive::new(dec);

        for entry in archive
            .entries()
            .map_err(|e| format!("read archive: {e}"))?
        {
            let mut entry = entry.map_err(|e| format!("read entry: {e}"))?;
            let entry_path = entry.path().map_err(|e| format!("read entry path: {e}"))?;
            let entry_path: PathBuf = entry_path.into_owned();

            // pgdump.sql — capture for later
            if entry_path.as_os_str() == "pgdump.sql" {
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .map_err(|e| format!("read pgdump.sql: {e}"))?;
                pg_sql = Some(buf);
                continue;
            }

            // data/ → data_dir
            if let Ok(rel) = entry_path.strip_prefix("data") {
                if !rel.as_os_str().is_empty() {
                    let dest = data_path.join(rel);
                    ensure_parent(&dest)?;
                    entry
                        .unpack(&dest)
                        .map_err(|e| format!("extract '{}': {e}", rel.display()))?;
                }
                continue;
            }

            // extra/<label>/ → target path from extra_dirs
            if let Ok(rel) = entry_path.strip_prefix("extra") {
                let mut parts = rel.components();
                let label = match parts.next() {
                    Some(c) => c.as_os_str().to_string_lossy().into_owned(),
                    None => continue,
                };
                let inner: PathBuf = parts.collect();
                if inner.as_os_str().is_empty() {
                    continue;
                }
                if let Some((_, target)) = extra_dirs.iter().find(|(l, _)| l == &label) {
                    let dest = Path::new(target).join(&inner);
                    ensure_parent(&dest)?;
                    entry
                        .unpack(&dest)
                        .map_err(|e| format!("extract extra '{label}/{}': {e}", inner.display()))?;
                }
                continue;
            }
        }
    }

    // Restore PostgreSQL if dump is present
    if has_pgdump {
        let sql = pg_sql.ok_or("archive claims HAS_PGDUMP=true but pgdump.sql not found")?;
        let url = match pg_url {
            Some(u) => u.to_string(),
            None => {
                let config = RuntimeConfig::load_from_dir(data_dir)
                    .map_err(|e| format!("read restored config.toml: {e}"))?;
                config.postgres_url_secret.ok_or(
                    "backup contains a PostgreSQL dump but no postgres_url_secret found; \
                     pass --pg-url-secret to specify the target database"
                        .to_string(),
                )?
            }
        };
        run_pg_restore(&url, &sql)?;
    }

    Ok(())
}

// ─── network backup ───────────────────────────────────────────────────────────

fn do_network_backup(
    data_dir: &str,
    network: Network,
    archive_path: &str,
    stamp: &str,
    wallet_filter: Option<&str>,
) -> Result<(), String> {
    let config = RuntimeConfig::load_from_dir(data_dir).unwrap_or_default();
    let backend = store::create_storage_backend(&config).ok_or("no storage backend available")?;

    let mut wallets = backend
        .list_wallet_metadata(Some(network))
        .map_err(|e| format!("list wallets: {e}"))?;

    if let Some(filter) = wallet_filter {
        wallets.retain(|w| w.id == filter || w.label.as_deref() == Some(filter));
        if wallets.is_empty() {
            return Err(format!(
                "no {network} wallet found with id or label: {filter}"
            ));
        }
    }

    let wallets_json =
        serde_json::to_vec_pretty(&wallets).map_err(|e| format!("serialize wallets: {e}"))?;

    let pg_dump_bytes = try_pg_dump(data_dir)?;

    let archive_p = Path::new(archive_path);
    if let Some(parent) = archive_p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create output directory: {e}"))?;
        }
    }

    let file =
        std::fs::File::create(archive_path).map_err(|e| format!("create archive file: {e}"))?;
    let enc = zstd::Encoder::new(file, 3).map_err(|e| format!("init zstd encoder: {e}"))?;
    let enc = enc.auto_finish();
    let mut tar = tar::Builder::new(enc);

    // manifest.env
    let manifest = build_network_manifest(
        stamp,
        data_dir,
        network,
        pg_dump_bytes.is_some(),
        wallets.len(),
    );
    append_bytes(&mut tar, "manifest.env", manifest.as_bytes(), 0o644)?;

    // wallets.json (contains seed_secret — treat as sensitive)
    append_bytes(&mut tar, "wallets.json", &wallets_json, 0o600)?;

    // wallet-data/<id>/ for each wallet (CDK files, BDK files, etc.)
    for wallet in &wallets {
        let wallet_data_path =
            crate::store::wallet::wallet_data_directory_path_for_wallet_metadata(data_dir, wallet);
        if wallet_data_path.is_dir() {
            let tar_path = format!("wallet-data/{}", wallet.id);
            tar.append_dir_all(&tar_path, &wallet_data_path)
                .map_err(|e| format!("archive wallet-data for '{}': {e}", wallet.id))?;
        }
    }

    // pgdump.sql (permissions 0o600)
    if let Some(bytes) = pg_dump_bytes {
        append_bytes(&mut tar, "pgdump.sql", &bytes, 0o600)?;
    }

    tar.finish().map_err(|e| format!("finalize archive: {e}"))?;
    Ok(())
}

// ─── network restore ──────────────────────────────────────────────────────────

fn do_network_restore(
    data_dir: &str,
    network: Network,
    archive_path: &str,
    overwrite: bool,
    pg_url: Option<&str>,
) -> Result<(), String> {
    if !Path::new(archive_path).is_file() {
        return Err(format!("archive not found: {archive_path}"));
    }

    // Pass 1: read manifest, wallets.json, pgdump.sql
    let (wallets, pg_sql) = read_network_archive(archive_path, network)?;

    // Load existing config (network restore doesn't modify config.toml)
    let config =
        RuntimeConfig::load_from_dir(data_dir).map_err(|e| format!("config error: {e}"))?;
    let backend = store::create_storage_backend(&config).ok_or("no storage backend available")?;

    // Overwrite mode: delete existing wallets for this network first
    if overwrite {
        let existing = backend
            .list_wallet_metadata(Some(network))
            .map_err(|e| format!("list existing wallets: {e}"))?;
        for w in &existing {
            let wallet_data_path =
                crate::store::wallet::wallet_data_directory_path_for_wallet_metadata(data_dir, w);
            if wallet_data_path.exists() {
                std::fs::remove_dir_all(&wallet_data_path)
                    .map_err(|e| format!("remove wallet data for '{}': {e}", w.id))?;
            }
            backend
                .delete_wallet_metadata(&w.id)
                .map_err(|e| format!("delete wallet '{}': {e}", w.id))?;
        }
    }

    // Restore wallet metadata
    for wallet in &wallets {
        backend
            .save_wallet_metadata(wallet)
            .map_err(|e| format!("save wallet '{}': {e}", wallet.id))?;
    }

    // Pass 2: extract wallet-data/<id>/ to {data_dir}/wallets/<id>/wallet-data/
    {
        let file = std::fs::File::open(archive_path).map_err(|e| format!("open archive: {e}"))?;
        let dec = zstd::Decoder::new(file).map_err(|e| format!("init zstd decoder: {e}"))?;
        let mut archive = tar::Archive::new(dec);

        for entry in archive
            .entries()
            .map_err(|e| format!("read archive: {e}"))?
        {
            let mut entry = entry.map_err(|e| format!("read entry: {e}"))?;
            let entry_path = entry.path().map_err(|e| format!("read entry path: {e}"))?;
            let entry_path: PathBuf = entry_path.into_owned();

            if let Ok(rel) = entry_path.strip_prefix("wallet-data") {
                let mut parts = rel.components();
                let wallet_id = match parts.next() {
                    Some(c) => c.as_os_str().to_string_lossy().into_owned(),
                    None => continue,
                };
                let inner: PathBuf = parts.collect();
                if inner.as_os_str().is_empty() {
                    continue;
                }
                let dest = Path::new(data_dir)
                    .join("wallets")
                    .join(&wallet_id)
                    .join("wallet-data")
                    .join(&inner);
                ensure_parent(&dest)?;
                entry.unpack(&dest).map_err(|e| {
                    format!(
                        "extract wallet-data for '{wallet_id}/{}': {e}",
                        inner.display()
                    )
                })?;
            }
        }
    }

    // Restore PostgreSQL if dump is present
    if let Some(sql) = pg_sql {
        let url = match pg_url {
            Some(u) => u.to_string(),
            None => config.postgres_url_secret.ok_or(
                "backup contains a PostgreSQL dump but no postgres_url_secret found; \
                 pass --pg-url-secret to specify the target database"
                    .to_string(),
            )?,
        };
        run_pg_restore(&url, &sql)?;
    }

    Ok(())
}

fn read_network_archive(
    archive_path: &str,
    network: Network,
) -> Result<(Vec<WalletMetadata>, Option<Vec<u8>>), String> {
    let file = std::fs::File::open(archive_path).map_err(|e| format!("open archive: {e}"))?;
    let dec = zstd::Decoder::new(file).map_err(|e| format!("init zstd decoder: {e}"))?;
    let mut archive = tar::Archive::new(dec);

    let mut wallets: Option<Vec<WalletMetadata>> = None;
    let mut pg_sql: Option<Vec<u8>> = None;
    let mut manifest_ok = false;

    for entry in archive
        .entries()
        .map_err(|e| format!("read archive: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let path = entry.path().map_err(|e| format!("read entry path: {e}"))?;
        let path: PathBuf = path.into_owned();

        if path.as_os_str() == "manifest.env" {
            let mut content = String::new();
            entry
                .read_to_string(&mut content)
                .map_err(|e| format!("read manifest: {e}"))?;
            let expected = format!("NETWORK={network}");
            if !content.contains("BACKUP_KIND=afpay-network") || !content.contains(&expected) {
                return Err(format!(
                    "archive is not an afpay-network backup for {network}"
                ));
            }
            manifest_ok = true;
        } else if path.as_os_str() == "wallets.json" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("read wallets.json: {e}"))?;
            wallets =
                Some(serde_json::from_slice(&buf).map_err(|e| format!("parse wallets.json: {e}"))?);
        } else if path.as_os_str() == "pgdump.sql" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("read pgdump.sql: {e}"))?;
            pg_sql = Some(buf);
        }
    }

    if !manifest_ok {
        return Err("archive has no manifest.env; may not be a valid afpay backup".to_string());
    }
    Ok((wallets.unwrap_or_default(), pg_sql))
}

// ─── pg helpers ───────────────────────────────────────────────────────────────

fn try_pg_dump(data_dir: &str) -> Result<Option<Vec<u8>>, String> {
    let config = RuntimeConfig::load_from_dir(data_dir).unwrap_or_default();
    if config.storage_backend.as_deref() != Some("postgres") {
        return Ok(None);
    }
    let url = match config.postgres_url_secret {
        Some(u) => u,
        None => {
            return Err("storage_backend=postgres but postgres_url_secret is not set".to_string())
        }
    };
    run_pg_dump(&url).map(Some)
}

fn run_pg_dump(url: &str) -> Result<Vec<u8>, String> {
    // Explicitly close stdin so pg_dump never reads from the terminal.
    let out = std::process::Command::new("pg_dump")
        .arg(format!("--dbname={url}"))
        .arg("--no-password")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("pg_dump not found or failed to start: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "pg_dump failed (exit {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn run_pg_restore(url: &str, sql: &[u8]) -> Result<(), String> {
    // stdin is piped so we can write the SQL; stdout/stderr are also redirected
    // so psql never interacts with the calling terminal (important in TUI mode).
    let mut child = std::process::Command::new("psql")
        .arg(format!("--dbname={url}"))
        .arg("--no-password")
        .arg("--quiet")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("psql not found or failed to start: {e}"))?;

    // Write SQL then drop stdin so psql sees EOF and can proceed.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(sql)
            .map_err(|e| format!("write to psql stdin: {e}"))?;
        // drop(stdin) happens here — sends EOF to psql
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("psql wait: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "psql failed (exit {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

// ─── manifest helpers ─────────────────────────────────────────────────────────

fn build_global_manifest(
    stamp: &str,
    data_dir: &str,
    has_pgdump: bool,
    extra_labels: &[&str],
) -> String {
    let mut s = format!(
        "BACKUP_KIND=afpay-global\nCREATED_AT_UTC={stamp}\nAFPAY_VERSION={VERSION}\nDATA_DIR={data_dir}\n"
    );
    s.push_str(&format!("HAS_PGDUMP={has_pgdump}\n"));
    if !extra_labels.is_empty() {
        s.push_str(&format!("EXTRA_DIRS={}\n", extra_labels.join(",")));
    }
    s
}

fn build_network_manifest(
    stamp: &str,
    data_dir: &str,
    network: Network,
    has_pgdump: bool,
    wallet_count: usize,
) -> String {
    format!(
        "BACKUP_KIND=afpay-network\nNETWORK={network}\nCREATED_AT_UTC={stamp}\nAFPAY_VERSION={VERSION}\nDATA_DIR={data_dir}\nHAS_PGDUMP={has_pgdump}\nWALLET_COUNT={wallet_count}\n"
    )
}

fn read_manifest(archive_path: &str) -> Result<String, String> {
    let file = std::fs::File::open(archive_path).map_err(|e| format!("open archive: {e}"))?;
    let dec = zstd::Decoder::new(file).map_err(|e| format!("init zstd decoder: {e}"))?;
    let mut archive = tar::Archive::new(dec);
    for entry in archive
        .entries()
        .map_err(|e| format!("read archive: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let path = entry.path().map_err(|e| format!("read entry path: {e}"))?;
        if path.as_os_str() == "manifest.env" {
            let mut content = String::new();
            entry
                .read_to_string(&mut content)
                .map_err(|e| format!("read manifest: {e}"))?;
            return Ok(content);
        }
    }
    Err("archive has no manifest.env; may not be a valid afpay backup".to_string())
}

// ─── low-level helpers ────────────────────────────────────────────────────────

fn append_bytes<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    path: &str,
    data: &[u8],
    mode: u32,
) -> Result<(), String> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    tar.append_data(&mut header, path, data)
        .map_err(|e| format!("write {path} to archive: {e}"))
}

fn clear_dir(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read directory: {e}"))? {
        let entry = entry.map_err(|e| format!("read directory entry: {e}"))?;
        let p = entry.path();
        if p.is_dir() {
            std::fs::remove_dir_all(&p)
                .map_err(|e| format!("remove directory '{}': {e}", p.display()))?;
        } else {
            std::fs::remove_file(&p).map_err(|e| format!("remove file '{}': {e}", p.display()))?;
        }
    }
    Ok(())
}

fn ensure_parent(dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create directory '{}': {e}", parent.display()))?;
    }
    Ok(())
}

fn validate_extra_label(label: &str) -> Result<(), String> {
    if label.contains('/') || label.contains('\\') || label.contains("..") || label.is_empty() {
        return Err(format!(
            "invalid extra-dir label '{label}': must not contain path separators or be empty"
        ));
    }
    Ok(())
}

fn emit_output(out: &Output, fmt: OutputFormat) {
    let value = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
    let rendered = output_fmt::render_value_with_policy(&value, fmt);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

fn emit_cli_error(msg: &str, fmt: OutputFormat) {
    let value = agent_first_data::build_cli_error(msg, None);
    let rendered = agent_first_data::cli_output(&value, fmt);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

// ─── timestamp ────────────────────────────────────────────────────────────────

pub(crate) fn utc_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let days = secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}{month:02}{day:02}T{hour:02}{min:02}{sec:02}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let diy = if is_leap(year) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }
    let month_days: [u64; 12] = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for md in &month_days {
        if days < *md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}
