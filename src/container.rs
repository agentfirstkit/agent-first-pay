//! `afpay container` subcommand. Builds the afpay host image and runs it under
//! Docker, Podman, or Apple Container — one command to stand up a long-lived
//! afpay daemon locally (supervisord runs afpay + optional bitcoind/phoenixd).
//! It embeds the canonical `container/docker/Dockerfile` and by default selects
//! its `downloader` stage, which pulls the matching prebuilt release (version
//! hard-pinned to this binary) — so a brew-only user needs no source tree.
//! `--from-source` instead selects the `builder` stage to compile from a
//! checkout. Mirrors the afhttp implementation, adapted to afpay's multi-process
//! image, deploy modes, and output conventions.

use std::path::{Path, PathBuf};
use std::process::Command;

use agent_first_data::json_result;
use serde_json::{Value, json};

use crate::args::{
    ContainerCliAction, ContainerInstallArgs, ContainerLogsArgs, ContainerRequest,
    ContainerRuntimeArg, ContainerStatusArgs, ContainerUninstallArgs,
};

/// Build context embedded in the binary and written to the cache dir at
/// `install` time. The SAME canonical Dockerfile used for from-source builds —
/// the embedded path just selects its `downloader` stage via
/// `--build-arg AFPAY_BIN_FROM=downloader` (single source of truth, no fork).
const CONTEXT_FILES: &[(&str, &str)] = &[
    (
        "container/docker/Dockerfile",
        include_str!("../container/docker/Dockerfile"),
    ),
    (
        "container/docker/entrypoint.sh",
        include_str!("../container/docker/entrypoint.sh"),
    ),
    (
        "container/docker/supervisord.conf",
        include_str!("../container/docker/supervisord.conf"),
    ),
    (
        "container/docker/container-setup.sh",
        include_str!("../container/docker/container-setup.sh"),
    ),
    (
        "container/docker/conf.d/afpay-setup.conf",
        include_str!("../container/docker/conf.d/afpay-setup.conf"),
    ),
    (
        "container/docker/conf.d/afpay.conf",
        include_str!("../container/docker/conf.d/afpay.conf"),
    ),
    (
        "container/docker/conf.d/bitcoind.conf",
        include_str!("../container/docker/conf.d/bitcoind.conf"),
    ),
    (
        "container/docker/conf.d/phoenixd.conf",
        include_str!("../container/docker/conf.d/phoenixd.conf"),
    ),
];

/// This binary's version — the image downloads exactly this release.
const VERSION: &str = env!("CARGO_PKG_VERSION");
const IMAGE_REPO: &str = "afpay";
const DEFAULT_NAME: &str = "afpay";

/// A failure with an operator-facing message and an optional remediation hint.
#[derive(Debug)]
struct Fail {
    message: String,
    hint: Option<String>,
}

fn fail(message: impl Into<String>) -> Fail {
    Fail {
        message: message.into(),
        hint: None,
    }
}

fn fail_hint(message: impl Into<String>, hint: impl Into<String>) -> Fail {
    Fail {
        message: message.into(),
        hint: Some(hint.into()),
    }
}

pub fn run(req: ContainerRequest) -> i32 {
    let reveal_daemon_secret = match &req.action {
        ContainerCliAction::Install(args) => args.reveal_daemon_secret,
        ContainerCliAction::Status(args) => args.reveal_daemon_secret,
        ContainerCliAction::Uninstall(_) | ContainerCliAction::Logs(_) => false,
    };
    // `logs` streams the runtime's own bytes through this process, so its
    // output contract is raw. A protocol result appended to those bytes would
    // contradict it; a failure still reports as an event on the error stream.
    let raw_output = matches!(req.action, ContainerCliAction::Logs(_));
    let result = match req.action {
        ContainerCliAction::Install(a) => install(a),
        ContainerCliAction::Uninstall(a) => uninstall(a),
        ContainerCliAction::Status(a) => status(a),
        ContainerCliAction::Logs(a) => logs(a),
    };
    if raw_output {
        return match result {
            Ok(_) => 0,
            Err(f) => {
                let _ = crate::output_fmt::emit_process_event(
                    crate::output_fmt::coded_error_event(
                        "container_failed",
                        &f.message,
                        f.hint.as_deref(),
                    ),
                    agent_first_data::OutputFormat::Json,
                );
                1
            }
        };
    }
    let (code, value) = match result {
        Ok(value) => (0, Value::from(json_result(value).build())),
        Err(f) => (
            1,
            crate::output_fmt::coded_error_event("container_failed", &f.message, f.hint.as_deref()),
        ),
    };
    let emitted = if code == 0 && reveal_daemon_secret {
        crate::output_fmt::emit_process_event_with_redaction(
            value,
            req.output,
            agent_first_data::RedactionPolicy::Off,
        )
    } else {
        crate::output_fmt::emit_process_event(value, req.output)
    };
    let _ = emitted;
    code
}

