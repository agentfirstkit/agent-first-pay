use super::cli::{emit_cli_error, emit_output, request_id_for_tracking};
use crate::args::PipeInit;
use crate::config;
use crate::handler::{self, App};
use crate::store;
use crate::types::*;
use crate::writer;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

const OUTPUT_CHANNEL_CAPACITY: usize = 4096;

pub(super) async fn run(init: PipeInit) {
    let PipeInit {
        output,
        log,
        data_dir,
        startup_argv,
        startup_args,
        startup_requested,
    } = init;

    let resolved_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(config) => config,
        Err(error) => {
            emit_cli_error(&error, output);
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
            Ok(value) => value,
            Err(error) => {
                let _ = app
                    .writer
                    .send(Output::Error {
                        id: None,
                        error_code: "invalid_request".to_string(),
                        error: format!("parse error: {error}"),
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
            Input::Version | Input::Config(_) | Input::ConfigShow { .. } | Input::Close => {
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

        app.in_flight
            .lock()
            .await
            .retain(|_, handle| !handle.is_finished());
    }

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
