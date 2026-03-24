use crate::args::{InteractiveFrontend, InteractiveInit, Mode};
use crate::config::VERSION;
use crate::handler::{self, App};
use crate::provider::remote;
use crate::types::*;
use agent_first_data::OutputFormat;
use std::io::Write as _;
use std::sync::Arc;
use tokio::sync::mpsc;

mod cli;
#[cfg(feature = "interactive")]
mod interactive;
mod pipe;
#[cfg(feature = "rest")]
pub mod rest;
pub mod rpc;
#[cfg(feature = "interactive")]
mod session;
#[cfg(feature = "interactive")]
mod tui;

#[cfg(feature = "interactive")]
use session::{
    banner_hint, mode_name, render_output, CommandCompleter, SessionBackend, SessionState,
    OUTPUT_CHANNEL_CAPACITY,
};

#[cfg(feature = "interactive")]
struct InteractiveSessionRuntime {
    frontend: InteractiveFrontend,
    state: SessionState,
    backend: SessionBackend,
    completer: CommandCompleter,
    history_path: String,
    intro_messages: Vec<String>,
}

pub async fn run(mode: Mode) {
    match mode {
        Mode::Cli(req) => {
            if req.rpc_endpoint.is_some() {
                cli::run_remote(*req).await;
            } else {
                cli::run(*req).await;
            }
        }
        Mode::Pipe(init) => pipe::run(init).await,
        Mode::Interactive(init) => {
            #[cfg(feature = "interactive")]
            {
                run_interactive(init).await;
            }
            #[cfg(not(feature = "interactive"))]
            {
                cli::emit_cli_error(
                    "interactive and tui modes require feature 'interactive'; rebuild with: cargo build --features interactive",
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

#[cfg(feature = "interactive")]
async fn run_interactive(init: InteractiveInit) {
    let InteractiveInit {
        frontend,
        output,
        log,
        data_dir,
        rpc_endpoint,
        rpc_secret,
    } = init;

    let runtime = if let Some(endpoint) = rpc_endpoint {
        bootstrap_remote_session(
            frontend,
            output,
            &log,
            data_dir.as_deref(),
            &endpoint,
            rpc_secret.as_deref(),
        )
        .await
    } else {
        bootstrap_local_session(frontend, output, &log, data_dir).await
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
    let log_filters = agent_first_data::cli_parse_log_filters(log);
    config.log = log_filters.clone();

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
        frontend,
        state,
        backend: SessionBackend::Local { app, rx },
        completer,
        history_path: format!("{data_dir_owned}/.afpay_history"),
        intro_messages,
    })
}

#[cfg(feature = "interactive")]
async fn bootstrap_remote_session(
    frontend: InteractiveFrontend,
    output: OutputFormat,
    log: &[String],
    data_dir: Option<&str>,
    endpoint: &str,
    rpc_secret: Option<&str>,
) -> Option<InteractiveSessionRuntime> {
    let (endpoint, secret) = remote::require_remote_args(Some(endpoint), rpc_secret, output);
    let log_filters = agent_first_data::cli_parse_log_filters(log);
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
    local_config.log = log_filters.clone();

    let mut intro_messages = Vec::new();
    if let Some(startup) = crate::config::maybe_startup_log(
        &log_filters,
        false,
        None,
        Some(&local_config),
        serde_json::json!({
            "mode": mode_name(frontend),
            "backend": "remote",
            "rpc_endpoint": endpoint,
            "data_dir": local_config.data_dir,
        }),
    ) {
        intro_messages.push(render_output(&startup, output));
    }

    let ping_outputs = remote::rpc_call(endpoint, secret, &Input::Version).await;
    for value in &ping_outputs {
        if value.get("code").and_then(|v| v.as_str()) == Some("error") {
            let error = Output::Error {
                id: None,
                error_code: "provider_unreachable".to_string(),
                error: format!(
                    "remote version check failed: {}",
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
                    error_code: "version_mismatch".to_string(),
                    error: format!("version mismatch: local v{VERSION}, remote v{remote_version}"),
                    hint: Some("upgrade both client and server to the same version".to_string()),
                    retryable: false,
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
        "afpay v{VERSION} {} mode (remote: {endpoint})",
        mode_name(frontend)
    ));
    intro_messages.push(banner_hint(frontend).to_string());

    Some(InteractiveSessionRuntime {
        frontend,
        state,
        backend: SessionBackend::Remote {
            endpoint: endpoint.to_string(),
            secret: secret.to_string(),
        },
        completer,
        history_path: format!("{}/.afpay_history", local_config.data_dir),
        intro_messages,
    })
}