// ── runtime selection ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Runtime {
    Docker,
    Podman,
    Apple,
}

impl Runtime {
    fn bin(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::Podman => "podman",
            Runtime::Apple => "container",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::Podman => "podman",
            Runtime::Apple => "apple",
        }
    }
}

fn resolve_runtime(explicit: Option<ContainerRuntimeArg>) -> Result<Runtime, Fail> {
    if let Some(r) = explicit {
        return Ok(match r {
            ContainerRuntimeArg::Docker => Runtime::Docker,
            ContainerRuntimeArg::Podman => Runtime::Podman,
            ContainerRuntimeArg::Apple => Runtime::Apple,
        });
    }
    if let Some(v) = std::env::var_os("AFPAY_CONTAINER_RUNTIME") {
        return runtime_from_str(v.to_string_lossy().trim());
    }
    if on_path("docker") {
        Ok(Runtime::Docker)
    } else if on_path("podman") {
        Ok(Runtime::Podman)
    } else if on_path("container") {
        Ok(Runtime::Apple)
    } else {
        Err(fail(
            "no container runtime found: install Docker, Podman, or Apple `container`, or pass --runtime",
        ))
    }
}

fn runtime_from_str(value: &str) -> Result<Runtime, Fail> {
    match value {
        "docker" => Ok(Runtime::Docker),
        "podman" => Ok(Runtime::Podman),
        "apple" | "container" => Ok(Runtime::Apple),
        other => Err(fail(format!(
            "invalid container runtime '{other}': expected docker, podman, or apple"
        ))),
    }
}

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
}

/// Apple's runtime needs its daemon started first; on Docker/Podman a no-op.
fn start_daemon(runtime: Runtime) {
    if runtime == Runtime::Apple {
        let _ = capture(runtime.bin(), &["system".into(), "start".into()]);
    }
}

// ── install ──────────────────────────────────────────────────────────────────

fn install(args: ContainerInstallArgs) -> Result<Value, Fail> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let extras = resolve_extras(&args.with)?;
    let allowlists = resolve_allowlists(&args.allow)?;
    let image = image_tag();
    let name = container_name(&args.common.name);

    start_daemon(runtime);

    if args.from_source {
        let ctx = resolve_source_context(args.context.as_deref())?;
        let build = build_args(
            &image,
            runtime,
            BuildSource::FromSource {
                ctx: &ctx,
                features: args.features.as_deref(),
            },
            &extras,
        );
        exec_inherit(runtime.bin(), &build)?;
    } else if args.rebuild || !image_exists(runtime, &image) {
        let ctx = write_build_context()?;
        let target = target_triple(runtime, std::env::consts::ARCH);
        let build = build_args(
            &image,
            runtime,
            BuildSource::Embedded { ctx: &ctx, target },
            &extras,
        );
        exec_inherit(runtime.bin(), &build).map_err(|_| build_failed_error(target))?;
    }

    // Recreate cleanly. Secrets + wallets live in the named volumes, so they are
    // stable across recreation.
    let _ = capture(runtime.bin(), &["stop".into(), name.clone()]);
    let _ = capture(runtime.bin(), &["rm".into(), name.clone()]);

    let run = run_args(&name, &image, &extras, &allowlists, &args);
    exec_inherit(runtime.bin(), &run)?;

    let secret = read_secret(runtime, &name);
    // afpay refuses to start a public listener with an empty allowlist, so warn
    // when none was provided — the daemon will crash-loop until one is set.
    let hint = if allowlists.is_empty() {
        Some(
            "no operator allowlist set: the public listener will not start. Re-run with \
             --allow mint=<url> (or esplora/ln/sol-rpc/evm-rpc/btc-core/btc-electrum) on a fresh \
             data volume, or add allowed_* to config.toml.",
        )
    } else {
        None
    };
    Ok(with_connection_fields(
        json!({
            "code": "container_install",
            "runtime": runtime.label(),
            "image": image,
            "container": name,
            "extras": extras.iter().map(|e| e.name).collect::<Vec<_>>(),
            "hint": hint,
        }),
        args.port,
        secret,
    ))
}

