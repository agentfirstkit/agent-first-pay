use crate::provider::{HistorySyncStats, PayError};
use crate::store::PayStore;
use crate::types::*;
use std::time::Instant;

use super::helpers::*;
use super::App;

pub(crate) async fn dispatch_history(app: &App, input: Input) {
    match input {
        Input::HistoryList {
            id,
            wallet,
            network,
            onchain_memo,
            limit,
            offset,
            since_epoch_s,
            until_epoch_s,
        } => {
            let start = Instant::now();
            let lim = limit.unwrap_or(20);
            let off = offset.unwrap_or(0);
            let memo_filter = onchain_memo
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let store = match require_store(app) {
                Ok(store) => store,
                Err(e) => {
                    emit_error(&app.writer, Some(id), &e, start).await;
                    return;
                }
            };

            let mut all_txs = Vec::new();
            if let Some(wallet_id) = wallet {
                let meta = match store.load_wallet_metadata(&wallet_id) {
                    Ok(meta) => meta,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                if let Some(expected_network) = network {
                    if meta.network != expected_network {
                        let _ = app
                            .writer
                            .send(Output::History {
                                id,
                                items: Vec::new(),
                                trace: trace_from(start),
                            })
                            .await;
                        return;
                    }
                }
                match store.load_wallet_transaction_records(&wallet_id) {
                    Ok(mut records) => all_txs.append(&mut records),
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                }
            } else {
                let wallets = match store.list_wallet_metadata(network) {
                    Ok(wallets) => wallets,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                for wallet_meta in wallets {
                    match store.load_wallet_transaction_records(&wallet_meta.id) {
                        Ok(mut records) => all_txs.append(&mut records),
                        Err(e) => {
                            emit_error(&app.writer, Some(id.clone()), &e, start).await;
                            return;
                        }
                    }
                }
            }

            if let Some(expected_network) = network {
                all_txs.retain(|item| item.network == expected_network);
            }
            if let Some(since) = since_epoch_s {
                all_txs.retain(|item| item.created_at_epoch_s >= since);
            }
            if let Some(until) = until_epoch_s {
                all_txs.retain(|item| item.created_at_epoch_s < until);
            }
            if let Some(filter) = memo_filter.as_deref() {
                all_txs.retain(|item| item.onchain_memo.as_deref() == Some(filter));
            }
            all_txs.sort_by(|a, b| b.created_at_epoch_s.cmp(&a.created_at_epoch_s));
            let start_idx = all_txs.len().min(off);
            let end_idx = all_txs.len().min(off.saturating_add(lim));
            let items = all_txs[start_idx..end_idx].to_vec();
            let _ = app
                .writer
                .send(Output::History {
                    id,
                    items,
                    trace: trace_from(start),
                })
                .await;
        }

        Input::HistoryStatus { id, transaction_id } => {
            let start = Instant::now();
            // Resolve the transaction's network from local store, then route
            // to the specific provider. No fallback — if the transaction isn't
            // in the local store, it doesn't exist.
            let routed = match require_store(app).and_then(|s| {
                s.find_transaction_record_by_id(&transaction_id)
                    .map(|opt| opt.map(|r| r.network))
            }) {
                Ok(Some(network)) => match app.providers.get(&network) {
                    Some(provider) => provider.history_status(&transaction_id).await,
                    None => Err(PayError::NotImplemented(format!(
                        "no provider for {network}"
                    ))),
                },
                _ => Err(PayError::WalletNotFound(format!(
                    "transaction {transaction_id} not found"
                ))),
            };
            match routed {
                Ok(info) => {
                    let _ = app
                        .writer
                        .send(Output::HistoryStatus {
                            id,
                            transaction_id: info.transaction_id,
                            status: info.status,
                            confirmations: info.confirmations,
                            preimage: info.preimage,
                            item: info.item,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::HistoryUpdate {
            id,
            wallet,
            network,
            limit,
        } => {
            let start = Instant::now();
            if let Err(e) = require_store(app) {
                emit_error(&app.writer, Some(id), &e, start).await;
                return;
            }

            let sync_limit = limit.unwrap_or(200).clamp(1, 5000);
            let mut totals = HistorySyncStats::default();
            let mut wallets_synced = 0usize;

            if let Some(wallet_id) = wallet {
                let sync_result = if let Some(expected_network) = network {
                    match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet_id)) {
                        Ok(meta) if meta.network != expected_network => {
                            Err(PayError::InvalidAmount(format!(
                                "wallet {wallet_id} belongs to {}, not {expected_network}",
                                meta.network
                            )))
                        }
                        Ok(_) => match get_provider(&app.providers, expected_network) {
                            Some(provider) => provider.history_sync(&wallet_id, sync_limit).await,
                            None => Err(PayError::NotImplemented(format!(
                                "network {expected_network} not enabled"
                            ))),
                        },
                        Err(e) => Err(e),
                    }
                } else {
                    match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet_id)) {
                        Ok(meta) => match get_provider(&app.providers, meta.network) {
                            Some(provider) => provider.history_sync(&wallet_id, sync_limit).await,
                            None => Err(PayError::NotImplemented(format!(
                                "network {} not enabled",
                                meta.network
                            ))),
                        },
                        Err(e) => Err(e),
                    }
                };

                match sync_result {
                    Ok(stats) => {
                        wallets_synced = 1;
                        totals.records_scanned =
                            totals.records_scanned.saturating_add(stats.records_scanned);
                        totals.records_added =
                            totals.records_added.saturating_add(stats.records_added);
                        totals.records_updated =
                            totals.records_updated.saturating_add(stats.records_updated);
                    }
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                }
            } else {
                let target_networks: Vec<Network> = if let Some(single) = network {
                    vec![single]
                } else {
                    vec![
                        Network::Cashu,
                        Network::Ln,
                        Network::Sol,
                        Network::Evm,
                        Network::Btc,
                    ]
                };

                let wallets = match require_store(app).and_then(|s| s.list_wallet_metadata(None)) {
                    Ok(wallets) => wallets,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };

                for network_key in target_networks {
                    let Some(provider) = get_provider(&app.providers, network_key) else {
                        if network.is_some() {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::NotImplemented(format!(
                                    "network {network_key} not enabled"
                                )),
                                start,
                            )
                            .await;
                            return;
                        }
                        continue;
                    };
                    for wallet_meta in &wallets {
                        if wallet_meta.network != network_key {
                            continue;
                        }
                        match provider.history_sync(&wallet_meta.id, sync_limit).await {
                            Ok(stats) => {
                                wallets_synced = wallets_synced.saturating_add(1);
                                totals.records_scanned =
                                    totals.records_scanned.saturating_add(stats.records_scanned);
                                totals.records_added =
                                    totals.records_added.saturating_add(stats.records_added);
                                totals.records_updated =
                                    totals.records_updated.saturating_add(stats.records_updated);
                            }
                            Err(PayError::NotImplemented(_)) | Err(PayError::WalletNotFound(_)) => {
                            }
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        }
                    }
                }
            }

            let _ = app
                .writer
                .send(Output::HistoryUpdated {
                    id,
                    wallets_synced,
                    records_scanned: totals.records_scanned,
                    records_added: totals.records_added,
                    records_updated: totals.records_updated,
                    trace: trace_from(start),
                })
                .await;
        }

        _ => {}
    }
}
