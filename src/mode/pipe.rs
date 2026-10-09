use super::cli::{emit_cli_error, emit_output, request_id_for_tracking};
use crate::args::PipeInit;
use crate::config;
use crate::handler::{self, App};
use crate::store;
use crate::types::*;
use crate::writer;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use tokio::time::Instant;

const OUTPUT_CHANNEL_CAPACITY: usize = 4096;
/// Cap on concurrently in-flight async dispatches. Above this, the pipe
/// returns `error_code: "busy"` with a `retry_after_ms` hint so a runaway
/// agent cannot exhaust task slots and starve shutdown drain.
const PIPE_IN_FLIGHT_CAP: usize = 32;

#[derive(Default)]
struct DispatchScope {
    tasks: Vec<tokio::task::AbortHandle>,
}

impl DispatchScope {
    fn track(&mut self, task: &tokio::task::JoinHandle<()>) {
        self.tasks.retain(|task| !task.is_finished());
        self.tasks.push(task.abort_handle());
    }
}

impl Drop for DispatchScope {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

struct PendingDispatches {
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl PendingDispatches {
    async fn shutdown(mut self, deadline: Instant) {
        for handle in &mut self.handles {
            if tokio::time::timeout_at(deadline, &mut *handle)
                .await
                .is_err()
            {
                handle.abort();
                let _joined = handle.await;
            }
        }
    }
}

impl Drop for PendingDispatches {
    fn drop(&mut self) {
        // The owner can also cancel shutdown while it is draining a dispatch.
        for handle in &self.handles {
            handle.abort();
        }
    }
}

pub(super) async fn run(init: PipeInit) {
    let PipeInit {
        output,
        log,
        data_dir,
        startup_argv,
        startup_args,
        startup_requested,
        scrub_parse_errors,
    } = init;

    let resolved_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(config) => config,
        Err(error) => {
            emit_cli_error("request_invalid", &error, output);
            std::process::exit(1);
        }
    };
    if !log.is_empty() {
        config.log = log.clone();
    }

    let log_filters = agent_first_data::LogFilters::new(config.log.clone());
    if let Some(event) = config::maybe_startup_log(
        &log_filters,
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
    let mut dispatch_scope = DispatchScope::default();

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = reader.lines();
    let mut task_seq: u64 = 0;

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                // In public-listen mode keep the byte offset (useful for an
                // operator inspecting the wire) but strip the field name /
                // variant detail so a probing attacker cannot enumerate the
                // schema. Without --public-listen the full serde message is
                // surfaced, matching prior dev-friendly behaviour.
                let wire_message = if scrub_parse_errors {
                    format!(
                        "parse error at line {} column {}",
                        error.line(),
                        error.column()
                    )
                } else {
                    format!("parse error: {error}")
                };
                let _ = app
                    .writer
                    .send(Output::Error {
                        id: None,
                        error_code: "invalid_request".to_string(),
                        error: wire_message,
                        hint: Some(
                            "request must be valid JSON matching the Input/Request schema; pass `dry_run: true` to validate without executing"
                                .to_string(),
                        ),
                        retryable: false,
                        retry_after_ms: None,
                        trace: Trace::from_duration(0),
                    })
                    .await;
                continue;
            }
        };

        let is_close = matches!(request.input, Input::Close);

        match &request.input {
            Input::Version | Input::ConfigGet { .. } | Input::ConfigSet { .. } | Input::Close => {
                app.requests_total.fetch_add(1, Ordering::Relaxed);
                handler::dispatch(&app, request).await;
            }
            _ => {
                // Reap finished tasks BEFORE checking the cap so steady-state
                // pipelining is not gated on the periodic retain() at the
                // bottom of the loop.
                {
                    let mut in_flight = app.in_flight.lock().await;
                    in_flight.retain(|_, handle| !handle.is_finished());
                    if in_flight.len() >= PIPE_IN_FLIGHT_CAP {
                        drop(in_flight);
                        let req_id = request_id_for_tracking(&request.input).map(str::to_string);
                        let _ = app
                            .writer
                            .send(Output::Error {
                                id: req_id,
                                error_code: "busy".to_string(),
                                error: format!(
                                    "pipe in-flight cap reached ({PIPE_IN_FLIGHT_CAP}); back off and retry"
                                ),
                                hint: Some(format!(
                                    "the daemon caps concurrent async dispatches at {PIPE_IN_FLIGHT_CAP} to keep shutdown drain bounded; resubmit after prior requests complete"
                                )),
                                retryable: true,
                                // 100ms is the empirical mean dispatch time
                                // for cached cashu/sol sends; longer than that
                                // and the cap was the real bottleneck, not
                                // backpressure noise. Tune if dispatches grow.
                                retry_after_ms: Some(100),
                                trace: Trace::from_duration(0),
                            })
                            .await;
                        continue;
                    }
                }
                let app2 = app.clone();
                app.requests_total.fetch_add(1, Ordering::Relaxed);
                let request_id = request_id_for_tracking(&request.input).unwrap_or("anonymous");
                let key = format!("{request_id}#{task_seq}");
                task_seq = task_seq.wrapping_add(1);
                let handle = tokio::spawn(async move {
                    handler::dispatch(&app2, request).await;
                });
                // Register before awaiting the map lock: cancellation during
                // intake must not detach a task that already owns resources.
                dispatch_scope.track(&handle);
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
    PendingDispatches { handles }.shutdown(deadline).await;

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn stalled_dispatch() -> (tokio::task::JoinHandle<()>, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind dispatch fixture");
        let address = listener.local_addr().expect("dispatch fixture address");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let stream = tokio::net::TcpStream::connect(address)
                .await
                .expect("connect dispatch transport");
            started_tx.send(()).expect("dispatch started");
            std::future::pending::<()>().await;
            drop(stream);
        });
        let (peer, _) = listener.accept().await.expect("accept dispatch transport");
        started_rx.await.expect("dispatch holds the transport");
        (handle, peer)
    }

    async fn assert_transport_closed(mut peer: tokio::net::TcpStream) {
        let mut bytes = [0; 1];
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), peer.read(&mut bytes))
                .await
                .expect("shutdown releases the dispatch socket")
                .expect("peer read"),
            0
        );
    }

    #[tokio::test]
    async fn shutdown_deadline_aborts_dispatches_and_closes_their_transports() {
        let (handle, peer) = stalled_dispatch().await;
        PendingDispatches {
            handles: vec![handle],
        }
        .shutdown(Instant::now())
        .await;
        assert_transport_closed(peer).await;
    }

    #[tokio::test]
    async fn cancelling_shutdown_aborts_dispatches_and_closes_their_transports() {
        let (handle, peer) = stalled_dispatch().await;
        let pending = PendingDispatches {
            handles: vec![handle],
        };
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                pending.shutdown(Instant::now() + std::time::Duration::from_secs(5)),
            )
            .await
            .is_err()
        );
        assert_transport_closed(peer).await;
    }

    #[tokio::test]
    async fn cancelling_intake_aborts_dispatches_and_closes_their_transports() {
        let (handle, peer) = stalled_dispatch().await;
        let intake = async move {
            let mut scope = DispatchScope::default();
            scope.track(&handle);
            drop(handle);
            std::future::pending::<()>().await;
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), intake)
                .await
                .is_err()
        );
        assert_transport_closed(peer).await;
    }
}
