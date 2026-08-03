/// Self-describing wire schema for the JSON Input/Output protocol.
///
/// Returned by the REST `/v1/schema` endpoint and by `Input::Schema` in every
/// other mode (pipe / rpc / cli) so an agent in any mode can discover the
/// operation set, field shapes, and error codes without scraping `--help`
/// or the source tree.
///
/// This builder is hand-maintained and intentionally minimal — there is no
/// schemars dependency. The
/// `wire_protocol_schema_listed_inputs_match_protocol_rs` test in
/// `mode/rest.rs` catches drift between this map and the `Input` enum.
pub fn wire_protocol_schema() -> serde_json::Value {
    serde_json::json!({
        "version": "v1",
        // Bump on every Input/Output/ErrorCode shape change so agents can pin
        // against a known set without re-fetching and diffing the whole doc.
        "schema_version": "2026-07-28.1",
        "envelope": {
            "description": "Every request is wrapped in a Request envelope. Plain Input JSON also works (dry_run defaults to false).",
            "shape": {
                "dry_run": "bool? — when true, validates without executing and emits Output::DryRun",
                "<flattened Input fields>": "the operation code and its parameters"
            }
        },
        "endpoints": {
            "POST /v1/afpay": "Submit a Request. Returns a JSON array of strict AFDATA events.",
            "GET /v1/schema": "This document.",
            "Input::Schema (pipe/rpc/cli)": "Same document, returned as Output::Schema."
        },
        "auth": "x-api-key header must match the --rest-api-key-secret the daemon was started with.",
        "inputs": [
            {"code": "version", "description": "Daemon version + uptime.", "fields": []},
            {"code": "schema", "description": "Return this self-describing wire schema.", "fields": []},
            {"code": "config_get", "description": "Get runtime config or a specific key.", "fields": ["id", "key?"]},
            {"code": "config_set", "description": "Set a runtime config key.", "fields": ["id", "key", "values?"]},
            {"code": "wallet_create", "description": "Create a wallet on the given network.", "fields": ["id", "network", "label?", "mint_url?", "rpc_endpoints?", "chain_id?", "mnemonic_secret?", "btc_esplora_url?", "btc_network?", "btc_address_type?", "btc_backend?", "btc_core_url?", "btc_core_auth_secret?", "btc_electrum_url?", "sol_cluster?"]},
            {"code": "ln_wallet_create", "description": "Create a Lightning wallet via NWC/phoenixd/lnbits.", "fields": ["id", "backend", "endpoint_url?", "nwc_uri_secret?", "password_secret?", "admin_key_secret?", "label?"]},
            {"code": "wallet_close", "description": "Close a wallet (refuses with non-zero balance unless overridden).", "fields": ["id", "wallet", "dangerously_skip_balance_check_and_may_lose_money"]},
            {"code": "wallet_list", "description": "List wallets, optionally filtered by network.", "fields": ["id", "network?"]},
            {"code": "balance", "description": "Get a wallet's balance.", "fields": ["id", "wallet?", "network?", "check"]},
            {"code": "receive", "description": "Generate a receive invoice/address; optionally wait for funds.", "fields": ["id", "wallet", "network?", "amount?", "onchain_memo?", "wait_until_paid", "wait_timeout_s?", "wait_poll_interval_ms?", "min_confirmations?", "reference?"]},
            {"code": "receive_claim", "description": "Claim a previously-quoted Lightning receive.", "fields": ["id", "wallet", "quote_id"]},
            {"code": "cashu_send", "description": "Mint a Cashu token to send out-of-band.", "fields": ["id", "wallet?", "amount", "onchain_memo?", "local_memo?", "mints?", "idempotency_key?"]},
            {"code": "cashu_receive", "description": "Redeem a Cashu token.", "fields": ["id", "wallet?", "token"]},
            {"code": "send", "description": "Pay to an address or invoice. Amount may be in the URI or in the explicit `amount` field.", "fields": ["id", "wallet?", "network?", "to", "amount?", "onchain_memo?", "local_memo?", "mints?", "chain_id?", "idempotency_key?"]},
            {"code": "restore", "description": "Rescan a wallet from its mnemonic.", "fields": ["id", "wallet"]},
            {"code": "history", "description": "List wallet history.", "fields": ["id", "wallet?", "network?", "onchain_memo?", "limit?", "offset?", "since_epoch_s?", "until_epoch_s?"]},
            {"code": "history_status", "description": "Look up a single transaction status.", "fields": ["id", "transaction_id"]},
            {"code": "history_update", "description": "Sync recent history from the chain.", "fields": ["id", "wallet?", "network?", "limit?"]},
            {"code": "limit_add", "description": "Add a spend-limit rule. Requires local ledger.", "fields": ["id", "limit"], "local_only": true},
            {"code": "limit_remove", "description": "Remove a spend-limit rule.", "fields": ["id", "rule_id"], "local_only": true},
            {"code": "limit_list", "description": "Show all spend-limit rules and current spend.", "fields": ["id"]},
            {"code": "limit_set", "description": "Replace the full set of spend-limit rules.", "fields": ["id", "limits"], "local_only": true},
            {"code": "reconcile_reservation", "description": "Force a stuck spend-ledger reservation to a terminal state. Operator-only repair when AccountingInconsistent fired or BTC settlement crossed the reservation TTL.", "fields": ["id", "reservation_id", "action", "reason"], "local_only": true},
            {"code": "wallet_config_show", "description": "Show per-wallet config.", "fields": ["id", "wallet"]},
            {"code": "wallet_config_set", "description": "Mutate per-wallet config.", "fields": ["id", "wallet", "label?", "rpc_endpoints?", "chain_id?"], "local_only": true},
            {"code": "wallet_config_token_add", "description": "Register an SPL/ERC20 token for a wallet.", "fields": ["id", "wallet", "symbol", "address", "decimals"], "local_only": true},
            {"code": "wallet_config_token_remove", "description": "Unregister an SPL/ERC20 token.", "fields": ["id", "wallet", "symbol"], "local_only": true},
            {"code": "close", "description": "Close the pipe session.", "fields": []}
        ],
        "outputs": [
            {"kind": "error", "payload": "error", "fields": ["code", "message", "hint?", "retryable", "id?"], "top_level": ["kind", "error", "trace"]},
            {"kind": "result", "payload": "result", "description": "Successful business outputs keep their concrete code and fields inside result.", "top_level": ["kind", "result", "trace"]},
            {"kind": "log", "payload": "log", "fields": ["timestamp_epoch_ms?", "message", "level", "event?"], "top_level": ["kind", "log", "trace"]},
            {"code": "limit_exceeded", "fields": ["id", "rule_id", "scope", "scope_key", "spent", "max_spend", "token?", "remaining_s", "origin?", "trace"]},
            {"code": "accounting_inconsistent", "fields": ["id", "transaction_id", "reservation_ids", "confirm_errors", "hint", "trace"], "description": "Money sent but ledger could not record the debit — must reconcile manually."},
            {"code": "dry_run", "fields": ["id?", "command", "params", "trace"]},
            {"code": "wallet_created", "fields": ["id", "wallet", "network", "address", "mnemonic_secret?", "trace"]},
            {"code": "wallet_closed", "fields": ["id", "wallet", "trace"]},
            {"code": "wallet_list", "fields": ["id", "wallets", "trace"]},
            {"code": "balance", "fields": ["id", "wallet", "network", "balance", "address?", "trace"]},
            {"code": "receive", "fields": ["id", "wallet", "info", "trace"]},
            {"code": "receive_claimed", "fields": ["id", "wallet", "amount", "trace"]},
            {"code": "cashu_sent", "fields": ["id", "wallet", "transaction_id", "status", "fee?", "token", "reservation_ids?", "trace"]},
            {"code": "cashu_received", "fields": ["id", "wallet", "amount", "memo?", "trace"]},
            {"code": "sent", "fields": ["id", "wallet", "transaction_id", "amount", "fee?", "preimage?", "reservation_ids?", "trace"]},
            {"code": "history", "fields": ["id", "items", "trace"]},
            {"code": "history_status", "fields": ["id", "info", "trace"]},
            {"code": "history_updated", "fields": ["id", "stats", "trace"]},
            {"code": "limit_added", "fields": ["id", "rule_id", "trace"]},
            {"code": "limit_removed", "fields": ["id", "rule_id", "trace"]},
            {"code": "limit_status", "fields": ["id", "limits", "downstream?", "trace"]},
            {"code": "reconciled", "fields": ["id", "reservation_id", "action", "previous_status", "new_status", "trace"]},
            {"kind": "log", "fields": ["event", "request_id?", "version?", "config?", "args?", "env?", "trace"]},
            {"code": "schema", "fields": ["schema", "trace"]},
            {"code": "version", "fields": ["version", "json_protocol_version", "trace"]}
        ],
        "error_codes": [
            {"code": "not_implemented", "retryable": false, "description": "Operation not supported in this build/mode."},
            {"code": "wallet_not_found", "retryable": false, "description": "No wallet with that id or label."},
            {"code": "invalid_amount", "retryable": false, "description": "Amount validation failed."},
            {"code": "network_error", "retryable": true, "description": "Transient network failure."},
            {"code": "internal_error", "retryable": false, "description": "Internal failure; check daemon logs."},
            {"code": "limit_exceeded", "retryable": false, "description": "A spend-limit rule rejected this debit. See limit_exceeded output."},
            {"code": "configure_on_daemon", "retryable": false, "description": "Mutating limits requires running on the daemon, not the client."},
            {"code": "remote_protocol_error", "retryable": false, "description": "Upstream daemon returned malformed payload."},
            {"code": "forbidden", "retryable": false, "description": "Operator policy (e.g. URL allowlist, --public-listen, wrong_chain, wrong_cluster, reservation_terminal) rejected the request."},
            {"code": "idempotency_conflict", "retryable": false, "description": "Same idempotency_key was used with a different body. Pick a new key or re-submit the exact original body."},
            {"code": "idempotency_in_progress", "retryable": true, "description": "Another request with this idempotency_key is still running. Retry after retry_after_ms; the original response will replay."}
        ],
        "notes": [
            "Local-only inputs (e.g. local_wallet_show_seed, limit_add) are rejected over REST/RPC with 403.",
            "Pass `dry_run: true` to validate a request without side effects.",
            "Errors with `retryable: true` should be retried with exponential backoff.",
            "Input::Send `chain_id` is opt-in pinning: when supplied, a mismatch refuses with `forbidden` (`wrong_chain`); when omitted, the send proceeds against the wallet's recorded chain and the daemon emits an `evm_chain_unpinned` log so observant agents can detect accidental cross-chain sends.",
            "Input::WalletCreate `sol_cluster` enables Solana cluster pinning. The check is hostname-based heuristic — unknown / private / proxied RPC hosts yield no opinion and the check is skipped; matched mismatches refuse with `forbidden` (`wrong_cluster`).",
            "URL allowlists (operator config): `allowed_mint_urls`, `allowed_esplora_urls`, `allowed_sol_rpc_endpoints`, `allowed_evm_rpc_endpoints`, `allowed_btc_core_urls`, `allowed_btc_electrum_urls`, `allowed_ln_endpoints`. Each defaults empty (no restriction). When `--public-listen` is set the daemon refuses to start unless at least one is non-empty.",
            "Input::Send / Input::CashuSend accept an opaque `idempotency_key` (≤128 chars, 24h TTL). Two requests with the same key replay the first terminal output instead of re-broadcasting; mismatched bodies return idempotency_conflict.",
        ]
    })
}