// ── uninstall ────────────────────────────────────────────────────────────────

fn uninstall(args: ContainerUninstallArgs) -> Result<Value, Fail> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let name = container_name(&args.common.name);
    let _ = capture(runtime.bin(), &["stop".into(), name.clone()]);
    let removed = capture(runtime.bin(), &["rm".into(), name.clone()])
        .map(|o| o.status.success())
        .unwrap_or(false);

    let mut image_removed = false;
    if args.purge {
        let image = image_tag();
        image_removed = capture(runtime.bin(), &["rmi".into(), image])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if let Ok(ctx) = cache_context_dir() {
            let _ = std::fs::remove_dir_all(&ctx);
        }
    }

    Ok(json!({
        "code": "container_uninstall",
        "runtime": runtime.label(),
        "container": name,
        "removed": removed,
        "image_removed": image_removed,
        "purged": args.purge,
    }))
}

// ── status ───────────────────────────────────────────────────────────────────

fn status(args: ContainerStatusArgs) -> Result<Value, Fail> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let name = container_name(&args.common.name);
    let running = container_running(runtime, &name);
    let secret = if running {
        read_secret(runtime, &name)
    } else {
        None
    };
    Ok(with_connection_fields(
        json!({
            "code": "container_status",
            "runtime": runtime.label(),
            "container": name,
            "running": running,
        }),
        args.port,
        secret,
    ))
}

// ── logs ─────────────────────────────────────────────────────────────────────

fn logs(args: ContainerLogsArgs) -> Result<Value, Fail> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let name = container_name(&args.common.name);
    let mut argv: Vec<String> = vec!["logs".into()];
    if args.follow {
        argv.push("-f".into());
    }
    argv.push(name);
    exec_inherit(runtime.bin(), &argv)?;
    Ok(json!({ "code": "container_logs", "runtime": runtime.label() }))
}

// ── extras (optional bundled daemons) ────────────────────────────────────────

/// An optional bundled daemon: the `--with` name plus its install build-arg and
/// runtime enable env var.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Extra {
    name: &'static str,
    install_arg: &'static str,
    enable_env: &'static str,
}

const EXTRAS: [Extra; 2] = [
    Extra {
        name: "phoenixd",
        install_arg: "INSTALL_PHOENIXD",
        enable_env: "ENABLE_PHOENIXD",
    },
    Extra {
        name: "bitcoind",
        install_arg: "INSTALL_BITCOIND",
        enable_env: "ENABLE_BITCOIND",
    },
];

fn resolve_extras(names: &[String]) -> Result<Vec<Extra>, Fail> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let extra = EXTRAS.iter().find(|e| e.name == name).ok_or_else(|| {
            fail(format!(
                "unknown extra '{name}': expected one of {}",
                EXTRAS.iter().map(|e| e.name).collect::<Vec<_>>().join(", ")
            ))
        })?;
        if !out.contains(extra) {
            out.push(*extra);
        }
    }
    Ok(out)
}

fn has_extra(extras: &[Extra], name: &str) -> bool {
    extras.iter().any(|e| e.name == name)
}

// ── operator allowlists ──────────────────────────────────────────────────────

/// `--allow <category>` → the `AFPAY_ALLOWED_*` env the entrypoint reads to seed
/// the matching `allowed_*` array in config.toml.
const ALLOW_CATEGORIES: [(&str, &str); 7] = [
    ("mint", "AFPAY_ALLOWED_MINT_URLS"),
    ("esplora", "AFPAY_ALLOWED_ESPLORA_URLS"),
    ("sol-rpc", "AFPAY_ALLOWED_SOL_RPC_ENDPOINTS"),
    ("evm-rpc", "AFPAY_ALLOWED_EVM_RPC_ENDPOINTS"),
    ("btc-core", "AFPAY_ALLOWED_BTC_CORE_URLS"),
    ("btc-electrum", "AFPAY_ALLOWED_BTC_ELECTRUM_URLS"),
    ("ln", "AFPAY_ALLOWED_LN_ENDPOINTS"),
];

