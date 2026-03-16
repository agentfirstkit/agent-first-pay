#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
    )
)]

mod cli;
mod config;
mod handler;
#[cfg(feature = "interactive")]
mod interactive;
mod output_fmt;
mod provider;
#[cfg(feature = "rest")]
pub mod rest;
pub mod rpc;
mod spend;
mod store;
mod types;
mod writer;

use agent_first_data::OutputFormat;
use cli::Mode;
use handler::App;
use provider::remote;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use types::*;

const OUTPUT_CHANNEL_CAPACITY: usize = 4096;

#[tokio::main]
async fn main() {
    let mode = match cli::parse_args() {
        Ok(m) => m,
        Err(e) => {
            emit_cli_error_hint(&e.message, e.hint.as_deref(), OutputFormat::Json);
            std::process::exit(2);
        }
    };

    // Remote modes: --rpc-endpoint means client-only, skip data lock.
    let is_remote = match &mode {
        Mode::Cli(req) => req.rpc_endpoint.is_some(),
        Mode::Interactive(init) => init.rpc_endpoint.is_some(),
        _ => false,
    };
    if is_remote {
        match mode {
            Mode::Cli(req) => {
                run_cli_remote(*req).await;
            }
            Mode::Interactive(_init) => {
                #[cfg(feature = "interactive")]
                {
                    interactive::run_interactive(_init).await;
                }
                #[cfg(not(feature = "interactive"))]
                {
                    emit_cli_error_hint(
                        "interactive mode requires feature 'interactive'",
                        Some("rebuild with: cargo build --features interactive"),
                        OutputFormat::Json,
                    );
                    std::process::exit(1);
                }
            }
            _ => {}
        }
        return;
    }

    match mode {
        Mode::Cli(req) => run_cli(*req).await,
        Mode::Pipe(init) => run_pipe(init).await,
        Mode::Interactive(_init) => {
            #[cfg(feature = "interactive")]
            {
                interactive::run_interactive(_init).await;
            }
            #[cfg(not(feature = "interactive"))]
            {
                emit_cli_error(
                    "interactive mode requires feature 'interactive'; rebuild with: cargo build --features interactive",
                    OutputFormat::Json,
                );
                std::process::exit(1);
            }
        }
        Mode::Rpc(init) => rpc::run_rpc(init).await,
        #[cfg(feature = "rest")]
        Mode::Rest(init) => rest::run_rest(init).await,
    }
}

async fn run_cli(req: cli::CliRequest) {
    let cli::CliRequest {
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
        Ok(c) => c,
        Err(e) => {
            emit_cli_error(&e, output_format);
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

async fn run_cli_remote(req: cli::CliRequest) {
    // Load config so startup log shows rpc_endpoint/rpc_secret from config.toml
    let resolved_dir = req
        .data_dir
        .as_deref()
        .map(|s| s.to_string())
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

async fn run_pipe(init: cli::PipeInit) {
    let cli::PipeInit {
        output,
        log,
        data_dir,
        startup_argv,
        startup_args,
        startup_requested,
    } = init;

    let resolved_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(c) => c,
        Err(e) => {
            emit_cli_error(&e, output);
            std::process::exit(1);
        }
    };
    if !log.is_empty() {
        config.log = log.clone();
    }

    if let Some(event) = config::maybe_startup_log(
        &config.log,
        startup_requested,
        Some(startup_argv),
        Some(&config),
        startup_args,
    ) {
        emit_output(&event, output);
    }

    let startup_errors = handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        emit_output(error_output, output);
    }
    if !startup_errors.is_empty() {
        std::process::exit(1);
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    tokio::spawn(writer::writer_task(rx, output));

    let store = store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, None, store));

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = reader.lines();
    let mut task_seq: u64 = 0;

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let input: Input = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let _ = app
                    .writer
                    .send(Output::Error {
                        id: None,
                        error_code: "invalid_request".to_string(),
                        error: format!("parse error: {e}"),
                        hint: None,
                        retryable: false,
                        trace: Trace::from_duration(0),
                    })
                    .await;
                continue;
            }
        };

        let is_close = matches!(input, Input::Close);

        match &input {
            Input::Version | Input::Config(_) | Input::Close => {
                app.requests_total.fetch_add(1, Ordering::Relaxed);
                handler::dispatch(&app, input).await;
            }
            _ => {
                let app2 = app.clone();
                app.requests_total.fetch_add(1, Ordering::Relaxed);
                let request_id = request_id_for_tracking(&input).unwrap_or("anonymous");
                let key = format!("{request_id}#{task_seq}");
                task_seq = task_seq.wrapping_add(1);
                let handle = tokio::spawn(async move {
                    handler::dispatch(&app2, input).await;
                });
                app.in_flight.lock().await.insert(key, handle);
            }
        }

        if is_close {
            break;
        }

        app.in_flight.lock().await.retain(|_, h| !h.is_finished());
    }

    // Drain in-flight tasks
    let handles: Vec<tokio::task::JoinHandle<()>> =
        app.in_flight.lock().await.drain().map(|(_, h)| h).collect();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    for handle in handles {
        let now = Instant::now();
        let remain = deadline.saturating_duration_since(now);
        if tokio::time::timeout(remain, handle).await.is_err() {
            // timeout waiting this task; move on
        }
    }

    let _ = app
        .writer
        .send(Output::Close {
            message: "shutdown".to_string(),
            trace: CloseTrace {
                uptime_s: app.start_time.elapsed().as_secs(),
                requests_total: app.requests_total.load(Ordering::Relaxed),
            },
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

fn emit_cli_error(msg: &str, format: OutputFormat) {
    emit_cli_error_hint(msg, None, format);
}

fn emit_cli_error_hint(msg: &str, hint: Option<&str>, format: OutputFormat) {
    let value = agent_first_data::build_cli_error(msg, hint);
    let rendered = agent_first_data::cli_output(&value, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

fn log_event_enabled(log: &[String], event: &str) -> bool {
    if log.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    log.iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}

fn emit_output(out: &Output, format: OutputFormat) {
    let value = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
    let rendered = output_fmt::render_value_with_policy(&value, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

fn request_id_for_tracking(input: &Input) -> Option<&str> {
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
        Input::Config(_) | Input::Version | Input::Close => None,
    }
}
