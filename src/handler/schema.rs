/// Self-describing wire schema for the JSON Input/Output protocol.
///
/// Returned by `Input::Schema` in pipe and CLI mode so an agent on those
/// transports can discover the operation set, field shapes, and error codes
/// without scraping `--help` or the source tree. The HTTP API describes itself
/// through OpenAPI instead — see `crate::api` — because a resource model has
/// per-operation schemas this flat operation list cannot express.
///
/// This builder is hand-maintained and intentionally minimal. The
/// `schema_includes_every_expected_input_code` test below catches drift
/// between this map and the `Input` enum.
pub fn wire_protocol_schema() -> serde_json::Value {
    serde_json::json!({
        "version": "v1",
        // Bump on every Input/Output/ErrorCode shape change so agents can pin
        // against a known set without re-fetching and diffing the whole doc.
        "schema_version": "2026-08-21.1",
        "git_sha": env!("GIT_SHA"),
        "envelope": {
            "description": "Every request is wrapped in a Request envelope. Plain Input JSON also works (dry_run defaults to false).",
            "shape": {
                "dry_run": "bool? — when true, validates without executing and emits Output::DryRun",
                "<flattened Input fields>": "the operation code and its parameters"
            }
        },
        "endpoints": {
            "Input::Schema (pipe/cli)": "This document, returned as Output::Schema.",
            "GET /openapi.json": "The HTTP transport is described by its own OpenAPI document, not by this one. This document describes the pipe and CLI wire protocol; the HTTP API is a resource model over the same dispatcher, and `afpay api export` writes its contract without needing a daemon."
        },
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
            {"code": "cashu_send_plan", "description": "Resolve a Cashu bearer-token mint into a reviewable plan. Nothing is minted; `pay_confirm` with the returned plan_id is what mints.", "fields": ["id", "wallet?", "amount", "onchain_memo?", "local_memo?", "mints?"]},
            {"code": "cashu_receive", "description": "Redeem a Cashu token.", "fields": ["id", "wallet?", "token"]},
            {"code": "send_plan", "description": "Resolve a payment to an address or invoice into a reviewable plan: the wallet afpay would use, what leaves it, the fee, and the spend budgets it debits. Amount may be in the URI or in the explicit `amount` field. Nothing is broadcast; `pay_confirm` with the returned plan_id is what pays.", "fields": ["id", "wallet?", "network?", "to", "amount?", "onchain_memo?", "local_memo?", "mints?", "chain_id?"]},
            {"code": "pay_confirm", "description": "Execute a plan that was reviewed. The only operation that moves money. Carries the plan id and nothing else: what runs is read from the stored plan. Single-use, expires, and is refused when the workspace, configuration, wallet or spend rules changed since it was resolved.", "fields": ["id", "plan_id", "expect?", "idempotency_key?"]},
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
            {"code": "pay_planned", "fields": ["id", "plan_id", "operation", "network", "wallet", "to?", "amount_native", "fee_estimate_native", "fee_unit", "onchain_memo?", "local_memo?", "spend_debits?", "warnings?", "expires_at_epoch_ms", "trace"], "description": "A resolved payment waiting to be confirmed. Nothing has moved; warnings are part of the result and cannot be hidden by log filters."},
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
            {"code": "remote_protocol_error", "retryable": false, "description": "An afpay peer returned a malformed payload."},
            {"code": "peer_mismatch", "retryable": false, "description": "The node named by --peer-url is not this afpay: another service, another version, a route it does not serve, or a credential it refused. Read <peer-url>/health."},
            {"code": "forbidden", "retryable": false, "description": "Operator policy (e.g. URL allowlist, --public-listen, wrong_chain, reservation_terminal) rejected the request."},
            {"code": "idempotency_conflict", "retryable": false, "description": "Same idempotency_key was used with a different body. Pick a new key or re-submit the exact original body."},
            {"code": "idempotency_in_progress", "retryable": true, "description": "Another request with this idempotency_key is still running. Retry after retry_after_ms; the original response will replay."},
            {"code": "busy", "retryable": true, "description": "Another operation holds the workspace write lock. Retry the identical request; reuse the same idempotency_key when it moves money."},
            {"code": "plan_not_found", "retryable": false, "description": "No confirmable plan with that id: never issued here, already confirmed, already refused, or expired. Plans are single-use — to retry a confirm that may already have run, resend the original idempotency_key instead of the plan."},
            {"code": "plan_expired", "retryable": false, "description": "The plan's window closed. Resolve the payment again; the new plan quotes a current fee."},
            {"code": "plan_stale", "retryable": false, "description": "The workspace, daemon configuration, wallet metadata or spend-limit rules changed after this plan was resolved, so the reviewed terms no longer describe what would happen. `drifted` names which. Resolve the payment again and review the new plan."}
        ],
        "notes": [
            "Local-only inputs (e.g. local_wallet_show_seed, limit_add) have no route in the HTTP API, and the federation client (--peer-url) refuses them before sending. There is one machine face and it withholds them.",
            "Pass `dry_run: true` to validate a request without side effects.",
            "Errors with `retryable: true` should be retried with exponential backoff.",
            "Every operation that moves value out of a wallet is two steps: `send_plan` / `cashu_send_plan` resolve it and record a plan, and `pay_confirm` executes exactly that plan. No remote effect begins before the confirm, and no transport — CLI, pipe, HTTP, the confirm window, or federation — has a route that skips it.",
            "Input::SendPlan `chain_id` is opt-in pinning: when supplied, a mismatch refuses with `forbidden` (`wrong_chain`) at plan time; when omitted, `pay_planned.warnings` includes `evm_chain_unpinned`.",
            "Input::WalletCreate `sol_cluster` records intent. Hostname classification is best-effort, never a hard guarantee: unpinned, unclassifiable, or apparently mismatched plans carry a structured warning that log filters cannot hide.",
            "A send whose network side effect succeeded but whose spend ledger could not confirm emits exactly one terminal `accounting_inconsistent` result. It never follows that result with `sent` or `cashu_sent`, and an idempotent retry replays the same inconsistency.",
            "URL allowlists (operator config): `allowed_mint_urls`, `allowed_esplora_urls`, `allowed_sol_rpc_endpoints`, `allowed_evm_rpc_endpoints`, `allowed_btc_core_urls`, `allowed_btc_electrum_urls`, `allowed_ln_endpoints`. Each defaults empty (no restriction). When `--public-listen` is set the daemon refuses to start unless at least one is non-empty.",
            "Input::PayConfirm accepts an opaque `idempotency_key` (≤128 chars, 24h TTL). Two confirms with the same key and plan replay the first terminal output instead of paying twice; the same key aimed at a different plan returns idempotency_conflict. The plan itself is single-use regardless, so a second confirm without a key is refused with plan_not_found rather than paying again.",
        ]
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::wire_protocol_schema;

    /// Every Input variant that goes over a wire must appear in the schema.
    /// Add a new variant and forget this map, and the test names the code you
    /// left out. Codes here are the serde-rename strings from
    /// `src/types/protocol.rs`.
    const EXPECTED_INPUT_CODES: &[&str] = &[
        "version",
        "schema",
        "config_get",
        "config_set",
        "wallet_create",
        "ln_wallet_create",
        "wallet_close",
        "wallet_list",
        "balance",
        "receive",
        "receive_claim",
        "cashu_send_plan",
        "cashu_receive",
        "send_plan",
        "pay_confirm",
        "restore",
        "history",
        "history_status",
        "history_update",
        "limit_add",
        "limit_remove",
        "limit_list",
        "limit_set",
        "reconcile_reservation",
        "wallet_config_show",
        "wallet_config_set",
        "wallet_config_token_add",
        "wallet_config_token_remove",
        "close",
    ];

    #[test]
    fn schema_includes_every_expected_input_code() {
        let schema = wire_protocol_schema();
        assert!(
            schema
                .get("git_sha")
                .and_then(|value| value.as_str())
                .is_some(),
            "wire schema must identify the build that emitted it"
        );
        let inputs = schema
            .get("inputs")
            .and_then(|value| value.as_array())
            .unwrap();
        let listed: std::collections::HashSet<&str> = inputs
            .iter()
            .filter_map(|entry| entry.get("code").and_then(|value| value.as_str()))
            .collect();
        for expected in EXPECTED_INPUT_CODES {
            assert!(
                listed.contains(expected),
                "wire_protocol_schema is missing input code `{expected}`; \
                 add it to the inputs[] array in src/handler/schema.rs"
            );
        }
    }

    /// This document describes the pipe/CLI wire protocol. It must not claim
    /// an HTTP route: the HTTP API is a resource model with its own OpenAPI
    /// contract, and a stale endpoint map here is how an agent ends up POSTing
    /// a command envelope at a route that no longer exists.
    #[test]
    fn schema_does_not_advertise_http_routes_of_its_own() {
        let schema = wire_protocol_schema();
        let endpoints = schema
            .get("endpoints")
            .and_then(|value| value.as_object())
            .unwrap();
        for key in endpoints.keys() {
            assert!(
                !key.starts_with("POST ")
                    && !key.starts_with("PUT ")
                    && !key.starts_with("DELETE "),
                "wire_protocol_schema advertises the HTTP route `{key}`; the HTTP API \
                 describes itself through GET /openapi.json"
            );
        }
        assert!(endpoints.contains_key("GET /openapi.json"));
    }

    #[test]
    fn schema_documents_all_pay_error_codes() {
        let schema = wire_protocol_schema();
        let errors = schema
            .get("error_codes")
            .and_then(|value| value.as_array())
            .unwrap();
        let listed: std::collections::HashSet<&str> = errors
            .iter()
            .filter_map(|entry| entry.get("code").and_then(|value| value.as_str()))
            .collect();
        // Mirror of PayError::error_code() in src/provider/mod.rs.
        for expected in &[
            "not_implemented",
            "wallet_not_found",
            "invalid_amount",
            "network_error",
            "internal_error",
            "limit_exceeded",
            "configure_on_daemon",
            "remote_protocol_error",
            "peer_mismatch",
            "forbidden",
            "busy",
            "plan_not_found",
            "plan_expired",
            "plan_stale",
        ] {
            assert!(
                listed.contains(expected),
                "wire_protocol_schema error_codes missing `{expected}` — \
                 keep in sync with PayError::error_code()"
            );
        }
    }
}
