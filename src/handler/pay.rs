use crate::provider::PayError;
use crate::spend::SpendContext;
use crate::store::PayStore;
use crate::types::*;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use super::helpers::*;
use super::receive_watch::{
    supports_onchain_receive_wait, wait_onchain_receive, ReceiveWaitOptions, ReceiveWatchRequest,
};
use super::spend_guard::{with_spend_reserve, with_spend_reserves};
use super::App;

pub(crate) async fn dispatch_pay(app: &App, input: Input) {
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
                    &PayError::NotImplemented(format!("no provider for {target_network}")),
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
                        emit_error(&app.writer, Some(id), &PayError::InvalidAmount(msg), start)
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
                                        &PayError::NetworkError(format!(
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
                    &PayError::NotImplemented(format!("no provider for {target_network}")),
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

            let wallet_str = wallet.unwrap_or_default();
            let mints_ref = mints.as_deref();
            let Some(provider) = get_provider(&app.providers, Network::Cashu) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented("no provider for cashu".to_string()),
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

            let result = with_spend_reserve(app, &id, "cashu_send", spend_ctx, start, || {
                provider.cashu_send(
                    &wallet_str,
                    amount.clone(),
                    onchain_memo.as_deref(),
                    mints_ref,
                )
            })
            .await;

            let Some(result) = result else { return };

            match result {
                Ok(r) => {
                    if local_memo.is_some() {
                        if let Some(s) = &app.store {
                            let _ = s.update_transaction_record_memo(
                                &r.transaction_id,
                                local_memo.as_ref(),
                            );
                        }
                    }
                    let _ = app
                        .writer
                        .send(Output::CashuSent {
                            id,
                            wallet: r.wallet,
                            transaction_id: r.transaction_id,
                            status: r.status,
                            fee: r.fee,
                            token: r.token,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
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
                    &PayError::NotImplemented("no provider for cashu".to_string()),
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
            onchain_memo,
            local_memo,
            mints,
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
                }),
            )
            .await;

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

            let Some(provider) = get_provider(&app.providers, target_network) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(format!("no provider for {target_network}")),
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

            let result = with_spend_reserves(app, &id, "send", spend_contexts, start, || {
                provider.send(
                    &wallet_for_call,
                    &to,
                    onchain_memo.as_deref(),
                    mints.as_deref(),
                )
            })
            .await;

            let Some(result) = result else { return };

            match result {
                Ok(r) => {
                    if local_memo.is_some() {
                        if let Some(s) = &app.store {
                            let _ = s.update_transaction_record_memo(
                                &r.transaction_id,
                                local_memo.as_ref(),
                            );
                        }
                    }
                    let _ = app
                        .writer
                        .send(Output::Sent {
                            id,
                            wallet: r.wallet,
                            transaction_id: r.transaction_id,
                            amount: r.amount,
                            fee: r.fee,
                            preimage: r.preimage,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        _ => {}
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