/// Parse `<category>=<url>` entries into `(env_var, comma-joined values)`, grouped
/// per category in declaration order.
fn resolve_allowlists(entries: &[String]) -> Result<Vec<(&'static str, String)>, Fail> {
    let mut grouped: Vec<(&'static str, Vec<String>)> = Vec::new();
    for entry in entries {
        let (category, value) = entry.split_once('=').ok_or_else(|| {
            fail(format!(
                "invalid --allow '{entry}': expected <category>=<url>"
            ))
        })?;
        let value = value.trim();
        if value.is_empty() {
            return Err(fail(format!("invalid --allow '{entry}': empty url")));
        }
        let env = ALLOW_CATEGORIES
            .iter()
            .find(|(c, _)| *c == category)
            .map(|(_, e)| *e)
            .ok_or_else(|| {
                fail(format!(
                    "unknown --allow category '{category}': expected one of {}",
                    ALLOW_CATEGORIES
                        .iter()
                        .map(|(c, _)| *c)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        if let Some(slot) = grouped.iter_mut().find(|(e, _)| *e == env) {
            slot.1.push(value.to_string());
        } else {
            grouped.push((env, vec![value.to_string()]));
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(env, values)| (env, values.join(",")))
        .collect())
}

// ── arg builders ─────────────────────────────────────────────────────────────

fn image_tag() -> String {
    format!("{IMAGE_REPO}:{VERSION}")
}

fn container_name(name: &str) -> String {
    if name.is_empty() {
        DEFAULT_NAME.to_string()
    } else {
        name.to_string()
    }
}

fn endpoint_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn with_connection_fields(mut result: Value, port: u16, daemon_secret: Option<String>) -> Value {
    if let Some(fields) = result.as_object_mut() {
        fields.insert("endpoint_url".to_string(), endpoint_url(port).into());
        fields.insert(
            "client_command_secret".to_string(),
            daemon_secret
                .as_deref()
                .map(|secret| client_command(port, secret))
                .into(),
        );
        fields.insert("daemon_secret".to_string(), daemon_secret.into());
    }
    result
}

fn client_command(port: u16, secret: &str) -> String {
    format!("curl http://127.0.0.1:{port}/v1/wallets -H \"Authorization: Bearer {secret}\"")
}

/// The Linux target triple for the image arch. Apple Container always runs
/// linux/arm64; Docker and Podman match the host arch.
fn target_triple(runtime: Runtime, host_arch: &str) -> &'static str {
    match runtime {
        Runtime::Apple => "aarch64-unknown-linux-gnu",
        Runtime::Docker | Runtime::Podman => match host_arch {
            "aarch64" | "arm64" => "aarch64-unknown-linux-gnu",
            _ => "x86_64-unknown-linux-gnu",
        },
    }
}

/// Which `AFPAY_BIN_FROM` stage of the canonical Dockerfile provides the binary.
enum BuildSource<'a> {
    Embedded {
        ctx: &'a Path,
        target: &'a str,
    },
    FromSource {
        ctx: &'a Path,
        features: Option<&'a str>,
    },
}

fn build_args(image: &str, runtime: Runtime, source: BuildSource, extras: &[Extra]) -> Vec<String> {
    let mut a: Vec<String> = vec!["build".into()];
    if runtime == Runtime::Apple {
        a.push("--platform".into());
        a.push("linux/arm64".into());
    }
    let ctx = match source {
        BuildSource::Embedded { ctx, target } => {
            a.push("--build-arg".into());
            a.push("AFPAY_BIN_FROM=downloader".into());
            a.push("--build-arg".into());
            a.push(format!("AFPAY_VERSION={VERSION}"));
            a.push("--build-arg".into());
            a.push(format!("AFPAY_TARGET={target}"));
            ctx
        }
        BuildSource::FromSource { ctx, features } => {
            a.push("--build-arg".into());
            a.push("AFPAY_BIN_FROM=builder".into());
            if let Some(features) = features {
                a.push("--build-arg".into());
                a.push(format!("FEATURES={features}"));
            }
            ctx
        }
    };
    // INSTALL_* are runtime-stage args (bake the daemon binary), applied in both
    // build modes.
    for e in extras {
        a.push("--build-arg".into());
        a.push(format!("{}=true", e.install_arg));
    }
    a.push("-t".into());
    a.push(image.to_string());
    a.push("-f".into());
    a.push(
        ctx.join("container/docker/Dockerfile")
            .to_string_lossy()
            .into_owned(),
    );
    a.push(ctx.to_string_lossy().into_owned());
    a
}

fn run_args(
    name: &str,
    image: &str,
    extras: &[Extra],
    allowlists: &[(&str, String)],
    args: &ContainerInstallArgs,
) -> Vec<String> {
    let mut a: Vec<String> = vec!["run".into(), "-d".into(), "--name".into(), name.to_string()];
    for sub in ["afpay", "bitcoind", "phoenixd"] {
        a.push("-v".into());
        a.push(format!("{name}-{sub}:/data/{sub}"));
    }
    a.push("-e".into());
    a.push(format!("AFPAY_PORT={}", args.port));
    for e in &EXTRAS {
        a.push("-e".into());
        a.push(format!("{}={}", e.enable_env, has_extra(extras, e.name)));
    }
    if has_extra(extras, "bitcoind") {
        a.push("-e".into());
        a.push(format!("BTC_NETWORK={}", args.btc_network));
        a.push("-e".into());
        a.push(format!("BTC_RPC_PORT={}", args.btc_rpc_port));
        a.push("-e".into());
        a.push(format!("BTC_PRUNE_MB={}", args.btc_prune_mb));
    }
    for (env, csv) in allowlists {
        a.push("-e".into());
        a.push(format!("{env}={csv}"));
    }
    a.push("-p".into());
    a.push(format!("127.0.0.1:{0}:{0}", args.port));
    a.push(image.to_string());
    a
}

// ── process plumbing ─────────────────────────────────────────────────────────

fn spawn_fail(bin: &str, err: &std::io::Error) -> Fail {
    if err.kind() == std::io::ErrorKind::NotFound {
        fail(format!("container runtime `{bin}` not found on PATH"))
    } else {
        fail(format!("spawning `{bin}` failed: {err}"))
    }
}

/// Run a runtime command, inheriting stdio so the user sees build/run progress.
fn exec_inherit(bin: &str, args: &[String]) -> Result<(), Fail> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .map_err(|e| spawn_fail(bin, &e))?;
    if status.success() {
        Ok(())
    } else {
        Err(fail(format!(
            "`{bin} {}` failed ({status})",
            args.join(" ")
        )))
    }
}

/// Run a runtime command capturing stdout/stderr.
fn capture(bin: &str, args: &[String]) -> Result<std::process::Output, Fail> {
    Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| spawn_fail(bin, &e))
}

fn image_exists(runtime: Runtime, image: &str) -> bool {
    capture(
        runtime.bin(),
        &["image".into(), "inspect".into(), image.to_string()],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

fn container_running(runtime: Runtime, name: &str) -> bool {
    capture(runtime.bin(), &["ps".into()])
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(name))
        .unwrap_or(false)
}

/// Where the entrypoint persists the daemon's bearer API key inside the data
/// volume. The second path is what earlier images wrote; both are read so a
/// volume created before the rename still resolves.
const SECRET_PATHS: [&str; 2] = [
    "/data/afpay/rest-api-key-secret",
    "/data/afpay/rest-api-key",
];

/// Read the bearer secret the entrypoint persisted to the data volume. The
/// entrypoint writes it on first start, so retry briefly after `run`.
fn read_secret(runtime: Runtime, name: &str) -> Option<String> {
    for attempt in 0..10 {
        for path in SECRET_PATHS {
            let argv = vec![
                "exec".into(),
                name.to_string(),
                "cat".into(),
                path.to_string(),
            ];
            if let Ok(out) = capture(runtime.bin(), &argv)
                && out.status.success()
            {
                let secret = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !secret.is_empty() {
                    return Some(secret);
                }
            }
        }
        if attempt < 9 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    None
}

fn build_failed_error(target: &str) -> Fail {
    fail_hint(
        format!(
            "image build failed. If v{VERSION} has no published release asset for {target}, \
             build from a source checkout instead"
        ),
        "afpay container install --from-source (or docker compose -f container/docker/compose.yaml up --build)",
    )
}

// ── embedded build context ───────────────────────────────────────────────────

fn cache_context_dir() -> Result<PathBuf, Fail> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or_else(|| fail("cannot resolve cache dir: set HOME or XDG_CACHE_HOME"))?;
    Ok(base.join("afpay").join("container").join(VERSION))
}

fn write_build_context() -> Result<PathBuf, Fail> {
    let root = cache_context_dir()?;
    // Mirror the repo's layout so the Dockerfile's COPY paths resolve the same
    // way they do for a from-source build. The downloader stage pulls the binary
    // over the network, so no source tree is needed here.
    for (rel, contents) in CONTEXT_FILES {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| fail(format!("creating {}: {e}", parent.display())))?;
        }
        std::fs::write(&path, contents)
            .map_err(|e| fail(format!("writing {}: {e}", path.display())))?;
    }
    Ok(root)
}

