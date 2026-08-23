use crate::args::Mode;
use agent_first_data::OutputFormat;

#[cfg(feature = "interactive")]
use crate::args::{InteractiveFrontend, InteractiveInit};
#[cfg(feature = "interactive")]
use crate::config::VERSION;
#[cfg(feature = "interactive")]
use crate::handler::{self, App};
#[cfg(all(feature = "interactive", feature = "federation"))]
use crate::provider::remote;
#[cfg(feature = "interactive")]
use crate::types::*;
#[cfg(feature = "interactive")]
use std::io::Write as _;
#[cfg(feature = "interactive")]
use std::sync::Arc;
#[cfg(feature = "interactive")]
use tokio::sync::mpsc;

mod cli;
#[cfg(feature = "backup")]
mod data;
#[cfg(feature = "interactive")]
mod interactive;
mod pipe;
#[cfg(any(feature = "interactive", feature = "ui"))]
mod qr;
#[cfg(feature = "rest")]
pub mod rest;
#[cfg(feature = "interactive")]
mod session;
#[cfg(feature = "interactive")]
mod tui;
#[cfg(feature = "ui")]
mod ui;

#[cfg(feature = "interactive")]
use session::{
    CommandCompleter, OUTPUT_CHANNEL_CAPACITY, SessionBackend, SessionState, banner_hint,
    mode_name, render_output,
};

/// What both interactive frontends are handed.
///
/// Not the frontend itself: `run` picks the entry point by that value, so a
/// copy in here would only let one of them ask a question it has already been
/// answered by being called.
#[cfg(feature = "interactive")]
struct InteractiveSessionRuntime {
    state: SessionState,
    backend: SessionBackend,
    completer: CommandCompleter,
    history_path: String,
    intro_messages: Vec<String>,
}

pub async fn run(mode: Mode) {
    match mode {
        Mode::Cli(req) => {
            if req.peer_url.is_some() {
                #[cfg(feature = "federation")]
                {
                    cli::run_remote(*req).await;
                }
                #[cfg(not(feature = "federation"))]
                {
                    cli::emit_cli_error(
                        "feature_unavailable",
                        "--peer-url requires feature 'federation'; rebuild with: cargo build --features federation",
                        req.output,
                    );
                    std::process::exit(1);
                }
            } else {
                cli::run(*req).await;
            }
        }
        Mode::Pipe(init) => pipe::run(init).await,
        Mode::Interactive(_init) => {
            #[cfg(feature = "interactive")]
            {
                run_interactive(_init).await;
            }
            #[cfg(not(feature = "interactive"))]
            {
                cli::emit_cli_error(
                    "feature_unavailable",
                    "interactive and tui modes require feature 'interactive'; rebuild with: cargo build --features interactive",
                    OutputFormat::Json,
                );
                std::process::exit(1);
            }
        }
        #[cfg(feature = "rest")]
        Mode::Rest(init) => rest::run_rest(init).await,
        #[cfg(feature = "rest")]
        Mode::ApiExport(request) => std::process::exit(run_api_export(request)),
        Mode::Ui(_init) => {
            #[cfg(feature = "ui")]
            {
                ui::run(*_init).await;
            }
            #[cfg(not(feature = "ui"))]
            {
                cli::emit_cli_error(
                    "feature_unavailable",
                    "ui panels require feature 'ui'; rebuild with: cargo build --features ui",
                    _init.output,
                );
                std::process::exit(1);
            }
        }
        Mode::SkillAdmin(req) => std::process::exit(crate::skill_admin::run(req)),
        Mode::Container(req) => std::process::exit(crate::container::run(req)),
        Mode::Data(_op) => {
            #[cfg(feature = "backup")]
            {
                data::run_data(_op).await;
            }
            #[cfg(not(feature = "backup"))]
            {
                cli::emit_cli_error(
                    "feature_unavailable",
                    "backup/restore requires feature 'backup'; rebuild with: cargo build --features backup",
                    OutputFormat::Json,
                );
                std::process::exit(1);
            }
        }
    }
}

#[cfg(feature = "rest")]
fn run_api_export(request: crate::args::ApiExportRequest) -> i32 {
    match crate::api::export_contract(std::path::Path::new(&request.directory), request.force) {
        Ok(result) => {
            let event = agent_first_data::json_result(result)
                .trace(serde_json::json!({"duration_ms": 0}))
                .build();
            if crate::output_fmt::emit_process_event(event.into(), request.output).is_err() {
                return 4;
            }
            0
        }
        Err(error) => {
            let event =
                crate::output_fmt::coded_error_event(error.code(), &error.message(), error.hint());
            if crate::output_fmt::emit_process_event(event, request.output).is_err() {
                return 4;
            }
            1
        }
    }
}

#[cfg(feature = "interactive")]
async fn run_interactive(init: InteractiveInit) {
    let InteractiveInit {
        frontend,
        output,
        log,
        data_dir,
        peer_url,
        peer_api_key_secret,
    } = init;

    let runtime = if let Some(peer_url) = peer_url {
        #[cfg(feature = "federation")]
        {
            bootstrap_remote_session(
                frontend,
                output,
                log.as_slice(),
                data_dir.as_deref(),
                &peer_url,
                peer_api_key_secret.as_deref(),
            )
            .await
        }
        #[cfg(not(feature = "federation"))]
        {
            let _ = (peer_url, peer_api_key_secret);
            cli::emit_cli_error(
                "feature_unavailable",
                "--peer-url requires feature 'federation'; rebuild with: cargo build --features federation",
                output,
            );
            return;
        }
    } else {
        bootstrap_local_session(frontend, output, log.as_slice(), data_dir).await
    };

    let Some(runtime) = runtime else {
        return;
    };

    match frontend {
        InteractiveFrontend::Interactive => interactive::run_interactive_ui(runtime).await,
        InteractiveFrontend::Tui => tui::run_tui_ui(runtime).await,
    }
}

