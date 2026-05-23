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
            let mut all_txs = Vec::new();
            if let Some(wallet_id) = wallet.as_deref() {
                let mut loaded_local = false;
                if let Some(store) = app.store.as_deref() {
                    match store.load_wallet_metadata(wallet_id) {
                        Ok(meta) => {
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
                            match store.load_wallet_transaction_records(wallet_id) {
                                Ok(mut records) => {
                                    all_txs.append(&mut records);
                                    loaded_local = true;
                                }
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    return;
                                }
                            }
                        }
                        Err(PayError::WalletNotFound(_)) => {}
                        Err(e) => {
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    }
                }

                if !loaded_local {
                    let (target_network, wallet_for_call) =
                        match resolve_wallet_for_provider(app, Some(wallet_id), network).await {
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
                            &PayError::NotImplemented(format!(
                                "network {target_network} not enabled"
                            )),
                            start,
                        )
                        .await;
                        return;
                    };
                    let fetch_limit = off.saturating_add(lim).clamp(1, 5000);
                    match provider
                        .history_list(&wallet_for_call, fetch_limit, 0)
                        .await
                    {
                        Ok(mut records) => all_txs.append(&mut records),
                        Err(e) => {
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    }
                }
            } else {
                if let Some(store) = app.store.as_deref() {
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

                if all_txs.is_empty() {
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
                    let fetch_limit = off.saturating_add(lim).clamp(1, 5000);
                    for network_key in target_networks {
                        let Some(provider) = get_provider(&app.providers, network_key) else {
                            continue;
                        };
                        let wallets = match provider.list_wallets().await {
                            Ok(wallets) => wallets,
                            Err(PayError::NotImplemented(_)) | Err(PayError::WalletNotFound(_)) => {
                                continue;
                            }
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        };
                        for wallet in wallets {
                            match provider.history_list(&wallet.id, fetch_limit, 0).await {
                                Ok(mut records) => all_txs.append(&mut records),
                                Err(PayError::NotImplemented(_))
                                | Err(PayError::WalletNotFound(_)) => {}
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    return;
                                }
                            }
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
            all_txs.sort_by_key(|item| std::cmp::Reverse(item.created_at_epoch_s));
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
            // Prefer local transaction metadata, then fall back to downstream providers
            // for coordinator/remote-only deployments.
            let routed = match app.store.as_deref().and_then(|s| {
                s.find_transaction_record_by_id(&transaction_id)
                    .ok()
                    .flatten()
                    .map(|r| r.network)
            }) {
                Some(network) => match app.providers.get(&network) {
                    Some(provider) => provider.history_status(&transaction_id).await,
                    None => Err(PayError::NotImplemented(format!(
                        "no provider for {network}"
                    ))),
                },
                None => {
                    let mut found = None;
                    let mut first_error = None;
                    for provider in app.providers.values() {
                        match provider.history_status(&transaction_id).await {
                            Ok(info) => {
                                found = Some(info);
                                break;
                            }
                            Err(PayError::NotImplemented(_)) | Err(PayError::WalletNotFound(_)) => {
                            }
                            Err(e) => {
                                if first_error.is_none() {
                                    first_error = Some(e);
                                }
                            }
                        }
                    }
                    found.ok_or_else(|| {
                        first_error.unwrap_or_else(|| {
                            PayError::WalletNotFound(format!(
                                "transaction {transaction_id} not found"
                            ))
                        })
                    })
                }
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
            let sync_limit = limit.unwrap_or(200).clamp(1, 5000);
            let mut totals = HistorySyncStats::default();
            let mut wallets_synced = 0usize;

            if let Some(wallet_id) = wallet {
                let (target_network, wallet_for_call) =
                    match resolve_wallet_for_provider(app, Some(&wallet_id), network).await {
                        Ok(resolved) => resolved,
                        Err(e) => {
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    };
                let sync_result = match get_provider(&app.providers, target_network) {
                    Some(provider) => provider.history_sync(&wallet_for_call, sync_limit).await,
                    None => Err(PayError::NotImplemented(format!(
                        "network {target_network} not enabled"
                    ))),
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
                    let wallet_ids: Vec<String> = match app.store.as_deref() {
                        Some(store) => match store.list_wallet_metadata(Some(network_key)) {
                            Ok(wallets) => wallets.into_iter().map(|wallet| wallet.id).collect(),
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        },
                        None => Vec::new(),
                    };
                    let wallet_ids = if wallet_ids.is_empty() {
                        match provider.list_wallets().await {
                            Ok(wallets) => wallets.into_iter().map(|wallet| wallet.id).collect(),
                            Err(PayError::NotImplemented(_)) | Err(PayError::WalletNotFound(_)) => {
                                Vec::new()
                            }
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        }
                    } else {
                        wallet_ids
                    };
                    for wallet_id in wallet_ids {
                        match provider.history_sync(&wallet_id, sync_limit).await {
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
