use crate::provider::{PayError, PayProvider};
use crate::types::*;
use async_trait::async_trait;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use super::helpers::{emit_error, emit_error_hint, evm_receive_token_matches, trace_from};
use super::App;

const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_WAIT_POLL_INTERVAL_MS: u64 = 1000;
const DEFAULT_WAIT_SYNC_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ReceiveWaitOptions {
    pub(crate) timeout_secs: u64,
    pub(crate) poll_interval_ms: u64,
    pub(crate) sync_limit: usize,
    pub(crate) min_confirmations: Option<u32>,
}

impl ReceiveWaitOptions {
    pub(crate) fn from_input(
        wait_timeout_s: Option<u64>,
        wait_poll_interval_ms: Option<u64>,
        wait_sync_limit: Option<usize>,
        min_confirmations: Option<u32>,
    ) -> Result<Self, PayError> {
        let timeout_secs = wait_timeout_s.unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS);
        if timeout_secs == 0 {
            return Err(PayError::InvalidAmount(
                "wait_timeout_s must be >= 1".to_string(),
            ));
        }

        let poll_interval_ms = wait_poll_interval_ms.unwrap_or(DEFAULT_WAIT_POLL_INTERVAL_MS);
        if poll_interval_ms == 0 {
            return Err(PayError::InvalidAmount(
                "wait_poll_interval_ms must be >= 1".to_string(),
            ));
        }

        Ok(Self {
            timeout_secs,
            poll_interval_ms,
            sync_limit: wait_sync_limit
                .unwrap_or(DEFAULT_WAIT_SYNC_LIMIT)
                .clamp(1, 5000),
            min_confirmations,
        })
    }

    fn deadline(self) -> Instant {
        Instant::now() + Duration::from_secs(self.timeout_secs)
    }

    fn poll_interval(self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }
}

pub(crate) struct ReceiveWatchRequest<'a> {
    pub(crate) app: &'a App,
    pub(crate) provider: &'a dyn PayProvider,
    pub(crate) id: String,
    pub(crate) wallet: String,
    pub(crate) amount: Option<Amount>,
    pub(crate) onchain_memo: Option<String>,
    pub(crate) reference: Option<String>,
    pub(crate) options: ReceiveWaitOptions,
    pub(crate) start: Instant,
}

pub(crate) fn supports_onchain_receive_wait(network: Network) -> bool {
    matches!(network, Network::Sol | Network::Evm | Network::Btc)
}

pub(crate) async fn wait_onchain_receive(network: Network, req: ReceiveWatchRequest<'_>) {
    match network {
        Network::Sol => SolReceiveWatcher.wait(req).await,
        Network::Evm => EvmReceiveWatcher.wait(req).await,
        Network::Btc => BtcReceiveWatcher.wait(req).await,
        Network::Ln | Network::Cashu => {}
    }
}