/// Resolve and validate the source checkout for `--from-source`.
fn resolve_source_context(arg: Option<&str>) -> Result<PathBuf, Fail> {
    let dir = match arg {
        Some(p) => PathBuf::from(p),
        None => {
            std::env::current_dir().map_err(|e| fail(format!("cannot read current dir: {e}")))?
        }
    };
    let dockerfile = dir.join("container/docker/Dockerfile");
    if !dockerfile.is_file() {
        return Err(fail_hint(
            format!(
                "--from-source needs a source checkout: {} not found",
                dockerfile.display()
            ),
            "run from the spore root or pass --context <dir>",
        ));
    }
    Ok(dir)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn runtime_from_str_parses_and_rejects() {
        assert!(matches!(runtime_from_str("docker"), Ok(Runtime::Docker)));
        assert!(matches!(runtime_from_str("podman"), Ok(Runtime::Podman)));
        assert!(matches!(runtime_from_str("apple"), Ok(Runtime::Apple)));
        assert!(matches!(runtime_from_str("container"), Ok(Runtime::Apple)));
        assert!(runtime_from_str("nerdctl").is_err());
    }

    #[test]
    fn target_triple_tracks_runtime_and_arch() {
        assert_eq!(
            target_triple(Runtime::Apple, "x86_64"),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            target_triple(Runtime::Docker, "aarch64"),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            target_triple(Runtime::Podman, "x86_64"),
            "x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn extras_map_to_args_and_reject_unknown() {
        let resolved = resolve_extras(&["phoenixd".into(), "bitcoind".into()]).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].install_arg, "INSTALL_PHOENIXD");
        assert_eq!(resolved[1].enable_env, "ENABLE_BITCOIND");
        // Duplicates collapse.
        assert_eq!(
            resolve_extras(&["phoenixd".into(), "phoenixd".into()])
                .unwrap()
                .len(),
            1
        );
        assert!(resolve_extras(&["lnd".into()]).is_err());
    }

    #[test]
    fn embedded_build_args_select_downloader_and_install_extras() {
        let ctx = PathBuf::from("/cache/ctx");
        let extras = resolve_extras(&["phoenixd".into()]).unwrap();
        let docker = build_args(
            "afpay:1.2.3",
            Runtime::Docker,
            BuildSource::Embedded {
                ctx: &ctx,
                target: "x86_64-unknown-linux-gnu",
            },
            &extras,
        );
        assert_eq!(docker[0], "build");
        assert!(docker.contains(&"AFPAY_BIN_FROM=downloader".to_string()));
        assert!(docker.contains(&format!("AFPAY_VERSION={VERSION}")));
        assert!(docker.contains(&"AFPAY_TARGET=x86_64-unknown-linux-gnu".to_string()));
        assert!(docker.contains(&"INSTALL_PHOENIXD=true".to_string()));
        assert_eq!(
            docker[docker.len() - 2],
            "/cache/ctx/container/docker/Dockerfile"
        );
        assert_eq!(docker.last().unwrap(), "/cache/ctx");
    }

    #[test]
    fn from_source_build_args_select_builder_with_features() {
        let repo = PathBuf::from("/repo");
        let args = build_args(
            "afpay:1.2.3",
            Runtime::Apple,
            BuildSource::FromSource {
                ctx: &repo,
                features: Some("redb,rest"),
            },
            &[],
        );
        assert!(args.contains(&"AFPAY_BIN_FROM=builder".to_string()));
        assert!(args.contains(&"FEATURES=redb,rest".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("AFPAY_VERSION=")));
        // Apple gets --platform.
        let pos = args.iter().position(|a| a == "--platform").unwrap();
        assert_eq!(args[pos + 1], "linux/arm64");
    }

    #[test]
    fn run_args_mount_three_volumes_and_publish_loopback() {
        use crate::args::ContainerCommonArgs;
        let extras = resolve_extras(&["bitcoind".into()]).unwrap();
        let args = ContainerInstallArgs {
            common: ContainerCommonArgs {
                runtime: None,
                name: "afpay".into(),
            },
            port: 9401,
            with: vec!["bitcoind".into()],
            allow: vec!["mint=https://mint.example".into()],
            btc_network: "mainnet".into(),
            btc_rpc_port: 8332,
            btc_prune_mb: 550,
            features: None,
            rebuild: false,
            from_source: false,
            context: None,
            reveal_daemon_secret: false,
        };
        let allowlists = resolve_allowlists(&args.allow).unwrap();
        let a = run_args("afpay", "afpay:1.2.3", &extras, &allowlists, &args);
        assert!(a.contains(&"afpay-afpay:/data/afpay".to_string()));
        assert!(a.contains(&"AFPAY_ALLOWED_MINT_URLS=https://mint.example".to_string()));
        assert!(a.contains(&"afpay-bitcoind:/data/bitcoind".to_string()));
        assert!(a.contains(&"afpay-phoenixd:/data/phoenixd".to_string()));
        assert!(a.contains(&"AFPAY_PORT=9401".to_string()));
        assert!(a.contains(&"ENABLE_BITCOIND=true".to_string()));
        assert!(a.contains(&"ENABLE_PHOENIXD=false".to_string()));
        assert!(a.contains(&"BTC_NETWORK=mainnet".to_string()));
        assert!(a.contains(&"127.0.0.1:9401:9401".to_string()));
    }

    #[test]
    fn the_client_command_is_a_bearer_curl_against_the_http_face() {
        let command = client_command(9401, "deadbeef");
        assert!(command.contains("curl"));
        assert!(command.contains("http://127.0.0.1:9401/v1/wallets"));
        assert!(command.contains("Bearer deadbeef"));
    }

    #[test]
    fn container_connection_fields_follow_afdata_url_and_secret_contracts() {
        let raw = with_connection_fields(
            json!({"code": "container_status"}),
            9401,
            Some("credential-canary".to_string()),
        );
        assert_eq!(raw["endpoint_url"], "http://127.0.0.1:9401");
        assert!(
            raw["client_command_secret"]
                .as_str()
                .is_some_and(|command| command.contains("credential-canary"))
        );

        let redacted = agent_first_data::redacted_value(&raw);
        assert_eq!(redacted["daemon_secret"], "***");
        assert_eq!(redacted["client_command_secret"], "***");
        assert!(!redacted.to_string().contains("credential-canary"));
        assert_eq!(redacted["endpoint_url"], "http://127.0.0.1:9401");
    }

    #[test]
    fn allowlists_group_by_category_and_reject_bad_entries() {
        let resolved = resolve_allowlists(&[
            "mint=https://m1".into(),
            "mint=https://m2".into(),
            "esplora=https://e1".into(),
        ])
        .unwrap();
        // Two mint urls collapse into one comma-joined env; esplora separate.
        assert_eq!(
            resolved,
            vec![
                (
                    "AFPAY_ALLOWED_MINT_URLS",
                    "https://m1,https://m2".to_string()
                ),
                ("AFPAY_ALLOWED_ESPLORA_URLS", "https://e1".to_string()),
            ]
        );
        // A url with '=' (query string) keeps everything after the first '='.
        let q = resolve_allowlists(&["ln=http://h:9740/?a=b".into()]).unwrap();
        assert_eq!(q[0].1, "http://h:9740/?a=b");
        assert!(resolve_allowlists(&["bogus=x".into()]).is_err());
        assert!(resolve_allowlists(&["noequals".into()]).is_err());
        assert!(resolve_allowlists(&["mint=".into()]).is_err());
    }
}
