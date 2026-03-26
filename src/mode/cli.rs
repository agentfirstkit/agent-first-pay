use crate::args::CliRequest;
use crate::config;
use crate::handler::{self, App};
use crate::output_fmt;
#[cfg(feature = "rpc")]
use crate::provider::remote;
use crate::store;
use crate::types::*;
use agent_first_data::OutputFormat;
use std::io::Write as _;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;

const OUTPUT_CHANNEL_CAPACITY: usize = 4096;

pub(super) async fn run(req: CliRequest) {
    let CliRequest {
        input,
        output: output_format,
        log,
        data_dir,
        rpc_endpoint: _,
        rpc_secret: _,
        startup_argv,
        startup_args,
        startup_requested,
        dry_run,
    } = req;

    if dry_run {
        let params = serde_json::to_value(&input).unwrap_or(serde_json::Value::Null);
        let command = params
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let dry = Output::DryRun {
            id: request_id_for_tracking(&input).map(str::to_string),
            command,
            params,
            trace: Trace::from_duration(0),
        };
        emit_output(&dry, output_format);
        return;
    }

    let resolved_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(config) => config,
        Err(error) => {
            emit_cli_error(&error, output_format);
            std::process::exit(1);
        }
    };
    if !log.is_empty() {
        config.log = log.clone();
    }

    let (tx, mut rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let store = store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, None, store));

    let cfg = app.config.read().await;
    if let Some(event) = config::maybe_startup_log(
        &log,
        startup_requested,
        Some(startup_argv),
        Some(&*cfg),
        startup_args,
    ) {
        emit_output(&event, output_format);
    }
    drop(cfg);

    app.requests_total.fetch_add(1, Ordering::Relaxed);
    handler::dispatch(&app, input).await;

    drop(app);

    let mut had_error = false;
    while let Some(out) = rx.recv().await {
        if matches!(out, Output::Error { .. }) {
            had_error = true;
        }
        if let Output::Log { ref event, .. } = out {
            if !log_event_enabled(&log, event) {
                continue;
            }
        }
        emit_output(&out, output_format);
    }

    std::process::exit(if had_error { 1 } else { 0 });
}

#[cfg(feature = "rpc")]
pub(super) async fn run_remote(req: CliRequest) {
    let resolved_dir = req
        .data_dir
        .as_deref()
        .map(ToString::to_string)
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let config = RuntimeConfig::load_from_dir(&resolved_dir).ok();

    if let Some(event) = config::maybe_startup_log(
        &req.log,
        req.startup_requested,
        Some(req.startup_argv.clone()),
        config.as_ref(),
        req.startup_args.clone(),
    ) {
        emit_output(&event, req.output);
    }

    let (endpoint, secret) = remote::require_remote_args(
        req.rpc_endpoint.as_deref(),
        req.rpc_secret.as_deref(),
        req.output,
    );

    let mut outputs = remote::rpc_call(endpoint, secret, &req.input).await;
    remote::wrap_remote_limit_topology(&mut outputs, endpoint);
    let had_error = remote::emit_remote_outputs(&outputs, req.output, &req.log);
    std::process::exit(if had_error { 1 } else { 0 });
}

pub(super) fn emit_cli_error(msg: &str, format: OutputFormat) {
    emit_cli_error_hint(msg, None, format);
}

pub(super) fn emit_cli_error_hint(msg: &str, hint: Option<&str>, format: OutputFormat) {
    let value = agent_first_data::build_cli_error(msg, hint);
    let rendered = agent_first_data::cli_output(&value, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

pub(super) fn log_event_enabled(log: &[String], event: &str) -> bool {
    if log.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    log.iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}

pub(super) fn emit_output(out: &Output, format: OutputFormat) {
    let value = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
    let rendered = output_fmt::render_value_with_policy(&value, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

pub(super) fn request_id_for_tracking(input: &Input) -> Option<&str> {
    match input {
        Input::WalletCreate { id, .. }
        | Input::LnWalletCreate { id, .. }
        | Input::WalletClose { id, .. }
        | Input::WalletList { id, .. }
        | Input::Balance { id, .. }
        | Input::Receive { id, .. }
        | Input::ReceiveClaim { id, .. }
        | Input::CashuSend { id, .. }
        | Input::CashuReceive { id, .. }
        | Input::Send { id, .. }
        | Input::Restore { id, .. }
        | Input::WalletShowSeed { id, .. }
        | Input::HistoryList { id, .. }
        | Input::HistoryStatus { id, .. }
        | Input::HistoryUpdate { id, .. }
        | Input::LimitAdd { id, .. }
        | Input::LimitRemove { id, .. }
        | Input::LimitList { id, .. }
        | Input::LimitSet { id, .. }
        | Input::WalletConfigShow { id, .. }
        | Input::WalletConfigSet { id, .. }
        | Input::WalletConfigTokenAdd { id, .. }
        | Input::WalletConfigTokenRemove { id, .. } => Some(id.as_str()),
        Input::Config(_) | Input::ConfigShow { .. } | Input::Version | Input::Close => None,
    }
}