#[async_trait]
pub(crate) trait ReceiveWatcher {
    async fn wait(&self, req: ReceiveWatchRequest<'_>);
}

struct SolReceiveWatcher;
struct EvmReceiveWatcher;
struct BtcReceiveWatcher;

#[async_trait]
impl ReceiveWatcher for SolReceiveWatcher {
    async fn wait(&self, req: ReceiveWatchRequest<'_>) {
        let req = &req;
        let memo_to_watch = trim_non_empty(req.onchain_memo.as_deref());
        let amount_to_watch = req.amount.as_ref().map(|a| a.value);
        let reference_to_watch = req.reference.as_deref().map(str::to_owned);

        if memo_to_watch.is_none() && amount_to_watch.is_none() && reference_to_watch.is_none() {
            emit_error_hint(
                &req.app.writer,
                Some(req.id.clone()),
                &PayError::InvalidAmount(
                    "sol receive --wait requires a match condition".to_string(),
                ),
                req.start,
                Some("pass --onchain-memo, --amount, or --reference"),
            )
            .await;
            return;
        }

        let deadline = req.options.deadline();
        loop {
            match req.provider.history_list(&req.wallet, 200, 0).await {
                Ok(items) => {
                    let matched = items.into_iter().find(|item| {
                        sol_item_matches(
                            item,
                            memo_to_watch.as_deref(),
                            amount_to_watch,
                            reference_to_watch.as_deref(),
                        )
                    });
                    if let Some(item) = matched {
                        let criteria = sol_criteria(
                            memo_to_watch.as_deref(),
                            amount_to_watch,
                            reference_to_watch.as_deref(),
                        );
                        if let Some(min_conf) = req.options.min_confirmations {
                            wait_for_min_confirmations(
                                req, item, min_conf, "sol", &criteria, deadline,
                            )
                            .await;
                        } else {
                            let transaction_id = item.transaction_id.clone();
                            let _ = req
                                .app
                                .writer
                                .send(Output::HistoryStatus {
                                    id: req.id.clone(),
                                    transaction_id,
                                    status: item.status,
                                    confirmations: None,
                                    preimage: item.preimage.clone(),
                                    item: Some(item),
                                    trace: trace_from(req.start),
                                })
                                .await;
                        }
                        break;
                    }

                    if Instant::now() >= deadline {
                        emit_no_incoming_timeout(
                            req,
                            "sol transaction",
                            &sol_criteria(
                                memo_to_watch.as_deref(),
                                amount_to_watch,
                                reference_to_watch.as_deref(),
                            ),
                        )
                        .await;
                        break;
                    }
                    sleep(req.options.poll_interval()).await;
                }
                Err(e) if e.retryable() => {
                    if Instant::now() >= deadline {
                        emit_no_incoming_timeout(
                            req,
                            "sol transaction",
                            &sol_criteria(
                                memo_to_watch.as_deref(),
                                amount_to_watch,
                                reference_to_watch.as_deref(),
                            ),
                        )
                        .await;
                        break;
                    }
                    sleep(req.options.poll_interval()).await;
                }
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
            }
        }
    }
}

#[async_trait]
impl ReceiveWatcher for EvmReceiveWatcher {
    async fn wait(&self, req: ReceiveWatchRequest<'_>) {
        let req = &req;
        let memo_to_watch = trim_non_empty(req.onchain_memo.as_deref());
        let amount_to_watch = req.amount.as_ref().map(|a| a.value);
        let token_to_watch = req.amount.as_ref().map(|a| a.token.to_ascii_lowercase());

        if amount_to_watch.is_none() {
            emit_error_hint(
                &req.app.writer,
                Some(req.id.clone()),
                &PayError::InvalidAmount("evm receive --wait requires --amount".to_string()),
                req.start,
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

        let mut known_receive_ids = known_receive_ids(req.provider, &req.wallet).await;
        let initial_balance = match req.provider.balance(&req.wallet).await {
            Ok(balance) => balance,
            Err(e) => {
                emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                return;
            }
        };

        let deadline = req.options.deadline();
        'evm_wait: loop {
            sleep(req.options.poll_interval()).await;
            if Instant::now() >= deadline {
                emit_wait_timeout(
                    req,
                    format!(
                        "wait timeout after {}s: no incoming evm deposit matching {wait_criteria}",
                        req.options.timeout_secs
                    ),
                )
                .await;
                break;
            }

            let current = match req.provider.balance(&req.wallet).await {
                Ok(current) => current,
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
            };

            let native_increase = current.confirmed.saturating_sub(initial_balance.confirmed);
            let token_increase = current.additional.iter().find_map(|(key, &cur)| {
                let init = initial_balance.additional.get(key).copied().unwrap_or(0);
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

            match sync_history(req.provider, &req.wallet, req.options.sync_limit).await {
                Ok(()) => {}
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
            }

            let recent = match req
                .provider
                .history_list(&req.wallet, req.options.sync_limit, 0)
                .await
            {
                Ok(items) => items,
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
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
                    if !evm_receive_token_matches(expected_token, &item.amount.token) {
                        continue;
                    }
                }
                if let Some(expected_memo) = memo_to_watch.as_deref() {
                    let mut memo_matches = item.onchain_memo.as_deref() == Some(expected_memo);
                    if !memo_matches {
                        match req
                            .provider
                            .history_onchain_memo(&req.wallet, &item.transaction_id)
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
                emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                break;
            }
            let Some(item) = matched else {
                continue;
            };

            known_receive_ids.insert(item.transaction_id.clone());
            if let Some(min_conf) = req.options.min_confirmations {
                wait_for_min_confirmations(req, item, min_conf, "evm", &wait_criteria, deadline)
                    .await;
                break;
            }

            match req.provider.history_status(&item.transaction_id).await {
                Ok(status_info) => {
                    emit_status_info(req, status_info, Some(item)).await;
                    break;
                }
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
            }
        }
    }
}

#[async_trait]
impl ReceiveWatcher for BtcReceiveWatcher {
    async fn wait(&self, req: ReceiveWatchRequest<'_>) {
        let req = &req;
        let amount_to_watch = req.amount.as_ref().map(|a| a.value).filter(|v| *v > 0);
        let mut known_receive_ids = known_receive_ids(req.provider, &req.wallet).await;
        let initial_balance = match req.provider.balance(&req.wallet).await {
            Ok(balance) => balance,
            Err(e) => {
                emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                return;
            }
        };

        let deadline = req.options.deadline();
        loop {
            sleep(req.options.poll_interval()).await;
            if Instant::now() >= deadline {
                let criteria = btc_criteria(amount_to_watch);
                emit_wait_timeout(
                    req,
                    format!(
                        "wait timeout after {}s: no incoming btc transaction matching {criteria}",
                        req.options.timeout_secs
                    ),
                )
                .await;
                break;
            }

            let current = match req.provider.balance(&req.wallet).await {
                Ok(current) => current,
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
            };
            let confirmed_delta = current.confirmed.saturating_sub(initial_balance.confirmed);
            let pending_delta = current.pending.saturating_sub(initial_balance.pending);
            let observed_delta = confirmed_delta.saturating_add(pending_delta);
            if observed_delta == 0 {
                continue;
            }
            if let Some(expected) = amount_to_watch {
                if observed_delta != expected {
                    continue;
                }
            }

            match sync_history(req.provider, &req.wallet, req.options.sync_limit).await {
                Ok(()) => {}
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
            }

            let recent = match req
                .provider
                .history_list(&req.wallet, req.options.sync_limit, 0)
                .await
            {
                Ok(items) => items,
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
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
            match req.provider.history_status(&item.transaction_id).await {
                Ok(status_info) => {
                    emit_status_info(req, status_info, Some(item)).await;
                    break;
                }
                Err(e) if e.retryable() => continue,
                Err(e) => {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
            }
        }
    }
}

fn trim_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn sol_item_matches(
    item: &HistoryRecord,
    memo_to_watch: Option<&str>,
    amount_to_watch: Option<u64>,
    reference_to_watch: Option<&str>,
) -> bool {
    if item.direction != Direction::Receive {
        return false;
    }
    if let Some(reference) = reference_to_watch {
        let has_ref = item
            .reference_keys
            .as_ref()
            .is_some_and(|keys| keys.iter().any(|key| key == reference));
        if !has_ref {
            return false;
        }
    }
    if let Some(memo) = memo_to_watch {
        item.onchain_memo.as_deref() == Some(memo)
    } else if let Some(expected) = amount_to_watch {
        item.amount.value == expected
    } else {
        reference_to_watch.is_some()
    }
}

fn sol_criteria(
    memo_to_watch: Option<&str>,
    amount_to_watch: Option<u64>,
    reference_to_watch: Option<&str>,
) -> String {
    if let Some(memo) = memo_to_watch {
        format!("memo '{memo}'")
    } else if let Some(expected) = amount_to_watch {
        format!("amount {expected}")
    } else if let Some(reference) = reference_to_watch {
        format!("reference '{reference}'")
    } else {
        "unknown".to_string()
    }
}

fn btc_criteria(amount_to_watch: Option<u64>) -> String {
    if let Some(expected) = amount_to_watch {
        format!("amount {expected}")
    } else {
        "any incoming amount".to_string()
    }
}

async fn known_receive_ids(provider: &dyn PayProvider, wallet: &str) -> HashSet<String> {
    match provider.history_list(wallet, 1000, 0).await {
        Ok(items) => items
            .into_iter()
            .filter(|item| item.direction == Direction::Receive)
            .map(|item| item.transaction_id)
            .collect(),
        Err(_) => HashSet::new(),
    }
}

async fn sync_history(
    provider: &dyn PayProvider,
    wallet: &str,
    sync_limit: usize,
) -> Result<(), PayError> {
    match provider.history_sync(wallet, sync_limit).await {
        Ok(_) | Err(PayError::NotImplemented(_)) | Err(PayError::WalletNotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

async fn wait_for_min_confirmations(
    req: &ReceiveWatchRequest<'_>,
    item: HistoryRecord,
    min_conf: u32,
    network_label: &str,
    criteria: &str,
    deadline: Instant,
) {
    loop {
        match req.provider.history_status(&item.transaction_id).await {
            Ok(status_info) => {
                let confs = confirmation_count(&status_info, min_conf);
                if confs >= min_conf {
                    emit_status_info_with_confirmations(
                        req,
                        status_info,
                        Some(item.clone()),
                        confs,
                    )
                    .await;
                    break;
                }
                if Instant::now() >= deadline {
                    emit_wait_timeout(
                        req,
                        format!(
                            "wait timeout after {}s: {network_label} transaction {tx} matching {criteria} has {confs}/{min_conf} confirmations",
                            req.options.timeout_secs,
                            tx = item.transaction_id
                        ),
                    )
                    .await;
                    break;
                }
                sleep(req.options.poll_interval()).await;
            }
            Err(e) if e.retryable() => {
                if Instant::now() >= deadline {
                    emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                    break;
                }
                sleep(req.options.poll_interval()).await;
            }
            Err(e) => {
                emit_error(&req.app.writer, Some(req.id.clone()), &e, req.start).await;
                break;
            }
        }
    }
}

fn confirmation_count(status_info: &HistoryStatusInfo, min_conf: u32) -> u32 {
    status_info.confirmations.unwrap_or_else(|| {
        if status_info.status == TxStatus::Confirmed {
            min_conf
        } else {
            0
        }
    })
}

async fn emit_status_info(
    req: &ReceiveWatchRequest<'_>,
    status_info: HistoryStatusInfo,
    fallback_item: Option<HistoryRecord>,
) {
    let _ = req
        .app
        .writer
        .send(Output::HistoryStatus {
            id: req.id.clone(),
            transaction_id: status_info.transaction_id,
            status: status_info.status,
            confirmations: status_info.confirmations,
            preimage: status_info.preimage,
            item: status_info.item.or(fallback_item),
            trace: trace_from(req.start),
        })
        .await;
}

async fn emit_status_info_with_confirmations(
    req: &ReceiveWatchRequest<'_>,
    status_info: HistoryStatusInfo,
    fallback_item: Option<HistoryRecord>,
    confirmations: u32,
) {
    let _ = req
        .app
        .writer
        .send(Output::HistoryStatus {
            id: req.id.clone(),
            transaction_id: status_info.transaction_id,
            status: status_info.status,
            confirmations: Some(confirmations),
            preimage: status_info.preimage,
            item: status_info.item.or(fallback_item),
            trace: trace_from(req.start),
        })
        .await;
}

async fn emit_no_incoming_timeout(req: &ReceiveWatchRequest<'_>, subject: &str, criteria: &str) {
    emit_wait_timeout(
        req,
        format!(
            "wait timeout after {}s: no incoming {subject} matching {criteria}",
            req.options.timeout_secs
        ),
    )
    .await;
}

async fn emit_wait_timeout(req: &ReceiveWatchRequest<'_>, message: String) {
    emit_error(
        &req.app.writer,
        Some(req.id.clone()),
        &PayError::NetworkError(message),
        req.start,
    )
    .await;
}
