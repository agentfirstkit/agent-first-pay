use crate::provider::PayError;
use crate::spend::SpendContext;
use crate::store::PayStore;
use crate::types::*;
use std::collections::HashSet;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use super::helpers::*;
use super::App;

const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_WAIT_POLL_INTERVAL_MS: u64 = 1000;
const DEFAULT_WAIT_SYNC_LIMIT: usize = 500;

/// Reserve spend budget, execute an async operation, then confirm or cancel.
///
/// On success: confirms the reservation (with warn log on confirm failure).
/// On failure: cancels the reservation with one retry on cancel failure.
/// Returns `Some(result)` if the operation ran, `None` if the reserve was rejected
/// (in which case the appropriate error/LimitExceeded output has already been emitted).
async fn with_spend_reserve<F, Fut, T>(
    app: &App,
    id: &str,
    op_prefix: &str,
    spend_ctx: SpendContext,
    start: Instant,
    send_fn: F,
) -> Option<Result<T, PayError>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, PayError>>,
{
    let reservation_id = if app.enforce_limits {
        match app
            .spend_ledger
            .reserve(&format!("{op_prefix}:{id}"), &spend_ctx)
            .await
        {
            Ok(rid) => {
                if app.spend_ledger.take_fx_stale_warning() {
                    emit_log(
                        app,
                        "fx_quote_stale",
                        Some(id.to_string()),
                        serde_json::json!({
                            "message": "exchange rate quote age exceeds 80% of TTL; rate may be outdated",
                        }),
                    )
                    .await;
                }
                Some(rid)
            }
            Err(e) => {
                if let PayError::LimitExceeded {
                    rule_id,
                    scope,
                    scope_key,
                    spent,
                    max_spend,
                    token,
                    remaining_s,
                    origin,
                } = &e
                {
                    let _ = app
                        .writer
                        .send(Output::LimitExceeded {
                            id: id.to_string(),
                            rule_id: rule_id.clone(),
                            scope: *scope,
                            scope_key: scope_key.clone(),
                            spent: *spent,
                            max_spend: *max_spend,
                            token: token.clone(),
                            remaining_s: *remaining_s,
                            origin: origin.clone(),
                            trace: trace_from(start),
                        })
                        .await;
                } else {
                    emit_error(&app.writer, Some(id.to_string()), &e, start).await;
                }
                return None;
            }
        }
    } else {
        None
    };

    let result = send_fn().await;

    if let Some(rid) = reservation_id {
        match &result {
            Ok(_) => {
                if let Err(e) = app.spend_ledger.confirm(rid).await {
                    emit_log(
                        app,
                        "spend_confirm_failed",
                        Some(id.to_string()),
                        serde_json::json!({
                            "reservation_id": rid,
                            "error": e.to_string(),
                        }),
                    )
                    .await;
                }
            }
            Err(_) => {
                if let Err(first_err) = app.spend_ledger.cancel(rid).await {
                    // Retry once
                    if let Err(retry_err) = app.spend_ledger.cancel(rid).await {
                        emit_log(
                            app,
                            "spend_cancel_failed",
                            Some(id.to_string()),
                            serde_json::json!({
                                "reservation_id": rid,
                                "first_error": first_err.to_string(),
                                "retry_error": retry_err.to_string(),
                            }),
                        )
                        .await;
                    }
                }
            }
        }
    }

    Some(result)
}

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

            let (target_network, wallet_for_call) = if wallet.trim().is_empty() {
                let wallets = match require_store(app).and_then(|s| s.list_wallet_metadata(network))
                {
                    Ok(v) => v,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                match wallets.len() {
                    0 => {
                        let msg = match network {
                            Some(network) => format!("no {network} wallet found"),
                            None => "no wallet found".to_string(),
                        };
                        emit_error(&app.writer, Some(id), &PayError::WalletNotFound(msg), start)
                            .await;
                        return;
                    }
                    1 => (wallets[0].network, wallets[0].id.clone()),
                    _ => {
                        let msg = match network {
                            Some(network) => {
                                format!("multiple {network} wallets found; pass --wallet")
                            }
                            None => "multiple wallets found; pass --wallet".to_string(),
                        };
                        emit_error(&app.writer, Some(id), &PayError::InvalidAmount(msg), start)
                            .await;
                        return;
                    }
                }
            } else {
                let meta = match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                    Ok(m) => m,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                if let Some(expected) = network {
                    if meta.network != expected {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "wallet {wallet} is {}, not {expected}",
                                meta.network
                            )),
                            start,
                        )
                        .await;
                        return;
                    }
                }
                (meta.network, wallet.clone())
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

                    let timeout_secs = wait_timeout_s.unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS);
                    if timeout_secs == 0 {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount("wait_timeout_s must be >= 1".to_string()),
                            start,
                        )
                        .await;
                        return;
                    }
                    let poll_interval_ms =
                        wait_poll_interval_ms.unwrap_or(DEFAULT_WAIT_POLL_INTERVAL_MS);
                    if poll_interval_ms == 0 {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(
                                "wait_poll_interval_ms must be >= 1".to_string(),
                            ),
                            start,
                        )
                        .await;
                        return;
                    }
                    let sync_limit = wait_sync_limit
                        .unwrap_or(DEFAULT_WAIT_SYNC_LIMIT)
                        .clamp(1, 5000);

                    if target_network == Network::Sol {
                        let memo_to_watch = onchain_memo
                            .as_deref()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(str::to_owned);
                        let amount_to_watch = amount.as_ref().map(|a| a.value);
                        let reference_to_watch = reference.clone();

                        if memo_to_watch.is_none()
                            && amount_to_watch.is_none()
                            && reference_to_watch.is_none()
                        {
                            emit_error_hint(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "sol receive --wait requires a match condition".to_string(),
                                ),
                                start,
                                Some("pass --onchain-memo, --amount, or --reference"),
                            )
                            .await;
                            return;
                        }

                        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                        loop {
                            match provider.history_list(&wallet_for_call, 200, 0).await {
                                Ok(items) => {
                                    let matched = items.into_iter().find(|item| {
                                        if item.direction != Direction::Receive {
                                            return false;
                                        }
                                        if let Some(ref r) = reference_to_watch {
                                            let has_ref = item
                                                .reference_keys
                                                .as_ref()
                                                .is_some_and(|keys| keys.iter().any(|k| k == r));
                                            if !has_ref {
                                                return false;
                                            }
                                        }
                                        if let Some(ref m) = memo_to_watch {
                                            item.onchain_memo.as_deref() == Some(m.as_str())
                                        } else if let Some(expected) = amount_to_watch {
                                            item.amount.value == expected
                                        } else {
                                            reference_to_watch.is_some()
                                        }
                                    });
                                    if let Some(item) = matched {
                                        // Check confirmation depth if requested
                                        if let Some(min_conf) = min_confirmations {
                                            match provider
                                                .history_status(&item.transaction_id)
                                                .await
                                            {
                                                Ok(status_info) => {
                                                    let confs = status_info
                                                        .confirmations
                                                        .unwrap_or_else(|| {
                                                            if status_info.status
                                                                == TxStatus::Confirmed
                                                            {
                                                                min_conf
                                                            } else {
                                                                0
                                                            }
                                                        });
                                                    if confs < min_conf {
                                                        // Not enough confirmations yet, keep polling
                                                        if Instant::now() >= deadline {
                                                            let criteria = if let Some(ref m) =
                                                                memo_to_watch
                                                            {
                                                                format!("memo '{m}'")
                                                            } else if let Some(expected) =
                                                                amount_to_watch
                                                            {
                                                                format!("amount {expected}")
                                                            } else if let Some(ref r) =
                                                                reference_to_watch
                                                            {
                                                                format!("reference '{r}'")
                                                            } else {
                                                                "unknown".to_string()
                                                            };
                                                            emit_error(
                                                                &app.writer,
                                                                Some(id),
                                                                &PayError::NetworkError(format!(
                                                                    "wait timeout after {timeout_secs}s: sol transaction {tx} matching {criteria} has {confs}/{min_conf} confirmations",
                                                                    tx = item.transaction_id,
                                                                )),
                                                                start,
                                                            )
                                                            .await;
                                                            break;
                                                        }
                                                        sleep(Duration::from_millis(
                                                            poll_interval_ms,
                                                        ))
                                                        .await;
                                                        continue;
                                                    }
                                                    // Enough confirmations — emit with confirmation count
                                                    let transaction_id =
                                                        item.transaction_id.clone();
                                                    let _ = app
                                                        .writer
                                                        .send(Output::HistoryStatus {
                                                            id,
                                                            transaction_id,
                                                            status: item.status,
                                                            confirmations: Some(confs),
                                                            preimage: item.preimage.clone(),
                                                            item: Some(item),
                                                            trace: trace_from(start),
                                                        })
                                                        .await;
                                                    break;
                                                }
                                                Err(e) if e.retryable() => {
                                                    sleep(Duration::from_millis(poll_interval_ms))
                                                        .await;
                                                    continue;
                                                }
                                                Err(e) => {
                                                    emit_error(&app.writer, Some(id), &e, start)
                                                        .await;
                                                    break;
                                                }
                                            }
                                        } else {
                                            let transaction_id = item.transaction_id.clone();
                                            let _ = app
                                                .writer
                                                .send(Output::HistoryStatus {
                                                    id,
                                                    transaction_id,
                                                    status: item.status,
                                                    confirmations: None,
                                                    preimage: item.preimage.clone(),
                                                    item: Some(item),
                                                    trace: trace_from(start),
                                                })
                                                .await;
                                            break;
                                        }
                                    }
                                    if Instant::now() >= deadline {
                                        let criteria = if let Some(ref m) = memo_to_watch {
                                            format!("memo '{m}'")
                                        } else if let Some(expected) = amount_to_watch {
                                            format!("amount {expected}")
                                        } else if let Some(ref r) = reference_to_watch {
                                            format!("reference '{r}'")
                                        } else {
                                            "unknown".to_string()
                                        };
                                        emit_error(
                                            &app.writer,
                                            Some(id),
                                            &PayError::NetworkError(format!(
                                                "wait timeout after {timeout_secs}s: no incoming sol transaction matching {criteria}"
                                            )),
                                            start,
                                        )
                                        .await;
                                        break;
                                    }
                                    sleep(Duration::from_millis(poll_interval_ms)).await;
                                }
                                Err(e) if e.retryable() => {
                                    if Instant::now() >= deadline {
                                        let criteria = if let Some(ref m) = memo_to_watch {
                                            format!("memo '{m}'")
                                        } else if let Some(expected) = amount_to_watch {
                                            format!("amount {expected}")
                                        } else if let Some(ref r) = reference_to_watch {
                                            format!("reference '{r}'")
                                        } else {
                                            "unknown".to_string()
                                        };
                                        emit_error(
                                            &app.writer,
                                            Some(id),
                                            &PayError::NetworkError(format!(
                                                "wait timeout after {timeout_secs}s: no incoming sol transaction matching {criteria}"
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
                        return;
                    }

                    // EVM: poll balance deltas, then resolve matched on-chain tx hash from history.
                    if target_network == Network::Evm {
                        let memo_to_watch = onchain_memo
                            .as_deref()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(str::to_owned);
                        let amount_to_watch = amount.as_ref().map(|a| a.value);
                        let token_to_watch = amount.as_ref().map(|a| a.token.to_ascii_lowercase());

                        if amount_to_watch.is_none() {
                            emit_error_hint(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "evm receive --wait requires --amount".to_string(),
                                ),
                                start,
                                Some("pass --amount"),
                            )
                            .await;
                            return;
                        }
                        let wait_criteria = if let Some(ref memo) = memo_to_watch {
                            format!("amount {} and memo '{memo}'", amount_to_watch.unwrap_or(0))
                        } else {
                            format!("amount {}", amount_to_watch.unwrap_or(0))
                        };

                        // Snapshot known receives so we only match newly arrived transactions.
                        let mut known_receive_ids: HashSet<String> =
                            match provider.history_list(&wallet_for_call, 1000, 0).await {
                                Ok(items) => items
                                    .into_iter()
                                    .filter(|item| item.direction == Direction::Receive)
                                    .map(|item| item.transaction_id)
                                    .collect(),
                                Err(_) => HashSet::new(),
                            };

                        let initial_balance = match provider.balance(&wallet_for_call).await {
                            Ok(b) => b,
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        };

                        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                        'evm_wait: loop {
                            sleep(Duration::from_millis(poll_interval_ms)).await;
                            if Instant::now() >= deadline {
                                emit_error(
                                    &app.writer,
                                    Some(id),
                                    &PayError::NetworkError(format!(
                                        "wait timeout after {timeout_secs}s: no incoming evm deposit matching {wait_criteria}"
                                    )),
                                    start,
                                )
                                .await;
                                break;
                            }

                            let current = match provider.balance(&wallet_for_call).await {
                                Ok(current) => current,
                                Err(e) if e.retryable() => continue,
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            };

                            let native_increase =
                                current.confirmed.saturating_sub(initial_balance.confirmed);
                            let token_increase =
                                current.additional.iter().find_map(|(key, &cur)| {
                                    let init =
                                        initial_balance.additional.get(key).copied().unwrap_or(0);
                                    (cur > init).then_some((key.clone(), cur - init))
                                });
                            if native_increase == 0 && token_increase.is_none() {
                                continue;
                            }

                            let observed_value = token_increase
                                .as_ref()
                                .map(|(_, delta)| *delta)
                                .unwrap_or(native_increase);
                            if let Some(expected) = amount_to_watch {
                                if observed_value != expected {
                                    continue;
                                }
                            }

                            match provider.history_sync(&wallet_for_call, sync_limit).await {
                                Ok(_)
                                | Err(PayError::NotImplemented(_))
                                | Err(PayError::WalletNotFound(_)) => {}
                                Err(e) if e.retryable() => continue,
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            }

                            let recent = match provider
                                .history_list(&wallet_for_call, sync_limit, 0)
                                .await
                            {
                                Ok(items) => items,
                                Err(e) if e.retryable() => continue,
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            };

                            let mut matched: Option<HistoryRecord> = None;
                            let mut memo_lookup_error: Option<PayError> = None;
                            for item in recent.into_iter() {
                                if item.direction != Direction::Receive {
                                    continue;
                                }
                                if known_receive_ids.contains(&item.transaction_id) {
                                    continue;
                                }
                                if let Some(expected) = amount_to_watch {
                                    if item.amount.value != expected {
                                        continue;
                                    }
                                }
                                if let Some(expected_token) = token_to_watch.as_deref() {
                                    if !evm_receive_token_matches(
                                        expected_token,
                                        &item.amount.token,
                                    ) {
                                        continue;
                                    }
                                }
                                if let Some(expected_memo) = memo_to_watch.as_deref() {
                                    let mut memo_matches =
                                        item.onchain_memo.as_deref() == Some(expected_memo);
                                    if !memo_matches {
                                        match provider
                                            .history_onchain_memo(
                                                &wallet_for_call,
                                                &item.transaction_id,
                                            )
                                            .await
                                        {
                                            Ok(Some(chain_memo)) => {
                                                memo_matches = chain_memo == expected_memo;
                                            }
                                            Ok(None)
                                            | Err(PayError::NotImplemented(_))
                                            | Err(PayError::WalletNotFound(_)) => {}
                                            Err(e) if e.retryable() => continue 'evm_wait,
                                            Err(e) => {
                                                memo_lookup_error = Some(e);
                                                break;
                                            }
                                        }
                                    }
                                    if !memo_matches {
                                        continue;
                                    }
                                }
                                matched = Some(item);
                                break;
                            }
                            if let Some(e) = memo_lookup_error {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                break;
                            }
                            let Some(item) = matched else {
                                continue;
                            };

                            known_receive_ids.insert(item.transaction_id.clone());
                            if let Some(min_conf) = min_confirmations {
                                loop {
                                    match provider.history_status(&item.transaction_id).await {
                                        Ok(status_info) => {
                                            let confs =
                                                status_info.confirmations.unwrap_or_else(|| {
                                                    if status_info.status == TxStatus::Confirmed {
                                                        min_conf
                                                    } else {
                                                        0
                                                    }
                                                });
                                            if confs >= min_conf {
                                                let _ = app
                                                    .writer
                                                    .send(Output::HistoryStatus {
                                                        id,
                                                        transaction_id: status_info.transaction_id,
                                                        status: status_info.status,
                                                        confirmations: Some(confs),
                                                        preimage: status_info.preimage,
                                                        item: status_info.item.or(Some(item)),
                                                        trace: trace_from(start),
                                                    })
                                                    .await;
                                                break 'evm_wait;
                                            }
                                            if Instant::now() >= deadline {
                                                emit_error(
                                                    &app.writer,
                                                    Some(id),
                                                    &PayError::NetworkError(format!(
                                                        "wait timeout after {timeout_secs}s: evm transaction {tx} matching {wait_criteria} has {confs}/{min_conf} confirmations",
                                                        tx = item.transaction_id
                                                    )),
                                                    start,
                                                )
                                                .await;
                                                break 'evm_wait;
                                            }
                                            sleep(Duration::from_millis(poll_interval_ms)).await;
                                        }
                                        Err(e) if e.retryable() => {
                                            if Instant::now() >= deadline {
                                                emit_error(&app.writer, Some(id), &e, start).await;
                                                break 'evm_wait;
                                            }
                                            sleep(Duration::from_millis(poll_interval_ms)).await;
                                        }
                                        Err(e) => {
                                            emit_error(&app.writer, Some(id), &e, start).await;
                                            break 'evm_wait;
                                        }
                                    }
                                }
                            } else {
                                match provider.history_status(&item.transaction_id).await {
                                    Ok(status_info) => {
                                        let _ = app
                                            .writer
                                            .send(Output::HistoryStatus {
                                                id,
                                                transaction_id: status_info.transaction_id,
                                                status: status_info.status,
                                                confirmations: status_info.confirmations,
                                                preimage: status_info.preimage,
                                                item: status_info.item.or(Some(item)),
                                                trace: trace_from(start),
                                            })
                                            .await;
                                        break;
                                    }
                                    Err(e) if e.retryable() => continue,
                                    Err(e) => {
                                        emit_error(&app.writer, Some(id), &e, start).await;
                                        break;
                                    }
                                }
                            }
                        }
                        return;
                    }

                    // BTC: poll balance deltas, then resolve matched on-chain txid from history.
                    if target_network == Network::Btc {
                        let amount_to_watch = amount.as_ref().map(|a| a.value).filter(|v| *v > 0);
                        let mut known_receive_ids: HashSet<String> =
                            match provider.history_list(&wallet_for_call, 1000, 0).await {
                                Ok(items) => items
                                    .into_iter()
                                    .filter(|item| item.direction == Direction::Receive)
                                    .map(|item| item.transaction_id)
                                    .collect(),
                                Err(_) => HashSet::new(),
                            };
                        let initial_balance = match provider.balance(&wallet_for_call).await {
                            Ok(b) => b,
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        };

                        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                        loop {
                            sleep(Duration::from_millis(poll_interval_ms)).await;
                            if Instant::now() >= deadline {
                                let criteria = if let Some(expected) = amount_to_watch {
                                    format!("amount {expected}")
                                } else {
                                    "any incoming amount".to_string()
                                };
                                emit_error(
                                    &app.writer,
                                    Some(id),
                                    &PayError::NetworkError(format!(
                                        "wait timeout after {timeout_secs}s: no incoming btc transaction matching {criteria}"
                                    )),
                                    start,
                                )
                                .await;
                                break;
                            }

                            let current = match provider.balance(&wallet_for_call).await {
                                Ok(current) => current,
                                Err(e) if e.retryable() => continue,
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            };
                            let confirmed_delta =
                                current.confirmed.saturating_sub(initial_balance.confirmed);
                            let pending_delta =
                                current.pending.saturating_sub(initial_balance.pending);
                            let observed_delta = confirmed_delta.saturating_add(pending_delta);
                            if observed_delta == 0 {
                                continue;
                            }
                            if let Some(expected) = amount_to_watch {
                                if observed_delta != expected {
                                    continue;
                                }
                            }

                            match provider.history_sync(&wallet_for_call, sync_limit).await {
                                Ok(_)
                                | Err(PayError::NotImplemented(_))
                                | Err(PayError::WalletNotFound(_)) => {}
                                Err(e) if e.retryable() => continue,
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            }

                            let recent = match provider
                                .history_list(&wallet_for_call, sync_limit, 0)
                                .await
                            {
                                Ok(items) => items,
                                Err(e) if e.retryable() => continue,
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            };

                            let matched = recent.into_iter().find(|item| {
                                if item.direction != Direction::Receive {
                                    return false;
                                }
                                if known_receive_ids.contains(&item.transaction_id) {
                                    return false;
                                }
                                if let Some(expected) = amount_to_watch {
                                    if item.amount.value != expected {
                                        return false;
                                    }
                                }
                                true
                            });

                            let Some(item) = matched else {
                                continue;
                            };

                            known_receive_ids.insert(item.transaction_id.clone());
                            match provider.history_status(&item.transaction_id).await {
                                Ok(status_info) => {
                                    let _ = app
                                        .writer
                                        .send(Output::HistoryStatus {
                                            id,
                                            transaction_id: status_info.transaction_id,
                                            status: status_info.status,
                                            confirmations: status_info.confirmations,
                                            preimage: status_info.preimage,
                                            item: status_info.item.or(Some(item)),
                                            trace: trace_from(start),
                                        })
                                        .await;
                                    break;
                                }
                                Err(e) if e.retryable() => continue,
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            }
                        }
                        return;
                    }

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
            let meta = match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(m) => m,
                Err(e) => {
                    emit_error(&app.writer, Some(id), &e, start).await;
                    return;
                }
            };
            let Some(provider) = get_provider(&app.providers, meta.network) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(format!("no provider for {}", meta.network)),
                    start,
                )
                .await;
                return;
            };

            match provider.receive_claim(&wallet, &quote_id).await {
                Ok(claimed) => {
                    let _ = app
                        .writer
                        .send(Output::ReceiveClaimed {
                            id,
                            wallet,
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

            let (target_network, wallet_for_call) = if let Some(w) =
                wallet.filter(|w| !w.is_empty())
            {
                let meta = match require_store(app).and_then(|s| s.load_wallet_metadata(&w)) {
                    Ok(m) => m,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                if let Some(expected) = network {
                    if meta.network != expected {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "wallet {w} is {}, not {expected}",
                                meta.network
                            )),
                            start,
                        )
                        .await;
                        return;
                    }
                }
                (meta.network, w)
            } else {
                let wallets = match require_store(app).and_then(|s| s.list_wallet_metadata(network))
                {
                    Ok(v) => v,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };

                // For cashu with --cashu-mint or multiple wallets: filter by mint,
                // then select wallet with smallest sufficient balance.
                let is_cashu = matches!(network, Some(Network::Cashu));
                let filtered: Vec<_> = if is_cashu {
                    if let Some(ref mint_list) = mints {
                        wallets
                            .into_iter()
                            .filter(|w| {
                                w.mint_url.as_deref().is_some_and(|u| {
                                    let nu = u.trim().trim_end_matches('/');
                                    mint_list
                                        .iter()
                                        .any(|m| m.trim().trim_end_matches('/') == nu)
                                })
                            })
                            .collect()
                    } else {
                        wallets
                    }
                } else {
                    wallets
                };

                match filtered.len() {
                    0 => {
                        let msg = if mints.is_some() {
                            "no cashu wallet found matching --cashu-mint".to_string()
                        } else {
                            match network {
                                Some(network) => format!("no {network} wallet found"),
                                None => "no wallet found".to_string(),
                            }
                        };
                        emit_error(&app.writer, Some(id), &PayError::WalletNotFound(msg), start)
                            .await;
                        return;
                    }
                    1 => (filtered[0].network, filtered[0].id.clone()),
                    _ if is_cashu => {
                        // Multiple cashu wallets: select by smallest sufficient balance.
                        // Pass empty wallet to provider — it will use select_wallet_by_balance.
                        (Network::Cashu, String::new())
                    }
                    _ => {
                        let msg = match network {
                            Some(network) => {
                                format!("multiple {network} wallets found; pass --wallet")
                            }
                            None => "multiple wallets found; pass --wallet".to_string(),
                        };
                        emit_error(&app.writer, Some(id), &PayError::InvalidAmount(msg), start)
                            .await;
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

            // Build spend context (requires a quote for Send to know the amount)
            let spend_ctx = if app.enforce_limits {
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
                let spend_amount = quote.amount_native + quote.fee_estimate_native;
                let provider_key = require_store(app)
                    .and_then(|s| s.load_wallet_metadata(&quote.wallet))
                    .ok()
                    .map(|meta| wallet_provider_key(&meta))
                    .unwrap_or_else(|| target_network.to_string());
                SpendContext {
                    network: provider_key,
                    wallet: Some(quote.wallet.clone()),
                    amount_native: spend_amount,
                    token: extract_token_from_target(&to),
                }
            } else {
                SpendContext {
                    network: target_network.to_string(),
                    wallet: Some(wallet_for_call.clone()),
                    amount_native: 0,
                    token: None,
                }
            };

            let result = with_spend_reserve(app, &id, "send", spend_ctx, start, || {
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
