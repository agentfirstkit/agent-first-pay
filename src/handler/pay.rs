use crate::provider::PayError;
use crate::spend::{
    IDEMPOTENCY_KEY_MAX_LEN, IdempotencyLookup, IdempotentReplayPayload, SpendContext,
};
use crate::store::PayStore;
use crate::types::*;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use super::App;
use super::helpers::*;
use super::receive_watch::{
    ReceiveWaitOptions, ReceiveWatchRequest, supports_onchain_receive_wait, wait_onchain_receive,
};
use super::spend_guard::{emit_accounting_inconsistent, with_spend_reserve, with_spend_reserves};

/// Outcome of [`enter_idempotent`]. When `Proceed`, the handler continues
/// with the real send flow and is responsible for calling
/// [`finalize_idempotent`] / [`clear_idempotent`] on terminal output. The
/// other variants mean the handler has ALREADY emitted an Output and must
/// return immediately.
enum IdempotencyEntry {
    /// No key was supplied, or the key was claimed fresh; continue normally.
    Proceed {
        /// Present only when the agent supplied a key — the caller threads
        /// it back through finalize/clear so the same (key, hash) pair is
        /// reused (preventing spoof or drift).
        ctx: Option<(String, String)>,
    },
    /// Replay completed: the prior payload was re-emitted as a fresh Output.
    Done,
}

async fn enter_idempotent_send(
    app: &App,
    id: &str,
    key: Option<&str>,
    hash: Option<&str>,
    start: Instant,
) -> IdempotencyEntry {
    let Some(key) = key else {
        return IdempotencyEntry::Proceed { ctx: None };
    };
    let hash = match hash {
        Some(h) => h.to_string(),
        None => {
            emit_error(
                &app.writer,
                Some(id.to_string()),
                &PayError::internal_error(
                    "idempotency_key set but canonical_send_hash refused this input".to_string(),
                ),
                start,
            )
            .await;
            return IdempotencyEntry::Done;
        }
    };
    if key.len() > IDEMPOTENCY_KEY_MAX_LEN {
        emit_error(
            &app.writer,
            Some(id.to_string()),
            &PayError::invalid_amount(format!(
                "idempotency_key length {} exceeds max {IDEMPOTENCY_KEY_MAX_LEN}",
                key.len()
            )),
            start,
        )
        .await;
        return IdempotencyEntry::Done;
    }

    match app.spend_ledger.idempotency_claim(key, &hash).await {
        Ok(IdempotencyLookup::Fresh) => IdempotencyEntry::Proceed {
            ctx: Some((key.to_string(), hash)),
        },
        Ok(IdempotencyLookup::Replay(payload)) => {
            emit_replay(app, id, payload, start).await;
            IdempotencyEntry::Done
        }
        Ok(IdempotencyLookup::InProgress) => {
            let _ = app
                .writer
                .send(Output::Error {
                    id: Some(id.to_string()),
                    error_code: "idempotency_in_progress".to_string(),
                    error: format!(
                        "another request with idempotency_key='{key}' is still in flight"
                    ),
                    hint: Some(
                        "retry after the suggested delay; on completion the original response will replay".to_string(),
                    ),
                    retryable: true,
                    retry_after_ms: Some(250),
                    trace: trace_from(start),
                })
                .await;
            IdempotencyEntry::Done
        }
        Ok(IdempotencyLookup::Conflict) => {
            let _ = app
                .writer
                .send(Output::Error {
                    id: Some(id.to_string()),
                    error_code: "idempotency_conflict".to_string(),
                    error: format!(
                        "idempotency_key='{key}' was already used with a different request body"
                    ),
                    hint: Some(
                        "to reuse the key the body must be byte-identical; otherwise pick a new idempotency_key".to_string(),
                    ),
                    retryable: false,
                    retry_after_ms: None,
                    trace: trace_from(start),
                })
                .await;
            IdempotencyEntry::Done
        }
        Err(e) => {
            emit_error(&app.writer, Some(id.to_string()), &e, start).await;
            IdempotencyEntry::Done
        }
    }
}

