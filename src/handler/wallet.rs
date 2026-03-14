use crate::provider::PayError;
use crate::store::wallet;
#[cfg(feature = "redb")]
use crate::store::wallet::load_wallet_metadata;
use crate::store::PayStore;
use crate::types::*;
use std::time::Instant;

use super::helpers::*;
use super::App;

pub(crate) async fn dispatch_wallet(app: &App, input: Input) {
    match input {
        Input::WalletCreate {
            id,
            network,
            label,
            mint_url,
            rpc_endpoints,
            chain_id,
            mnemonic_secret,
            btc_esplora_url,
            btc_network,
            btc_address_type,
            btc_backend,
            btc_core_url,
            btc_core_auth_secret,
            btc_electrum_url,
        } => {
            let start = Instant::now();
            let mut log_args = serde_json::json!({
                "operation": "wallet_create",
                "network": network.to_string(),
                "label": label.as_deref().unwrap_or("default"),
            });
            if let Some(object) = log_args.as_object_mut() {
                if !rpc_endpoints.is_empty() {
                    object.insert(
                        "rpc_endpoints".to_string(),
                        serde_json::json!(rpc_endpoints),
                    );
                }
                if let Some(cid) = chain_id {
                    object.insert("chain_id".to_string(), serde_json::json!(cid));
                }
                if let Some(url) = mint_url.as_deref() {
                    object.insert("mint_url".to_string(), serde_json::json!(url));
                }
                object.insert(
                    "use_recovery_mnemonic".to_string(),
                    serde_json::json!(mnemonic_secret.is_some()),
                );
            }
            emit_log(app, "wallet", Some(id.clone()), log_args).await;
            let request = WalletCreateRequest {
                label: label.unwrap_or_else(|| "default".to_string()),
                mint_url,
                rpc_endpoints,
                chain_id,
                mnemonic_secret,
                btc_esplora_url,
                btc_network,
                btc_address_type,
                btc_backend,
                btc_core_url,
                btc_core_auth_secret,
                btc_electrum_url,
            };
            match get_provider(&app.providers, network) {
                Some(p) => match p.create_wallet(&request).await {
                    Ok(info) => {
                        // Sync wallet metadata to store (for non-redb backends).
                        #[cfg(feature = "redb")]
                        if let Some(store) = app.store.as_ref() {
                            if let Ok(meta) =
                                load_wallet_metadata(&app.config.read().await.data_dir, &info.id)
                            {
                                let _ = store.save_wallet_metadata(&meta);
                            }
                        }
                        let _ = app
                            .writer
                            .send(Output::WalletCreated {
                                id,
                                wallet: info.id,
                                network: info.network,
                                address: info.address,
                                mnemonic: None,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                },
                None => {
                    emit_error(
                        &app.writer,
                        Some(id),
                        &PayError::NotImplemented(format!("no provider for {network}")),
                        start,
                    )
                    .await;
                }
            }
        }

        Input::LnWalletCreate { id, request } => {
            let start = Instant::now();
            let mut log_args =
                serde_json::to_value(&request).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(object) = log_args.as_object_mut() {
                object.insert(
                    "operation".to_string(),
                    serde_json::json!("ln_wallet_create"),
                );
                object.insert("network".to_string(), serde_json::json!("ln"));
            }
            emit_log(app, "wallet", Some(id.clone()), log_args).await;

            match get_provider(&app.providers, Network::Ln) {
                Some(p) => match p.create_ln_wallet(request).await {
                    Ok(info) => {
                        #[cfg(feature = "redb")]
                        if let Some(store) = app.store.as_ref() {
                            if let Ok(meta) =
                                load_wallet_metadata(&app.config.read().await.data_dir, &info.id)
                            {
                                let _ = store.save_wallet_metadata(&meta);
                            }
                        }
                        let _ = app
                            .writer
                            .send(Output::WalletCreated {
                                id,
                                wallet: info.id,
                                network: info.network,
                                address: info.address,
                                mnemonic: None,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                },
                None => {
                    emit_error(
                        &app.writer,
                        Some(id),
                        &PayError::NotImplemented("no provider for ln".to_string()),
                        start,
                    )
                    .await;
                }
            }
        }

        Input::WalletClose {
            id,
            wallet,
            dangerously_skip_balance_check_and_may_lose_money,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "wallet_close",
                    "wallet": &wallet,
                    "dangerously_skip_balance_check_and_may_lose_money": dangerously_skip_balance_check_and_may_lose_money,
                }),
            )
            .await;
            let close_result = if dangerously_skip_balance_check_and_may_lose_money {
                match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                    Ok(_) => require_store(app).and_then(|s| s.delete_wallet_metadata(&wallet)).map(|_| ()),
                    Err(PayError::WalletNotFound(_)) => Err(PayError::WalletNotFound(format!(
                        "wallet {wallet} not found locally; dangerous skip balance check only supports local wallets"
                    ))),
                    Err(error) => Err(error),
                }
            } else {
                match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                    Ok(meta) => match get_provider(&app.providers, meta.network) {
                        Some(provider) => provider.close_wallet(&wallet).await,
                        None => Err(PayError::NotImplemented(format!(
                            "no provider for {}",
                            meta.network
                        ))),
                    },
                    Err(PayError::WalletNotFound(_)) => {
                        // Fallback for remote-only deployments where wallets may not be stored locally.
                        try_provider!(&app.providers, |p| p.close_wallet(&wallet))
                    }
                    Err(error) => Err(error),
                }
            };

            match close_result {
                Ok(()) => {
                    let _ = app
                        .writer
                        .send(Output::WalletClosed {
                            id,
                            wallet,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletList { id, network } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "wallet_list",
                    "network": network.map(|c| c.to_string()).unwrap_or_else(|| "all".to_string()),
                }),
            )
            .await;
            if let Some(network) = network {
                match get_provider(&app.providers, network) {
                    Some(p) => match p.list_wallets().await {
                        Ok(wallets) => {
                            let _ = app
                                .writer
                                .send(Output::WalletList {
                                    id,
                                    wallets,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    },
                    None => {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::NotImplemented(format!("no provider for {network}")),
                            start,
                        )
                        .await;
                    }
                }
            } else {
                let wallets = collect_all!(&app.providers, |p| p.list_wallets());
                match wallets {
                    Ok(all) => {
                        let _ = app
                            .writer
                            .send(Output::WalletList {
                                id,
                                wallets: all,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                }
            }
        }

        Input::Balance {
            id,
            wallet,
            network,
            check,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "balance", "wallet": wallet.as_deref().unwrap_or("all"), "check": check,
                }),
            )
            .await;
            if let Some(wallet_id) = wallet {
                let meta_opt = require_store(app)
                    .and_then(|s| s.load_wallet_metadata(&wallet_id))
                    .ok();
                let result = if let Some(ref meta) = meta_opt {
                    match get_provider(&app.providers, meta.network) {
                        Some(provider) => {
                            if check {
                                match provider.check_balance(&wallet_id).await {
                                    Err(PayError::NotImplemented(_)) => {
                                        provider.balance(&wallet_id).await
                                    }
                                    other => other,
                                }
                            } else {
                                provider.balance(&wallet_id).await
                            }
                        }
                        None => Err(PayError::NotImplemented(format!(
                            "no provider for {}",
                            meta.network
                        ))),
                    }
                } else {
                    // Remote-only fallback: wallet metadata may not exist locally.
                    if check {
                        try_provider!(&app.providers, |p| async {
                            match p.check_balance(&wallet_id).await {
                                Err(PayError::NotImplemented(_)) => p.balance(&wallet_id).await,
                                other => other,
                            }
                        })
                    } else {
                        try_provider!(&app.providers, |p| p.balance(&wallet_id))
                    }
                };
                match result {
                    Ok(balance) => {
                        let summary = if let Some(meta) = meta_opt {
                            wallet_summary_from_meta(&meta, &wallet_id)
                        } else {
                            resolve_wallet_summary(app, &wallet_id).await
                        };
                        let _ = app
                            .writer
                            .send(Output::WalletBalances {
                                id,
                                wallets: vec![WalletBalanceItem {
                                    wallet: summary,
                                    balance: Some(balance),
                                    error: None,
                                }],
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                }
            } else {
                match collect_all!(&app.providers, |p| p.balance_all()) {
                    Ok(wallets) => {
                        let filtered = if let Some(network) = network {
                            wallets
                                .into_iter()
                                .filter(|w| w.wallet.network == network)
                                .collect()
                        } else {
                            wallets
                        };
                        let _ = app
                            .writer
                            .send(Output::WalletBalances {
                                id,
                                wallets: filtered,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                }
            }
        }

        Input::Restore { id, wallet } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "restore", "wallet": &wallet,
                }),
            )
            .await;
            match try_provider!(&app.providers, |p| p.restore(&wallet)) {
                Ok(r) => {
                    let _ = app
                        .writer
                        .send(Output::Restored {
                            id,
                            wallet: r.wallet,
                            unspent: r.unspent,
                            spent: r.spent,
                            pending: r.pending,
                            unit: r.unit,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletShowSeed { id, wallet } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "wallet_show_seed", "wallet": &wallet,
                }),
            )
            .await;
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(meta) => match meta.network {
                    Network::Cashu => match meta.seed_secret {
                        Some(mnemonic) => {
                            let _ = app
                                .writer
                                .send(Output::WalletSeed {
                                    id,
                                    wallet,
                                    mnemonic_secret: mnemonic,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        None => {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InternalError("wallet has no seed".to_string()),
                                start,
                            )
                            .await;
                        }
                    },
                    Network::Sol => match meta.seed_secret {
                        Some(secret) => {
                            if looks_like_bip39_mnemonic(&secret) {
                                let _ = app
                                    .writer
                                    .send(Output::WalletSeed {
                                        id,
                                        wallet,
                                        mnemonic_secret: secret,
                                        trace: trace_from(start),
                                    })
                                    .await;
                            } else {
                                emit_error(
                                        &app.writer,
                                        Some(id),
                                        &PayError::InvalidAmount(
                                            "this sol wallet was created before mnemonic support; create a new sol wallet to get 12-word backup".to_string(),
                                        ),
                                        start,
                                    )
                                    .await;
                            }
                        }
                        None => {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InternalError("wallet has no seed".to_string()),
                                start,
                            )
                            .await;
                        }
                    },
                    Network::Ln => {
                        emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "ln wallets do not have mnemonic words; they store backend credentials (nwc-uri/password/admin-key)".to_string(),
                                ),
                                start,
                            )
                            .await;
                    }
                    Network::Evm | Network::Btc => match meta.seed_secret {
                        Some(secret) => {
                            if looks_like_bip39_mnemonic(&secret) {
                                let _ = app
                                    .writer
                                    .send(Output::WalletSeed {
                                        id,
                                        wallet,
                                        mnemonic_secret: secret,
                                        trace: trace_from(start),
                                    })
                                    .await;
                            } else {
                                emit_error(
                                        &app.writer,
                                        Some(id),
                                        &PayError::InvalidAmount(
                                            "this wallet was created before mnemonic support; create a new wallet to get 12-word backup".to_string(),
                                        ),
                                        start,
                                    )
                                    .await;
                            }
                        }
                        None => {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InternalError("wallet has no seed".to_string()),
                                start,
                            )
                            .await;
                        }
                    },
                },
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigShow { id, wallet } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(meta) => {
                    let resolved_wallet = meta.id.clone();
                    let _ = app
                        .writer
                        .send(Output::WalletConfig {
                            id,
                            wallet: resolved_wallet,
                            config: meta,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigSet {
            id,
            wallet,
            label,
            rpc_endpoints,
            chain_id,
        } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(mut meta) => {
                    let resolved_wallet = meta.id.clone();
                    let mut changed = false;

                    if let Some(new_label) = label {
                        let trimmed = new_label.trim();
                        meta.label = if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        };
                        changed = true;
                    }

                    if !rpc_endpoints.is_empty() {
                        match meta.network {
                            Network::Sol => {
                                meta.sol_rpc_endpoints = Some(rpc_endpoints);
                                changed = true;
                            }
                            Network::Evm => {
                                meta.evm_rpc_endpoints = Some(rpc_endpoints);
                                changed = true;
                            }
                            _ => {
                                emit_error(
                                    &app.writer,
                                    Some(id),
                                    &PayError::InvalidAmount(format!(
                                        "rpc-endpoint not supported for {} wallets",
                                        meta.network
                                    )),
                                    start,
                                )
                                .await;
                                return;
                            }
                        }
                    }

                    if let Some(cid) = chain_id {
                        if meta.network != Network::Evm {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "chain-id is only supported for evm wallets".to_string(),
                                ),
                                start,
                            )
                            .await;
                            return;
                        }
                        meta.evm_chain_id = Some(cid);
                        changed = true;
                    }

                    if !changed {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(
                                "no configuration changes specified".to_string(),
                            ),
                            start,
                        )
                        .await;
                        return;
                    }

                    match require_store(app).and_then(|s| s.save_wallet_metadata(&meta)) {
                        Ok(()) => {
                            let _ = app
                                .writer
                                .send(Output::WalletConfigUpdated {
                                    id,
                                    wallet: resolved_wallet,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigTokenAdd {
            id,
            wallet,
            symbol,
            address,
            decimals,
        } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(mut meta) => {
                    let resolved_wallet = meta.id.clone();
                    if !matches!(meta.network, Network::Evm | Network::Sol) {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "custom tokens not supported for {} wallets",
                                meta.network
                            )),
                            start,
                        )
                        .await;
                        return;
                    }

                    let lower_symbol = symbol.to_ascii_lowercase();
                    let tokens = meta.custom_tokens.get_or_insert_with(Vec::new);
                    if tokens
                        .iter()
                        .any(|t| t.symbol.to_ascii_lowercase() == lower_symbol)
                    {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "custom token '{lower_symbol}' already registered"
                            )),
                            start,
                        )
                        .await;
                        return;
                    }

                    tokens.push(wallet::CustomToken {
                        symbol: lower_symbol.clone(),
                        address: address.clone(),
                        decimals,
                    });

                    match require_store(app).and_then(|s| s.save_wallet_metadata(&meta)) {
                        Ok(()) => {
                            let _ = app
                                .writer
                                .send(Output::WalletConfigTokenAdded {
                                    id,
                                    wallet: resolved_wallet,
                                    symbol: lower_symbol,
                                    address,
                                    decimals,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigTokenRemove { id, wallet, symbol } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(mut meta) => {
                    let resolved_wallet = meta.id.clone();
                    let lower_symbol = symbol.to_ascii_lowercase();
                    let tokens = meta.custom_tokens.get_or_insert_with(Vec::new);
                    let before_len = tokens.len();
                    tokens.retain(|t| t.symbol.to_ascii_lowercase() != lower_symbol);
                    if tokens.len() == before_len {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "custom token '{lower_symbol}' not found"
                            )),
                            start,
                        )
                        .await;
                        return;
                    }
                    if tokens.is_empty() {
                        meta.custom_tokens = None;
                    }

                    match require_store(app).and_then(|s| s.save_wallet_metadata(&meta)) {
                        Ok(()) => {
                            let _ = app
                                .writer
                                .send(Output::WalletConfigTokenRemoved {
                                    id,
                                    wallet: resolved_wallet,
                                    symbol: lower_symbol,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        _ => {}
    }
}
