use crate::provider::PayError;
use crate::spend::{IdempotentReplayPayload, SpendContext};
use crate::store::{PayStore, plan};
use crate::types::*;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use super::App;
use super::helpers::*;
use super::idempotency::{
    IdempotencyEntry, canonical_request_hash, clear_idempotent, enter_idempotent,
    finalize_idempotent,
};
use super::receive_watch::{
    ReceiveWaitOptions, ReceiveWatchRequest, supports_onchain_receive_wait, wait_onchain_receive,
};
use super::spend_guard::{emit_accounting_inconsistent, with_spend_reserves};

pub(crate) async fn dispatch_pay(app: &App, input: Input) {
    // Read before destructuring so the hash covers the request exactly as it
    // arrived. Only `receive` and `pay_confirm` need one down here.
    let request_hash = canonical_request_hash(&input);

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
            idempotency_key,
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

            // A repeat mints a second invoice or mint quote while a payer may
            // already be holding the first, so this is not naturally
            // idempotent and takes a key like a payment does. An on-chain
            // address happens to be stable, but the rule is per-operation, not
            // per-network.
            let idem_ctx = match enter_idempotent(
                app,
                &id,
                idempotency_key.as_deref(),
                request_hash.as_deref(),
                start,
            )
            .await
            {
                IdempotencyEntry::Proceed { ctx } => ctx,
                IdempotencyEntry::Done => return,
            };

            let (target_network, wallet_for_call) =
                match resolve_wallet_for_provider(app, Some(&wallet), network).await {
                    Ok(resolved) => resolved,
                    Err(e) => {
                        clear_idempotent(app, idem_ctx.as_ref()).await;
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };

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

            match provider
                .receive_info(&wallet_for_call, amount.clone())
                .await
            {
                Ok(receive_info) => {
                    let quote_id = receive_info.quote_id.clone();
                    let is_bolt12 =
                        receive_info.address.is_some() && receive_info.invoice.is_none();
                    // A waiting receive is not finished yet, so its slot stays
                    // Pending — a retry gets `idempotency_in_progress` rather
                    // than a second invoice. A non-waiting one is done here.
                    if !wait_requested {
                        finalize_idempotent(
                            app,
                            idem_ctx.as_ref(),
                            IdempotentReplayPayload::ReceiveInfo {
                                wallet: wallet_for_call.clone(),
                                receive_info: receive_info.clone(),
                            },
                        )
                        .await;
                    }
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
                            clear_idempotent(app, idem_ctx.as_ref()).await;
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    };

                    if supports_onchain_receive_wait(target_network) {
                        // The chain watcher owns the rest of this request; its
                        // outcome is not something a later retry can replay, so
                        // the slot is released rather than finalised.
                        clear_idempotent(app, idem_ctx.as_ref()).await;
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
                        clear_idempotent(app, idem_ctx.as_ref()).await;
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
                                let amount = Amount {
                                    value: claimed,
                                    token: "sats".to_string(),
                                };
                                finalize_idempotent(
                                    app,
                                    idem_ctx.as_ref(),
                                    IdempotentReplayPayload::ReceiveClaimed {
                                        wallet: wallet_for_call.clone(),
                                        amount: amount.clone(),
                                    },
                                )
                                .await;
                                let _ = app
                                    .writer
                                    .send(Output::ReceiveClaimed {
                                        id,
                                        wallet: wallet_for_call.clone(),
                                        amount,
                                        trace: trace_from(start),
                                    })
                                    .await;
                                break;
                            }
                            Err(e) if e.retryable() => {
                                if Instant::now() >= deadline {
                                    clear_idempotent(app, idem_ctx.as_ref()).await;
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
                                clear_idempotent(app, idem_ctx.as_ref()).await;
                                emit_error(&app.writer, Some(id), &e, start).await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    clear_idempotent(app, idem_ctx.as_ref()).await;
                    emit_error(&app.writer, Some(id), &e, start).await;
                }
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

        Input::CashuSendPlan {
            id,
            wallet,
            amount,
            onchain_memo,
            local_memo,
            mints,
        } => {
            plan_cashu_send(app, id, wallet, amount, onchain_memo, local_memo, mints).await;
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

        Input::SendPlan {
            id,
            wallet,
            network,
            to,
            amount,
            onchain_memo,
            local_memo,
            mints,
            chain_id,
        } => {
            plan_send(
                app,
                id,
                wallet,
                network,
                to,
                amount,
                onchain_memo,
                local_memo,
                mints,
                chain_id,
            )
            .await;
        }

        Input::PayConfirm {
            id,
            plan_id,
            expect,
            idempotency_key,
        } => {
            confirm_plan(app, id, plan_id, expect, idempotency_key, request_hash).await;
        }

        _ => {}
    }
}

// ═══════════════════════════════════════════
// Plan: resolve a payment without making it
// ═══════════════════════════════════════════

/// Resolve a send and record it as a reviewable plan.
///
/// Everything that decides *what* the payment does happens here — wallet
/// selection, target normalisation, the chain pin and cluster signals, and the
/// provider's own quote — and none of it moves value. The confirm reads its
/// instructions back out of the record this writes, so a caller who reviewed a
/// plan and a daemon that executes it are looking at the same payment.
#[allow(clippy::too_many_arguments)]
async fn plan_send(
    app: &App,
    id: String,
    wallet: Option<String>,
    network: Option<Network>,
    to: String,
    amount: Option<Amount>,
    onchain_memo: Option<String>,
    local_memo: Option<BTreeMap<String, String>>,
    mints: Option<Vec<String>>,
    chain_id: Option<u64>,
) {
    let start = Instant::now();
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
            "operation": "send_plan", "wallet": wallet.as_deref().unwrap_or("auto"),
            "network": network.map(|c| c.to_string()).unwrap_or_else(|| "auto".to_string()),
            "to": to_preview, "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
            "chain_id": chain_id,
        }),
    )
    .await;

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
                    emit_error(&app.writer, Some(id), &e, start).await;
                    return;
                }
            }
        };

    // The chain pin and cluster signals live here rather than at confirm time.
    // They read wallet metadata, and wallet metadata is part of a plan's binding —
    // so a wallet that moves chains after a plan is resolved invalidates the
    // plan instead of slipping past a check that already ran.
    let pin_warnings =
        match check_chain_pins(app, &id, target_network, &wallet_for_call, chain_id).await {
            Ok(warnings) => warnings,
            Err(refusal) => {
                emit_error(&app.writer, Some(id), &refusal, start).await;
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

    // The one resolution. `send_quote` is read-only by contract: it must not
    // reserve budget, write to the ledger, or move value.
    let mut quote = match provider
        .send_quote(&wallet_for_call, &to, mints.as_deref())
        .await
    {
        Ok(quote) => quote,
        Err(e) => {
            emit_error(&app.writer, Some(id), &e, start).await;
            return;
        }
    };
    quote.warnings.extend(pin_warnings);

    let provider_key = require_store(app)
        .and_then(|s| s.load_wallet_metadata(&quote.wallet))
        .ok()
        .map(|meta| wallet_provider_key(&meta))
        .unwrap_or_else(|| target_network.to_string());
    let spend_debits = spend_debits_from_quote(&quote, &to);

    let plan = plan::PayPlan {
        plan_id: String::new(),
        operation: PayPlanOperation::Send,
        network: target_network,
        wallet: quote.wallet.clone(),
        to: Some(to),
        amount: None,
        onchain_memo,
        local_memo,
        mints,
        chain_id,
        spend_provider_key: provider_key,
        quote,
        binding: plan::PlanBinding {
            workspace: String::new(),
            config: String::new(),
            wallet: String::new(),
            limits: String::new(),
        },
        created_at_epoch_ms: 0,
        expires_at_epoch_ms: 0,
    };
    record_plan(app, &id, plan, spend_debits, start).await;
}

/// Resolve a Cashu bearer-token mint and record it as a reviewable plan.
async fn plan_cashu_send(
    app: &App,
    id: String,
    wallet: Option<String>,
    amount: Amount,
    onchain_memo: Option<String>,
    local_memo: Option<BTreeMap<String, String>>,
    mints: Option<Vec<String>>,
) {
    let start = Instant::now();
    emit_log(
        app,
        "pay",
        Some(id.clone()),
        serde_json::json!({
            "operation": "cashu_send_plan", "wallet": wallet.as_deref().unwrap_or("auto"),
            "amount": amount.value, "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
            "mints": mints.as_deref().unwrap_or(&[]),
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

    let quote = match provider.cashu_send_quote(&wallet_str, &amount).await {
        Ok(quote) => quote,
        Err(e) => {
            emit_error(&app.writer, Some(id), &e, start).await;
            return;
        }
    };

    // The debit stays what a Cashu mint has always debited — the face amount.
    // The mint's fee comes out of the proofs the wallet already holds, so
    // adding it here would double-count against the operator's budget.
    let spend_debits = vec![SpendDebit {
        amount_native: amount.value,
        token: None,
    }];

    let plan = plan::PayPlan {
        plan_id: String::new(),
        operation: PayPlanOperation::CashuSend,
        network: Network::Cashu,
        wallet: quote.wallet.clone(),
        to: None,
        amount: Some(amount.clone()),
        onchain_memo,
        local_memo,
        mints,
        chain_id: None,
        spend_provider_key: Network::Cashu.to_string(),
        quote: SendQuoteInfo {
            wallet: quote.wallet,
            amount_native: quote.amount_native,
            fee_estimate_native: quote.fee_native,
            fee_unit: quote.fee_unit,
            spend_debits: vec![SpendDebit {
                amount_native: amount.value,
                token: None,
            }],
            warnings: quote.warnings,
            upstream_plan_id: quote.upstream_plan_id,
        },
        binding: plan::PlanBinding {
            workspace: String::new(),
            config: String::new(),
            wallet: String::new(),
            limits: String::new(),
        },
        created_at_epoch_ms: 0,
        expires_at_epoch_ms: 0,
    };
    record_plan(app, &id, plan, spend_debits, start).await;
}

/// Stamp a resolved plan with its identity, lifetime and binding, write it to
/// the workspace, and tell the caller what it would do.
///
/// The spend contexts are carried on the plan's `quote.spend_debits`, which is
/// what the confirm reserves against — so what the reviewer was shown as
/// "budgets this debits" is exactly what gets reserved.
async fn record_plan(
    app: &App,
    id: &str,
    mut plan: plan::PayPlan,
    spend_debits: Vec<SpendDebit>,
    start: Instant,
) {
    let (data_dir, binding) = match plan_binding(app, &plan.wallet).await {
        Ok(resolved) => resolved,
        Err(e) => {
            emit_error(&app.writer, Some(id.to_string()), &e, start).await;
            return;
        }
    };
    let plan_id = match plan::generate_plan_identifier() {
        Ok(plan_id) => plan_id,
        Err(e) => {
            emit_error(&app.writer, Some(id.to_string()), &e, start).await;
            return;
        }
    };
    let now = plan::now_epoch_ms();
    plan.plan_id = plan_id.clone();
    plan.binding = binding;
    plan.created_at_epoch_ms = now;
    plan.expires_at_epoch_ms = now.saturating_add(plan::PLAN_TTL_MS);
    plan.quote.spend_debits = spend_debits;

    if let Err(e) = plan::save(&data_dir, &plan) {
        emit_error(&app.writer, Some(id.to_string()), &e, start).await;
        return;
    }

    let _ = app
        .writer
        .send(Output::PayPlanned {
            id: id.to_string(),
            plan_id,
            operation: plan.operation.as_str().to_string(),
            network: plan.network,
            wallet: plan.wallet.clone(),
            to: plan.to.clone(),
            amount_native: plan.quote.amount_native,
            fee_estimate_native: plan.quote.fee_estimate_native,
            fee_unit: plan.quote.fee_unit.clone(),
            onchain_memo: plan.onchain_memo.clone(),
            local_memo: plan.local_memo.clone().unwrap_or_default(),
            spend_debits: plan.quote.spend_debits.clone(),
            warnings: plan.quote.warnings.clone(),
            expires_at_epoch_ms: plan.expires_at_epoch_ms,
            trace: trace_from(start),
        })
        .await;
}

/// The workspace directory and the state a plan is being bound to.
async fn plan_binding(app: &App, wallet: &str) -> Result<(String, plan::PlanBinding), PayError> {
    let config = app.config.read().await;
    let data_dir = config.data_dir.clone();
    let limits = app.spend_ledger.get_status().await?;
    let wallet_metadata = app
        .store
        .as_deref()
        .and_then(|store| store.load_wallet_metadata(wallet).ok())
        .and_then(|meta| serde_json::to_value(&meta).ok());
    let binding = plan::PlanBinding::resolve(&data_dir, &config, wallet_metadata.as_ref(), &limits);
    Ok((data_dir, binding))
}

/// EVM chain pinning and best-effort Solana cluster signals.
async fn check_chain_pins(
    app: &App,
    id: &str,
    target_network: Network,
    wallet_for_call: &str,
    chain_id: Option<u64>,
) -> Result<Vec<PlanWarning>, PayError> {
    let mut warnings = Vec::new();
    // EVM: when the agent supplied chain_id, verify it matches the wallet.
    // Catches a Base wallet being sent to from an "I expected Arbitrum"
    // prompt — the agent gets `wrong_chain` instead of a payment on the wrong
    // chain it has no way to detect afterwards. When chain_id is omitted we
    // cannot refuse (it would break the happy path for callers that don't
    // track it), but we DO log so an observant agent or operator can spot an
    // accidental cross-chain send.
    if target_network == Network::Evm {
        let meta = app
            .store
            .as_deref()
            .and_then(|s| s.load_wallet_metadata(wallet_for_call).ok());
        // Base default; mirrors EvmProvider::chain_id_for_wallet.
        let wallet_chain = meta.as_ref().and_then(|m| m.evm_chain_id).unwrap_or(8453);
        match chain_id {
            Some(supplied) if supplied != wallet_chain => {
                return Err(PayError::Forbidden {
                    message: format!(
                        "wrong_chain: supplied chain_id {supplied} does not match wallet chain_id {wallet_chain}"
                    ),
                    hint: Some(
                        "verify the wallet is configured for the chain the agent intends; use wallet_config_set to change chain_id, or omit chain_id from the request"
                            .to_string(),
                    ),
                });
            }
            Some(_) => {}
            None => {
                warnings.push(PlanWarning {
                    code: "evm_chain_unpinned".to_string(),
                    message: format!(
                        "the plan did not pin chain_id; the selected wallet currently uses chain_id {wallet_chain}"
                    ),
                    hint: Some(
                        "pass chain_id when resolving the payment to get a hard refusal on mismatch"
                            .to_string(),
                    ),
                });
                emit_log(
                    app,
                    "evm_chain_unpinned",
                    Some(id.to_string()),
                    serde_json::json!({
                        "wallet": wallet_for_call,
                        "wallet_chain_id": wallet_chain,
                        "hint": "plan resolved without an explicit chain_id; pass `chain_id` to pin the request to the wallet's chain and get a hard refuse on mismatch",
                    }),
                )
                .await;
            }
        }
    }

    // A hostname is only a hint about a Solana cluster. Keep that hint visible
    // on the reviewable plan, but never turn it into a false hard guarantee.
    if target_network == Network::Sol {
        let meta = app
            .store
            .as_deref()
            .and_then(|s| s.load_wallet_metadata(wallet_for_call).ok());
        let sol_warnings = sol_cluster_plan_warnings(
            meta.as_ref().and_then(|meta| meta.sol_cluster.as_deref()),
            meta.as_ref()
                .and_then(|meta| meta.sol_rpc_endpoints.as_deref())
                .unwrap_or(&[]),
        );
        if !sol_warnings.is_empty() {
            emit_log(
                app,
                "plan_safety_warning",
                Some(id.to_string()),
                serde_json::json!({
                    "wallet": wallet_for_call,
                    "warnings": &sol_warnings,
                }),
            )
            .await;
        }
        warnings.extend(sol_warnings);
    }
    Ok(warnings)
}

fn sol_cluster_plan_warnings(expected: Option<&str>, endpoints: &[String]) -> Vec<PlanWarning> {
    let Some(expected) = expected else {
        return vec![PlanWarning {
            code: "sol_cluster_unpinned".to_string(),
            message: "the wallet has no recorded Solana cluster".to_string(),
            hint: Some(
                "record sol_cluster on the wallet before relying on cluster intent".to_string(),
            ),
        }];
    };

    let mut identified_any = false;
    let mut unidentified_any = false;
    for endpoint in endpoints {
        if let Some(detected) = sol_cluster_from_endpoint(endpoint) {
            identified_any = true;
            if detected != expected {
                return vec![PlanWarning {
                    code: "sol_cluster_mismatch_heuristic".to_string(),
                    message: format!(
                        "wallet cluster is {expected}, but a configured RPC endpoint hostname looks like {detected}"
                    ),
                    hint: Some(
                        "verify the endpoint and wallet cluster before confirming; hostname detection is best-effort"
                            .to_string(),
                    ),
                }];
            }
        } else {
            unidentified_any = true;
        }
    }
    if identified_any && !unidentified_any {
        Vec::new()
    } else {
        vec![PlanWarning {
            code: "sol_cluster_unverified".to_string(),
            message: format!(
                "one or more RPC endpoint hostnames do not identify the recorded {expected} cluster"
            ),
            hint: Some(
                "verify the endpoint independently before confirming; private and proxied RPC hosts cannot be classified from their names"
                    .to_string(),
            ),
        }]
    }
}

// ═══════════════════════════════════════════
// Confirm: the only path that moves money
// ═══════════════════════════════════════════

/// Execute a plan someone reviewed.
///
/// The order matters and is the whole safety argument:
///
/// 1. **Idempotency first.** A retry carrying the key of a confirm that
///    already ran replays its terminal output, and never reaches the plan —
///    which by then is spent. Putting the plan lookup first would turn every
///    legitimate retry into `plan_not_found`.
/// 2. **Then the plan's own validity**, read without taking it, so a refusal
///    never burns the plan it refused.
/// 3. **Then the claim**, which is an atomic rename: exactly one confirm can
///    take a plan even when two callers with different keys race for it.
/// 4. **Then the existing ledger path**, unchanged — reserve, execute,
///    confirm or cancel, `AccountingInconsistent` when the money left but the
///    ledger could not record it.
async fn confirm_plan(
    app: &App,
    id: String,
    plan_id: String,
    expect: Option<PayPlanOperation>,
    idempotency_key: Option<String>,
    request_hash: Option<String>,
) {
    let start = Instant::now();
    emit_log(
        app,
        "pay",
        Some(id.clone()),
        serde_json::json!({"operation": "pay_confirm", "plan_id": plan_id}),
    )
    .await;

    let idem_ctx = match enter_idempotent(
        app,
        &id,
        idempotency_key.as_deref(),
        request_hash.as_deref(),
        start,
    )
    .await
    {
        IdempotencyEntry::Proceed { ctx } => ctx,
        IdempotencyEntry::Done => return,
    };

    let data_dir = app.config.read().await.data_dir.clone();
    let plan = match plan::peek(&data_dir, &plan_id) {
        Ok(plan) => plan,
        Err(e) => {
            clear_idempotent(app, idem_ctx.as_ref()).await;
            emit_error(&app.writer, Some(id), &e, start).await;
            return;
        }
    };
    if plan.expired(plan::now_epoch_ms()) {
        clear_idempotent(app, idem_ctx.as_ref()).await;
        emit_error(
            &app.writer,
            Some(id),
            &PayError::PlanExpired {
                message: format!(
                    "plan '{plan_id}' expired at epoch ms {}",
                    plan.expires_at_epoch_ms
                ),
            },
            start,
        )
        .await;
        return;
    }

    // §9: configuration, identity and workspace changes invalidate an
    // outstanding plan. Recomputed here against live state, never re-resolved.
    let current = match plan_binding(app, &plan.wallet).await {
        Ok((_, binding)) => binding,
        Err(e) => {
            clear_idempotent(app, idem_ctx.as_ref()).await;
            emit_error(&app.writer, Some(id), &e, start).await;
            return;
        }
    };
    let drifted = plan.binding.drifted_from(&current);
    if !drifted.is_empty() {
        clear_idempotent(app, idem_ctx.as_ref()).await;
        emit_error(
            &app.writer,
            Some(id),
            &PayError::PlanStale {
                message: format!(
                    "plan '{plan_id}' was resolved before {} changed",
                    drifted.join(" and ")
                ),
                drifted: drifted.iter().map(|part| part.to_string()).collect(),
            },
            start,
        )
        .await;
        return;
    }

    if let Some(expect) = expect
        && expect != plan.operation
    {
        clear_idempotent(app, idem_ctx.as_ref()).await;
        emit_error(
            &app.writer,
            Some(id),
            &PayError::invalid_request(format!(
                "plan '{plan_id}' authorises {}, not {}",
                plan.operation.as_str(),
                expect.as_str()
            )),
            start,
        )
        .await;
        return;
    }

    let plan = match plan::claim(&data_dir, &plan_id) {
        Ok(plan) => plan,
        Err(e) => {
            clear_idempotent(app, idem_ctx.as_ref()).await;
            emit_error(&app.writer, Some(id), &e, start).await;
            return;
        }
    };

    match plan.operation {
        PayPlanOperation::Send => {
            execute_send(app, &id, &data_dir, plan, idem_ctx, start).await;
        }
        PayPlanOperation::CashuSend => {
            execute_cashu_send(app, &id, &data_dir, plan, idem_ctx, start).await;
        }
    }
}

/// Broadcast the payment a plan describes.
///
/// Nothing is re-resolved: the wallet, the destination and the debits all come
/// off the plan. A fee that moved since it was quoted is the same exposure
/// afpay has always carried — the estimate reserves, the network charges what
/// it charges — but the caller never approves one payment and gets another.
async fn execute_send(
    app: &App,
    id: &str,
    data_dir: &str,
    plan: plan::PayPlan,
    idem_ctx: Option<(String, String)>,
    start: Instant,
) {
    let Some(provider) = get_provider(&app.providers, plan.network) else {
        settle_plan(app, data_dir, &plan.plan_id, idem_ctx.as_ref(), false).await;
        emit_error(
            &app.writer,
            Some(id.to_string()),
            &PayError::not_implemented(format!("no provider for {}", plan.network)),
            start,
        )
        .await;
        return;
    };
    let to = plan.to.clone().unwrap_or_default();
    let spend_contexts = spend_contexts_from_plan(&plan);

    let outcome = with_spend_reserves(app, id, "send", spend_contexts, start, || {
        provider.send_confirmed(
            &plan.wallet,
            &to,
            plan.onchain_memo.as_deref(),
            plan.mints.as_deref(),
            plan.quote.upstream_plan_id.as_deref(),
        )
    })
    .await;

    let Some(outcome) = outcome else {
        // A spend rule refused before anything was broadcast. The plan is
        // intact and the terms are unchanged, so hand it back rather than
        // making the caller review the same payment twice.
        settle_plan(app, data_dir, &plan.plan_id, idem_ctx.as_ref(), false).await;
        return;
    };

    match outcome.result {
        Ok(r) => {
            let accounting_inconsistent = !outcome.unconfirmed_reservations.is_empty();
            if plan.local_memo.is_some()
                && let Some(s) = &app.store
            {
                let _ =
                    s.update_transaction_record_memo(&r.transaction_id, plan.local_memo.as_ref());
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
                finalize_idempotent(
                    app,
                    idem_ctx.as_ref(),
                    IdempotentReplayPayload::AccountingInconsistent {
                        transaction_id: r.transaction_id.clone(),
                        reservation_ids,
                        confirm_errors,
                        hint: ACCOUNTING_INCONSISTENT_HINT.to_string(),
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
            plan::consume(data_dir, &plan.plan_id);
            emit_accounting_inconsistent(
                app,
                id,
                &r.transaction_id,
                outcome.unconfirmed_reservations,
                start,
            )
            .await;
            if accounting_inconsistent {
                return;
            }
            let _ = app
                .writer
                .send(Output::Sent {
                    id: id.to_string(),
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
            settle_plan(app, data_dir, &plan.plan_id, idem_ctx.as_ref(), false).await;
            emit_error(&app.writer, Some(id.to_string()), &e, start).await;
        }
    }
}

/// Mint the bearer token a plan describes.
async fn execute_cashu_send(
    app: &App,
    id: &str,
    data_dir: &str,
    plan: plan::PayPlan,
    idem_ctx: Option<(String, String)>,
    start: Instant,
) {
    let Some(provider) = get_provider(&app.providers, Network::Cashu) else {
        settle_plan(app, data_dir, &plan.plan_id, idem_ctx.as_ref(), false).await;
        emit_error(
            &app.writer,
            Some(id.to_string()),
            &PayError::not_implemented("no provider for cashu".to_string()),
            start,
        )
        .await;
        return;
    };
    let Some(amount) = plan.amount.clone() else {
        // A defect, not an outcome: nothing was attempted, so hand both the
        // plan and the key back rather than wedging them for 24 hours.
        settle_plan(app, data_dir, &plan.plan_id, idem_ctx.as_ref(), false).await;
        emit_error(
            &app.writer,
            Some(id.to_string()),
            &PayError::internal_error("cashu plan carries no amount".to_string()),
            start,
        )
        .await;
        return;
    };
    let spend_contexts = spend_contexts_from_plan(&plan);

    let outcome = with_spend_reserves(app, id, "cashu_send", spend_contexts, start, || {
        provider.cashu_send_confirmed(
            &plan.wallet,
            amount.clone(),
            plan.onchain_memo.as_deref(),
            plan.mints.as_deref(),
            plan.quote.upstream_plan_id.as_deref(),
        )
    })
    .await;

    let Some(outcome) = outcome else {
        settle_plan(app, data_dir, &plan.plan_id, idem_ctx.as_ref(), false).await;
        return;
    };

    match outcome.result {
        Ok(r) => {
            let accounting_inconsistent = !outcome.unconfirmed_reservations.is_empty();
            if plan.local_memo.is_some()
                && let Some(s) = &app.store
            {
                let _ =
                    s.update_transaction_record_memo(&r.transaction_id, plan.local_memo.as_ref());
            }
            // Cross-link the history record with the spend ledger reservation
            // ids — best-effort: silently no-ops when the provider has not yet
            // written its history row.
            if !outcome.confirmed_reservation_ids.is_empty()
                && let Some(s) = &app.store
            {
                let _ = s.update_transaction_record_reservation_ids(
                    &r.transaction_id,
                    &outcome.confirmed_reservation_ids,
                );
            }
            // AccountingInconsistent (money moved, ledger lost the debit) is
            // itself a terminal state for idempotency — replay must emit the
            // same inconsistency so the agent never retries.
            if !outcome.unconfirmed_reservations.is_empty() {
                let (reservation_ids, confirm_errors): (Vec<_>, Vec<_>) =
                    outcome.unconfirmed_reservations.iter().cloned().unzip();
                finalize_idempotent(
                    app,
                    idem_ctx.as_ref(),
                    IdempotentReplayPayload::AccountingInconsistent {
                        transaction_id: r.transaction_id.clone(),
                        reservation_ids,
                        confirm_errors,
                        hint: ACCOUNTING_INCONSISTENT_HINT.to_string(),
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
            plan::consume(data_dir, &plan.plan_id);
            // Ledger inconsistency is the sole terminal output; a normal
            // success below is emitted only when every reservation committed.
            emit_accounting_inconsistent(
                app,
                id,
                &r.transaction_id,
                outcome.unconfirmed_reservations,
                start,
            )
            .await;
            if accounting_inconsistent {
                return;
            }
            let _ = app
                .writer
                .send(Output::CashuSent {
                    id: id.to_string(),
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
            settle_plan(app, data_dir, &plan.plan_id, idem_ctx.as_ref(), false).await;
            emit_error(&app.writer, Some(id.to_string()), &e, start).await;
        }
    }
}

const ACCOUNTING_INCONSISTENT_HINT: &str = "money left the wallet but the spend ledger could not record one or more debits; reconcile manually before issuing further sends to avoid double-spending the budget";

/// Give a claimed plan back, or keep it.
///
/// `spent` follows exactly the judgement the idempotency ledger already makes:
/// when afpay is willing to clear the key and let an identical request run
/// again, the payment demonstrably did not happen, so the plan is safe to hand
/// back too. Anything else keeps the plan taken.
async fn settle_plan(
    app: &App,
    data_dir: &str,
    plan_id: &str,
    idem_ctx: Option<&(String, String)>,
    spent: bool,
) {
    if spent {
        plan::consume(data_dir, plan_id);
    } else {
        plan::release(data_dir, plan_id);
        clear_idempotent(app, idem_ctx).await;
    }
}

/// The budgets the reviewer was shown, in the shape the ledger reserves.
fn spend_contexts_from_plan(plan: &plan::PayPlan) -> Vec<SpendContext> {
    let provider_key = plan.spend_provider_key.clone();
    plan.quote
        .spend_debits
        .iter()
        .filter(|debit| debit.amount_native > 0)
        .map(|debit| SpendContext {
            network: provider_key.clone(),
            wallet: Some(plan.wallet.clone()),
            amount_native: debit.amount_native,
            token: debit.token.clone(),
        })
        .collect()
}

/// Embed `amount` into a BIP21-style send target when the explicit
/// `Input::SendPlan.amount` is provided and the URI does not already carry
/// one. This lets agents pass
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

/// The budgets one quoted payment would debit, normalised.
///
/// A provider that names its own debits knows best (a token transfer debits
/// the token *and* the gas asset); one that does not gets the single "amount
/// plus fee in the target's asset" reading afpay has always used.
fn spend_debits_from_quote(quote: &SendQuoteInfo, to: &str) -> Vec<SpendDebit> {
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
        .map(|debit| SpendDebit {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::sol_cluster_plan_warnings;

    #[test]
    fn solana_hostname_evidence_is_a_plan_warning_not_a_refusal() {
        let mismatch = sol_cluster_plan_warnings(
            Some("mainnet-beta"),
            &["https://api.devnet.solana.com".to_string()],
        );
        assert_eq!(mismatch.len(), 1);
        assert_eq!(mismatch[0].code, "sol_cluster_mismatch_heuristic");

        let unknown = sol_cluster_plan_warnings(
            Some("mainnet-beta"),
            &["https://private-rpc.example".to_string()],
        );
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].code, "sol_cluster_unverified");

        let mixed = sol_cluster_plan_warnings(
            Some("devnet"),
            &[
                "https://api.devnet.solana.com".to_string(),
                "https://private-rpc.example".to_string(),
            ],
        );
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].code, "sol_cluster_unverified");

        let matching = sol_cluster_plan_warnings(
            Some("devnet"),
            &["https://api.devnet.solana.com".to_string()],
        );
        assert!(matching.is_empty());
    }
}
