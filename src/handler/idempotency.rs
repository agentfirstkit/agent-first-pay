//! The one idempotency gate every operation that is not naturally idempotent
//! goes through.
//!
//! §8 of the Provider OpenAPI baseline requires a persistent `Idempotency-Key`
//! on local mutations that a retry could duplicate, and requires the HTTP face
//! to reuse the CLI's implementation rather than deduplicating in memory. This
//! module is that implementation: the key, a canonical hash of what the
//! request asked for, and the terminal outcome go into the spend ledger's
//! 24-hour store, so a repeat replays instead of happening twice.
//!
//! Four operations use it, and they are exactly the ones a retry could
//! duplicate: `pay_confirm` (money leaves), `wallet_create` and
//! `ln_wallet_create` (a second wallet with a second key), and `receive` (a
//! second invoice, while a payer may already hold the first). Everything else
//! either converges on its own — closing a wallet, claiming a mint quote,
//! redeeming a token, syncing history — or is a read.

use std::time::Instant;

use crate::provider::PayError;
use crate::spend::{IDEMPOTENCY_KEY_MAX_LEN, IdempotencyLookup, IdempotentReplayPayload};
use crate::types::{Input, Output};

use super::App;
use super::helpers::emit_error;
use super::helpers::trace_from;

/// Outcome of [`enter_idempotent`]. When `Proceed`, the handler continues
/// with the real send flow and is responsible for calling
/// [`finalize_idempotent`] / [`clear_idempotent`] on terminal output. The
/// other variants mean the handler has ALREADY emitted an Output and must
/// return immediately.
pub(super) enum IdempotencyEntry {
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

pub(super) async fn enter_idempotent(
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
                    "idempotency_key set but this operation has no canonical request hash"
                        .to_string(),
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
        IdempotentReplayPayload::WalletCreated {
            wallet,
            network,
            address,
        } => {
            let _ = app
                .writer
                .send(Output::WalletCreated {
                    id: id.to_string(),
                    wallet,
                    network,
                    address,
                    // Never stored, so never replayed. See the payload's own
                    // documentation for why.
                    mnemonic_secret: None,
                    trace: trace_from(start),
                })
                .await;
        }
        IdempotentReplayPayload::ReceiveInfo {
            wallet,
            receive_info,
        } => {
            let _ = app
                .writer
                .send(Output::ReceiveInfo {
                    id: id.to_string(),
                    wallet,
                    receive_info,
                    trace: trace_from(start),
                })
                .await;
        }
        IdempotentReplayPayload::ReceiveClaimed { wallet, amount } => {
            let _ = app
                .writer
                .send(Output::ReceiveClaimed {
                    id: id.to_string(),
                    wallet,
                    amount,
                    trace: trace_from(start),
                })
                .await;
        }
    }
}

/// Stable hex blake3 hash of what a request asked for.
///
/// Two requests carrying the same key are only treated as the same request if
/// this matches, so a caller who edited the body but forgot to bump the key is
/// refused rather than served someone else's outcome.
///
/// Excludes `id` (request correlation, varies per call), `idempotency_key`
/// (it *is* the key), and `dry_run` (validating does not change identity).
pub(super) fn canonical_request_hash(input: &Input) -> Option<String> {
    let value = match input {
        // The whole body of a confirm is the plan it submits. The payment's
        // contents cannot vary here — they were fixed when it was resolved.
        Input::PayConfirm { plan_id, .. } => {
            serde_json::json!({"kind": "pay_confirm", "plan_id": plan_id})
        }
        Input::WalletCreate { .. } | Input::LnWalletCreate { .. } | Input::Receive { .. } => {
            let mut value = serde_json::to_value(input).ok()?;
            if let Some(object) = value.as_object_mut() {
                object.remove("id");
                object.remove("idempotency_key");
            }
            value
        }
        _ => return None,
    };
    let bytes = serde_json::to_vec(&value).ok()?;
    Some(hex::encode(blake3::hash(&bytes).as_bytes()))
}

pub(super) async fn finalize_idempotent(
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

pub(super) async fn clear_idempotent(app: &App, ctx: Option<&(String, String)>) {
    if let Some((key, hash)) = ctx {
        let _ = app.spend_ledger.idempotency_clear(key, hash).await;
    }
}