#[cfg(feature = "interactive")]
async fn bootstrap_local_session(
    frontend: InteractiveFrontend,
    output: OutputFormat,
    log: &[String],
    data_dir: Option<String>,
) -> Option<InteractiveSessionRuntime> {
    let resolved_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(config) => config,
        Err(error) => {
            let _ = writeln!(std::io::stdout(), "config error: {error}");
            return None;
        }
    };

    let data_dir_owned = config.data_dir.clone();
    config.log = log.to_vec();

    let log_filters = agent_first_data::LogFilters::new(log.to_vec());
    let mut intro_messages = Vec::new();
    if let Some(startup) = crate::config::maybe_startup_log(
        &log_filters,
        false,
        None,
        Some(&config),
        serde_json::json!({
            "mode": mode_name(frontend),
            "backend": "local",
            "data_dir": config.data_dir,
        }),
    ) {
        intro_messages.push(render_output(&startup, output));
    }

    let startup_errors = handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        intro_messages.push(render_output(error_output, output));
    }
    if !startup_errors.is_empty() {
        for message in intro_messages {
            let _ = writeln!(std::io::stdout(), "{message}");
        }
        return None;
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let store = crate::store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, None, store));
    let store_ref = app.store.clone();
    let state = SessionState::new(
        data_dir_owned.clone(),
        output,
        log_filters,
        store_ref.clone(),
    );
    let completer = CommandCompleter::new(data_dir_owned.clone(), store_ref);

    intro_messages.push(format!("afpay v{VERSION} {} mode", mode_name(frontend)));
    intro_messages.push(banner_hint(frontend).to_string());

    Some(InteractiveSessionRuntime {
        state,
        backend: SessionBackend::Local { app, rx },
        completer,
        history_path: format!("{data_dir_owned}/.afpay_history"),
        intro_messages,
    })
}

#[cfg(all(feature = "interactive", feature = "federation"))]
async fn bootstrap_remote_session(
    frontend: InteractiveFrontend,
    output: OutputFormat,
    log: &[String],
    data_dir: Option<&str>,
    peer_url: &str,
    peer_api_key_secret: Option<&str>,
) -> Option<InteractiveSessionRuntime> {
    let (peer_url, api_key_secret) =
        remote::require_peer_args(Some(peer_url), peer_api_key_secret, output);
    let resolved_dir = data_dir
        .map(ToString::to_string)
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut local_config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(config) => config,
        Err(error) => {
            let _ = writeln!(std::io::stdout(), "config error: {error}");
            return None;
        }
    };
    local_config.log = log.to_vec();

    let log_filters = agent_first_data::LogFilters::new(log.to_vec());
    let mut intro_messages = Vec::new();
    if let Some(startup) = crate::config::maybe_startup_log(
        &log_filters,
        false,
        None,
        Some(&local_config),
        serde_json::json!({
            "mode": mode_name(frontend),
            "backend": "peer",
            "peer_url": peer_url,
            "data_dir": local_config.data_dir,
        }),
    ) {
        intro_messages.push(render_output(&startup, output));
    }

    let ping_outputs = remote::peer_call(peer_url, api_key_secret, &Input::Version).await;
    for value in &ping_outputs {
        if value.get("code").and_then(|v| v.as_str()) == Some("error") {
            let error = Output::Error {
                id: None,
                error_code: "provider_unreachable".to_string(),
                error: format!(
                    "peer identity check failed: {}",
                    value
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                ),
                hint: value
                    .get("hint")
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string()),
                retryable: true,
                retry_after_ms: None,
                trace: Trace::from_duration(0),
            };
            let _ = writeln!(std::io::stdout(), "{}", render_output(&error, output));
            return None;
        }
        if value.get("code").and_then(|v| v.as_str()) == Some("version") {
            let remote_version = value
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if remote_version != VERSION {
                let error = Output::Error {
                    id: None,
                    error_code: "peer_mismatch".to_string(),
                    error: format!(
                        "afpay version mismatch: this node is v{VERSION}, the peer at {peer_url} is v{remote_version}"
                    ),
                    hint: Some("run the same afpay version on both nodes".to_string()),
                    retryable: false,
                    retry_after_ms: None,
                    trace: Trace::from_duration(0),
                };
                let _ = writeln!(std::io::stdout(), "{}", render_output(&error, output));
                return None;
            }
        }
    }

    let store_ref = crate::store::create_storage_backend(&local_config).map(Arc::new);
    let state = SessionState::new(
        local_config.data_dir.clone(),
        output,
        log_filters,
        store_ref.clone(),
    );
    let completer = CommandCompleter::new(local_config.data_dir.clone(), store_ref);

    intro_messages.push(format!(
        "afpay v{VERSION} {} mode (peer: {peer_url})",
        mode_name(frontend)
    ));
    intro_messages.push(banner_hint(frontend).to_string());

    Some(InteractiveSessionRuntime {
        state,
        backend: SessionBackend::Remote {
            peer_url: peer_url.to_string(),
            api_key_secret: api_key_secret.to_string(),
        },
        completer,
        history_path: format!("{}/.afpay_history", local_config.data_dir),
        intro_messages,
    })
}