async fn emit_replay(app: &App, id: &str, payload: IdempotentReplayPayload, start: Instant) {
    match payload {
        IdempotentReplayPayload::Sent {
            wallet,
            transaction_id,
            amount,
            fee,
            preimage,
            reservation_ids,
        } => {
            let _ = app
                .writer
                .send(Output::Sent {
                    id: id.to_string(),
                    wallet,
                    transaction_id,
                    amount,
                    fee,
                    preimage,
                    reservation_ids,
                    trace: trace_from(start),
                })
                .await;
        }
        IdempotentReplayPayload::CashuSent {
            wallet,
            transaction_id,
            status,
            fee,
            token,
            reservation_ids,
        } => {
            let _ = app
                .writer
                .send(Output::CashuSent {
                    id: id.to_string(),
                    wallet,
                    transaction_id,
                    status,
                    fee,
                    token,
                    reservation_ids,
                    trace: trace_from(start),
                })
                .await;
        }
        IdempotentReplayPayload::AccountingInconsistent {
            transaction_id,
            reservation_ids,
            confirm_errors,
            hint,
        } => {
            let _ = app
                .writer
                .send(Output::AccountingInconsistent {
                    id: id.to_string(),
                    transaction_id,
                    reservation_ids,
                    confirm_errors,
                    hint,
                    trace: trace_from(start),
                })
                .await;
        }
    }
}

async fn finalize_idempotent(
    app: &App,
    ctx: Option<&(String, String)>,
    payload: IdempotentReplayPayload,
) {
    if let Some((key, hash)) = ctx {
        let _ = app
            .spend_ledger
            .idempotency_finalize(key, hash, payload)
            .await;
    }
}

async fn clear_idempotent(app: &App, ctx: Option<&(String, String)>) {
    if let Some((key, hash)) = ctx {
        let _ = app.spend_ledger.idempotency_clear(key, hash).await;
    }
}

