use crate::provider::PayError;
use crate::store::PayStore;
use crate::types::*;
use std::time::Instant;

use super::App;
use super::helpers::*;

/// Pick a daemon endpoint to suggest in `ConfigureOnDaemon` hints. Returns the
/// first configured `afpay_rpc` endpoint, since limit-bearing daemons are typically
/// the user's primary RPC backend.
async fn primary_daemon_endpoint(app: &App) -> Option<String> {
    let cfg = app.config.read().await;
    cfg.afpay_rpc.values().next().map(|c| c.endpoint.clone())
}

pub(crate) async fn dispatch_limit(app: &App, input: Input) {
    match input {
        Input::LimitAdd { id, mut limit } => {
            let start = Instant::now();
            if !app.enforce_limits {
                let daemon_endpoint = primary_daemon_endpoint(app).await;
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::ConfigureOnDaemon {
                        operation: "limit_add".to_string(),
                        daemon_endpoint,
                    },
                    start,
                )
                .await;
                return;
            }

            // Auto-fill provider for wallet-scope rules that don't have one
            if limit.scope == SpendScope::Wallet
                && limit.network.is_none()
                && let Some(wallet_id) = limit.wallet.as_deref()
            {
                match require_store(app).and_then(|s| s.load_wallet_metadata(wallet_id)) {
                    Ok(meta) => {
                        limit.network = Some(meta.network.to_string());
                    }
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                }
            }

            match app.spend_ledger.add_limit(&mut limit).await {
                Ok(rule_id) => {
                    let _ = app
                        .writer
                        .send(Output::LimitAdded {
                            id,
                            rule_id,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::LimitRemove { id, rule_id } => {
            let start = Instant::now();
            if !app.enforce_limits {
                let daemon_endpoint = primary_daemon_endpoint(app).await;
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::ConfigureOnDaemon {
                        operation: "limit_remove".to_string(),
                        daemon_endpoint,
                    },
                    start,
                )
                .await;
                return;
            }

            match app.spend_ledger.remove_limit(&rule_id).await {
                Ok(()) => {
                    let _ = app
                        .writer
                        .send(Output::LimitRemoved {
                            id,
                            rule_id,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::LimitList { id } => {
            let start = Instant::now();
            let local_limits = if app.enforce_limits {
                match app.spend_ledger.get_status().await {
                    Ok(status) => status,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                }
            } else {
                vec![]
            };

            // Query downstream afpay_rpc nodes
            let config = app.config.read().await.clone();
            let downstream = query_downstream_limits(&config).await;

            let _ = app
                .writer
                .send(Output::LimitStatus {
                    id,
                    limits: local_limits,
                    downstream,
                    trace: trace_from(start),
                })
                .await;
        }

        Input::LimitSet { id, mut limits } => {
            let start = Instant::now();
            if !app.enforce_limits {
                let daemon_endpoint = primary_daemon_endpoint(app).await;
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::ConfigureOnDaemon {
                        operation: "limit_set".to_string(),
                        daemon_endpoint,
                    },
                    start,
                )
                .await;
                return;
            }

            // Auto-fill provider for wallet-scope rules that don't have one
            for rule in &mut limits {
                if rule.scope == SpendScope::Wallet
                    && rule.network.is_none()
                    && let Some(wallet_id) = rule.wallet.as_deref()
                {
                    match require_store(app).and_then(|s| s.load_wallet_metadata(wallet_id)) {
                        Ok(meta) => {
                            rule.network = Some(meta.network.to_string());
                        }
                        Err(e) => {
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    }
                }
            }

            match app.spend_ledger.set_limits(&limits).await {
                Ok(()) => match app.spend_ledger.get_status().await {
                    Ok(status) => {
                        let _ = app
                            .writer
                            .send(Output::LimitStatus {
                                id,
                                limits: status,
                                downstream: vec![],
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                },
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        _ => {}
    }
}