pub(crate) async fn dispatch_pay(app: &App, input: Input) {
    // Compute the canonical body hash before destructuring so Send / CashuSend
    // arms can compare their agent-supplied idempotency_key against a stable
    // identity of "what is this payment moving" (excludes the request id and
    // dry_run flag — see canonical_send_hash).
    let send_hash = canonical_send_hash(&input);

    match input {
        Input::Receive {
            id,
            wallet,
            network,
            amount,
            onchain_memo,
            wait_until_paid,
            wait_timeout_s,
            wait_poll_interval_ms,
            wait_sync_limit,
            write_qr_svg_file: _,
            min_confirmations,
            reference,
        } => {
            let start = Instant::now();
            let wait_requested = wait_until_paid
                || wait_timeout_s.is_some()
                || wait_poll_interval_ms.is_some()
                || wait_sync_limit.is_some();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "receive",
                    "wallet": &wallet,
                    "network": network.map(|c| c.to_string()).unwrap_or_else(|| "auto".to_string()),
                    "amount": amount.as_ref().map(|a| a.value),
                    "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
                    "wait_until_paid": wait_requested,
                    "wait_timeout_s": wait_timeout_s,
                    "wait_poll_interval_ms": wait_poll_interval_ms,
                    "wait_sync_limit": wait_sync_limit,
                }),
            )
            .await;

            let (target_network, wallet_for_call) =
                match resolve_wallet_for_provider(app, Some(&wallet), network).await {
                    Ok(resolved) => resolved,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };

            let Some(provider) = get_provider(&app.providers, target_network) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::not_implemented(format!("no provider for {target_network}")),
                    start,
                )
                .await;
                return;
            };

            match provider
                .receive_info(&wallet_for_call, amount.clone())
                .await
            {
                Ok(receive_info) => {
                    let quote_id = receive_info.quote_id.clone();
                    let is_bolt12 =
                        receive_info.address.is_some() && receive_info.invoice.is_none();
                    let _ = app
                        .writer
                        .send(Output::ReceiveInfo {
                            id: id.clone(),
                            wallet: wallet_for_call.clone(),
                            receive_info,
                            trace: trace_from(start),
                        })
                        .await;

                    if !wait_requested {
                        return;
                    }

                    let wait_options = match ReceiveWaitOptions::from_input(
                        wait_timeout_s,
                        wait_poll_interval_ms,
                        wait_sync_limit,
                        min_confirmations,
                    ) {
                        Ok(options) => options,
                        Err(e) => {
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    };

                    if supports_onchain_receive_wait(target_network) {
                        wait_onchain_receive(
                            target_network,
                            ReceiveWatchRequest {
                                app,
                                provider,
                                id: id.clone(),
                                wallet: wallet_for_call.clone(),
                                amount: amount.clone(),
                                onchain_memo: onchain_memo.clone(),
                                reference: reference.clone(),
                                options: wait_options,
                                start,
                            },
                        )
                        .await;
                        return;
                    }

                    let timeout_secs = wait_options.timeout_secs;
                    let poll_interval_ms = wait_options.poll_interval_ms;

                    let Some(quote_id) = quote_id else {
                        let msg = if is_bolt12 {
                            "bolt12 offers are persistent and do not support --wait; \
                             share the offer and check balance manually"
                                .to_string()
                        } else {
                            "deposit response missing quote_id/payment_hash".to_string()
                        };
                        emit_error(&app.writer, Some(id), &PayError::invalid_amount(msg), start)
                            .await;
                        return;
                    };

                    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                    loop {
                        match provider.receive_claim(&wallet_for_call, &quote_id).await {
                            Ok(claimed) => {
                                let _ = app
                                    .writer
                                    .send(Output::ReceiveClaimed {
                                        id,
                                        wallet: wallet_for_call.clone(),
                                        amount: Amount {
                                            value: claimed,
                                            token: "sats".to_string(),
                                        },
                                        trace: trace_from(start),
                                    })
                                    .await;
                                break;
                            }
                            Err(e) if e.retryable() => {
                                if Instant::now() >= deadline {
                                    emit_error(
                                        &app.writer,
                                        Some(id),
                                        &PayError::network_error(format!(
                                            "wait-until-paid timeout after {timeout_secs}s"
                                        )),
                                        start,
                                    )
                                    .await;
                                    break;
                                }
                                sleep(Duration::from_millis(poll_interval_ms)).await;
                            }
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::ReceiveClaim {
            id,
            wallet,
            quote_id,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "receive_claim", "wallet": &wallet, "quote_id": &quote_id,
                }),
            )
            .await;
            let (target_network, wallet_for_call) =
                match resolve_wallet_for_provider(app, Some(&wallet), None).await {
                    Ok(resolved) => resolved,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
            let Some(provider) = get_provider(&app.providers, target_network) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::not_implemented(format!("no provider for {target_network}")),
                    start,
                )
                .await;
                return;
            };

            match provider.receive_claim(&wallet_for_call, &quote_id).await {
                Ok(claimed) => {
                    let _ = app
                        .writer
                        .send(Output::ReceiveClaimed {
                            id,
                            wallet: wallet_for_call,
                            amount: Amount {
                                value: claimed,
                                token: "sats".to_string(),
                            },
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::CashuSend {
            id,
            wallet,
            amount,
            onchain_memo,
            local_memo,
            mints,
            idempotency_key,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "pay",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "cashu_send", "wallet": wallet.as_deref().unwrap_or("auto"),
                    "amount": amount.value, "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
                    "mints": mints.as_deref().unwrap_or(&[]),
                }),
            )
            .await;

            let idem_ctx = match enter_idempotent_send(
                app,
                &id,
                idempotency_key.as_deref(),
                send_hash.as_deref(),
                start,
            )
            .await
            {
                IdempotencyEntry::Proceed { ctx } => ctx,
                IdempotencyEntry::Done => return,
            };

            let wallet_str = wallet.unwrap_or_default();
            let mints_ref = mints.as_deref();
            let Some(provider) = get_provider(&app.providers, Network::Cashu) else {
                clear_idempotent(app, idem_ctx.as_ref()).await;
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::not_implemented("no provider for cashu".to_string()),
                    start,
                )
                .await;
                return;
            };

            let spend_ctx = SpendContext {
                network: "cashu".to_string(),
                wallet: if wallet_str.is_empty() {
                    None
                } else {
                    Some(wallet_str.clone())
                },
                amount_native: amount.value,
                token: None,
            };

            let outcome = with_spend_reserve(app, &id, "cashu_send", spend_ctx, start, || {
                provider.cashu_send(
                    &wallet_str,
                    amount.clone(),
                    onchain_memo.as_deref(),
                    mints_ref,
                )
            })
            .await;

            let Some(outcome) = outcome else {
                clear_idempotent(app, idem_ctx.as_ref()).await;
                return;
            };

            match outcome.result {
                Ok(r) => {
                    if local_memo.is_some()
                        && let Some(s) = &app.store
                    {
                        let _ = s
                            .update_transaction_record_memo(&r.transaction_id, local_memo.as_ref());
                    }
                    // Cross-link the history record with the spend ledger
                    // reservation ids — best-effort: silently no-ops when the
                    // provider has not yet written its history row.
                    if !outcome.confirmed_reservation_ids.is_empty()
                        && let Some(s) = &app.store
                    {
                        let _ = s.update_transaction_record_reservation_ids(
                            &r.transaction_id,
                            &outcome.confirmed_reservation_ids,
                        );
                    }
                    // AccountingInconsistent (money moved, ledger lost the debit)
                    // is itself a terminal state for idempotency — replay must
                    // emit the same inconsistency so the agent never retries.
                    if !outcome.unconfirmed_reservations.is_empty() {
                        let (reservation_ids, confirm_errors): (Vec<_>, Vec<_>) =
                            outcome.unconfirmed_reservations.iter().cloned().unzip();
                        let hint = "money left the wallet but the spend ledger could not record one or more debits; reconcile manually before issuing further sends to avoid double-spending the budget".to_string();
                        finalize_idempotent(
                            app,
                            idem_ctx.as_ref(),
                            IdempotentReplayPayload::AccountingInconsistent {
                                transaction_id: r.transaction_id.clone(),
                                reservation_ids: reservation_ids.clone(),
                                confirm_errors: confirm_errors.clone(),
                                hint: hint.clone(),
                            },
                        )
                        .await;
                    } else {
                        finalize_idempotent(
                            app,
                            idem_ctx.as_ref(),
                            IdempotentReplayPayload::CashuSent {
                                wallet: r.wallet.clone(),
                                transaction_id: r.transaction_id.clone(),
                                status: r.status,
                                fee: r.fee.clone(),
                                token: r.token.clone(),
                                reservation_ids: outcome.confirmed_reservation_ids.clone(),
                            },
                        )
                        .await;
                    }

                    // Surface ledger inconsistency BEFORE the success output so
                    // an agent sees the inconsistency first and never retries.
                    emit_accounting_inconsistent(
                        app,
                        &id,
                        &r.transaction_id,
                        outcome.unconfirmed_reservations,
                        start,
                    )
                    .await;
                    let _ = app
                        .writer
                        .send(Output::CashuSent {
                            id,
                            wallet: r.wallet,
                            transaction_id: r.transaction_id,
                            status: r.status,
                            fee: r.fee,
                            token: r.token,
                            reservation_ids: outcome.confirmed_reservation_ids,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => {
                    clear_idempotent(app, idem_ctx.as_ref()).await;
                    emit_error(&app.writer, Some(id), &e, start).await;
                }
            }
        }

        Input::CashuReceive { id, wallet, token } => {
            let start = Instant::now();
            let token_preview = if token.len() > 20 {
                format!("{}...", &token[..20])
            } else {
                token.clone()
            };
            emit_log(
                app,
                "pay",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "cashu_receive", "wallet": wallet.as_deref().unwrap_or("auto"), "token": token_preview,
                }),
            )
            .await;
            let wallet_str = wallet.unwrap_or_default();
            let Some(provider) = get_provider(&app.providers, Network::Cashu) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::not_implemented("no provider for cashu".to_string()),
                    start,
                )
                .await;
                return;
            };
            match provider.cashu_receive(&wallet_str, &token).await {
                Ok(r) => {
                    let _ = app
                        .writer
                        .send(Output::CashuReceived {
                            id,
                            wallet: r.wallet,
                            amount: r.amount,
                            memo: r.memo,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::Send {
            id,
            wallet,
            network,
            to,
            amount,
            onchain_memo,
            local_memo,
            mints,
            chain_id,
            idempotency_key,
        } => {
            let start = Instant::now();
            let operation_name = "send";
            let to_preview = if to.len() > 20 {
                format!("{}...", &to[..20])
            } else {
                to.clone()
            };
            emit_log(
                app,
                "pay",
                Some(id.clone()),
                serde_json::json!({
                    "operation": operation_name, "wallet": wallet.as_deref().unwrap_or("auto"),
                    "network": network.map(|c| c.to_string()).unwrap_or_else(|| "auto".to_string()),
                    "to": to_preview, "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
                    "chain_id": chain_id,
                }),
            )
            .await;

            let idem_ctx = match enter_idempotent_send(
                app,
                &id,
                idempotency_key.as_deref(),
                send_hash.as_deref(),
                start,
            )
            .await
            {
                IdempotencyEntry::Proceed { ctx } => ctx,
                IdempotencyEntry::Done => return,
            };

            let to = normalize_send_target(&to, amount.as_ref(), network);

            let wallet_arg = wallet.as_deref();
            let (target_network, wallet_for_call) =
                if wallet_arg.is_none() && matches!(network, Some(Network::Cashu)) {
                    // Cashu provider can select the smallest sufficient wallet after
                    // applying mint filters; this also works when Cashu is remote-only.
                    (Network::Cashu, String::new())
                } else {
                    match resolve_wallet_for_provider(app, wallet_arg, network).await {
                        Ok(resolved) => resolved,
                        Err(e) => {
                            clear_idempotent(app, idem_ctx.as_ref()).await;
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    }
                };

            // EVM chain-pinning: when the agent supplied chain_id, verify it
            // matches the wallet before any broadcast. Catches a Base wallet
            // being sent to from an "I expected Arbitrum" prompt — the agent
            // gets `wrong_chain` instead of a successful send on the wrong
            // chain that the agent has no way to detect after the fact.
            // When chain_id is omitted we cannot refuse (would break the
            // happy path for callers that don't track it), but we DO emit a
            // log so an observant agent / operator can spot accidental
            // cross-chain sends after the fact.
            if target_network == Network::Evm {
                let meta = app
                    .store
                    .as_deref()
                    .and_then(|s| s.load_wallet_metadata(&wallet_for_call).ok());
                let wallet_chain = meta.as_ref().and_then(|m| m.evm_chain_id).unwrap_or(8453); // Base default; mirrors EvmProvider::chain_id_for_wallet
                if let Some(supplied) = chain_id {
                    if supplied != wallet_chain {
                        clear_idempotent(app, idem_ctx.as_ref()).await;
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::Forbidden {
                                message: format!(
                                    "wrong_chain: supplied chain_id {supplied} does not match wallet chain_id {wallet_chain}"
                                ),
                                hint: Some(
                                    "verify the wallet is configured for the chain the agent intends; use wallet_config_set to change chain_id, or omit chain_id from the request"
                                        .to_string(),
                                ),
                            },
                            start,
                        )
                        .await;
                        return;
                    }
                } else {
                    emit_log(
                        app,
                        "evm_chain_unpinned",
                        Some(id.clone()),
                        serde_json::json!({
                            "wallet": wallet_for_call,
                            "wallet_chain_id": wallet_chain,
                            "hint": "send proceeded without an explicit chain_id; pass `chain_id` to pin the request to the wallet's chain and get a hard refuse on mismatch",
                        }),
                    )
                    .await;
                }
            }

            // SOL cluster pinning: when the wallet was tagged with a cluster
            // and the active RPC endpoint's hostname identifies a different
            // one, refuse the send. Heuristic — unknown hosts (private RPC,
            // proxy) yield no opinion and the check is skipped. This catches
            // the common case of the wallet metadata pointing at mainnet but
            // the rpc_endpoints having been swapped to a devnet URL.
            if target_network == Network::Sol
                && let Some(meta) = app
                    .store
                    .as_deref()
                    .and_then(|s| s.load_wallet_metadata(&wallet_for_call).ok())
                && let Some(expected) = meta.sol_cluster.as_deref()
            {
                let endpoints = meta.sol_rpc_endpoints.as_deref().unwrap_or(&[]);
                for ep in endpoints {
                    if let Some(detected) = sol_cluster_from_endpoint(ep)
                        && detected != expected
                    {
                        clear_idempotent(app, idem_ctx.as_ref()).await;
                        emit_error(
                                        &app.writer,
                                        Some(id),
                                        &PayError::Forbidden {
                                            message: format!(
                                                "wrong_cluster: wallet tagged sol_cluster={expected} but rpc endpoint '{ep}' looks like {detected}"
                                            ),
                                            hint: Some(
                                                "the wallet's recorded cluster does not match its configured rpc endpoint; either fix sol_rpc_endpoints via wallet_config_set or create a new wallet for the intended cluster"
                                                    .to_string(),
                                            ),
                                        },
                                        start,
                                    )
                                    .await;
                        return;
                    }
                }
            }

            let Some(provider) = get_provider(&app.providers, target_network) else {
                clear_idempotent(app, idem_ctx.as_ref()).await;
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::not_implemented(format!("no provider for {target_network}")),
                    start,
                )
                .await;
                return;
            };

            // Build spend contexts (requires a quote for Send to know amount and fee assets).
            let spend_contexts = if app.enforce_limits {
                let quote = match provider
                    .send_quote(&wallet_for_call, &to, mints.as_deref())
                    .await
                {
                    Ok(q) => q,
                    Err(e) => {
                        clear_idempotent(app, idem_ctx.as_ref()).await;
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                let provider_key = require_store(app)
                    .and_then(|s| s.load_wallet_metadata(&quote.wallet))
                    .ok()
                    .map(|meta| wallet_provider_key(&meta))
                    .unwrap_or_else(|| target_network.to_string());
                spend_contexts_from_quote(provider_key, &quote, &to)
            } else {
                Vec::new()
            };

            let outcome = with_spend_reserves(app, &id, "send", spend_contexts, start, || {
                provider.send(
                    &wallet_for_call,
                    &to,
                    onchain_memo.as_deref(),
                    mints.as_deref(),
                )
            })
            .await;

            let Some(outcome) = outcome else {
                clear_idempotent(app, idem_ctx.as_ref()).await;
                return;
            };

            match outcome.result {
                Ok(r) => {
                    if local_memo.is_some()
                        && let Some(s) = &app.store
                    {
                        let _ = s
                            .update_transaction_record_memo(&r.transaction_id, local_memo.as_ref());
                    }
                    if !outcome.confirmed_reservation_ids.is_empty()
                        && let Some(s) = &app.store
                    {
                        let _ = s.update_transaction_record_reservation_ids(
                            &r.transaction_id,
                            &outcome.confirmed_reservation_ids,
                        );
                    }
                    if !outcome.unconfirmed_reservations.is_empty() {
                        let (reservation_ids, confirm_errors): (Vec<_>, Vec<_>) =
                            outcome.unconfirmed_reservations.iter().cloned().unzip();
                        let hint = "money left the wallet but the spend ledger could not record one or more debits; reconcile manually before issuing further sends to avoid double-spending the budget".to_string();
                        finalize_idempotent(
                            app,
                            idem_ctx.as_ref(),
                            IdempotentReplayPayload::AccountingInconsistent {
                                transaction_id: r.transaction_id.clone(),
                                reservation_ids,
                                confirm_errors,
                                hint,
                            },
                        )
                        .await;
                    } else {
                        finalize_idempotent(
                            app,
                            idem_ctx.as_ref(),
                            IdempotentReplayPayload::Sent {
                                wallet: r.wallet.clone(),
                                transaction_id: r.transaction_id.clone(),
                                amount: r.amount.clone(),
                                fee: r.fee.clone(),
                                preimage: r.preimage.clone(),
                                reservation_ids: outcome.confirmed_reservation_ids.clone(),
                            },
                        )
                        .await;
                    }
                    emit_accounting_inconsistent(
                        app,
                        &id,
                        &r.transaction_id,
                        outcome.unconfirmed_reservations,
                        start,
                    )
                    .await;
                    let _ = app
                        .writer
                        .send(Output::Sent {
                            id,
                            wallet: r.wallet,
                            transaction_id: r.transaction_id,
                            amount: r.amount,
                            fee: r.fee,
                            preimage: r.preimage,
                            reservation_ids: outcome.confirmed_reservation_ids,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => {
                    clear_idempotent(app, idem_ctx.as_ref()).await;
                    emit_error(&app.writer, Some(id), &e, start).await;
                }
            }
        }

        _ => {}
    }
}

/// Embed `amount` into a BIP21-style send target when the explicit Input::Send.amount
/// is provided and the URI does not already carry one. This lets agents pass
/// `{to: "<address>", amount: {...}}` without hand-building network-specific URIs;
/// existing pre-built URIs and Lightning/Cashu paths pass through unchanged.
fn normalize_send_target(to: &str, amount: Option<&Amount>, network: Option<Network>) -> String {
    let Some(amount) = amount else {
        return to.to_string();
    };
    if to.contains("?amount=") || to.contains("&amount=") {
        // Caller already encoded an amount; respect it (URI wins; explicit
        // Amount is ignored to avoid silent mismatch).
        return to.to_string();
    }
    let already_has_scheme = to.contains(':');
    match network {
        Some(Network::Btc) => {
            if already_has_scheme {
                if to.contains('?') {
                    format!("{to}&amount={}", amount.value)
                } else {
                    format!("{to}?amount={}", amount.value)
                }
            } else {
                format!("bitcoin:{to}?amount={}", amount.value)
            }
        }
        Some(Network::Sol) => {
            let qs = format!("amount={}&token={}", amount.value, amount.token);
            if already_has_scheme {
                if to.contains('?') {
                    format!("{to}&{qs}")
                } else {
                    format!("{to}?{qs}")
                }
            } else {
                format!("solana:{to}?{qs}")
            }
        }
        Some(Network::Evm) => {
            let qs = format!("amount={}&token={}", amount.value, amount.token);
            if already_has_scheme {
                if to.contains('?') {
                    format!("{to}&{qs}")
                } else {
                    format!("{to}?{qs}")
                }
            } else {
                format!("ethereum:{to}?{qs}")
            }
        }
        // Lightning encodes amount in the invoice; Cashu uses CashuSend.
        // Passing amount here is a no-op rather than an error to keep the
        // wire surface uniform for agents that may set amount unconditionally.
        _ => to.to_string(),
    }
}

fn spend_contexts_from_quote(
    provider_key: String,
    quote: &SendQuoteInfo,
    to: &str,
) -> Vec<SpendContext> {
    let debits = if quote.spend_debits.is_empty() {
        vec![SpendDebit {
            amount_native: quote
                .amount_native
                .saturating_add(quote.fee_estimate_native),
            token: extract_token_from_target(to),
        }]
    } else {
        quote.spend_debits.clone()
    };

    debits
        .into_iter()
        .filter(|debit| debit.amount_native > 0)
        .map(|debit| SpendContext {
            network: provider_key.clone(),
            wallet: Some(quote.wallet.clone()),
            amount_native: debit.amount_native,
            token: normalize_spend_token(debit.token),
        })
        .collect()
}

fn normalize_spend_token(token: Option<String>) -> Option<String> {
    token
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}
