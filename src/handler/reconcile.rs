use crate::provider::PayError;
use crate::spend::ReconcileOutcome;
use crate::types::*;
use std::time::Instant;

use super::App;
use super::helpers::*;

const RECONCILE_REASON_MAX_LEN: usize = 512;

pub(crate) async fn dispatch_reconcile(app: &App, input: Input) {
    let Input::ReconcileReservation {
        id,
        reservation_id,
        action,
        reason,
    } = input
    else {
        return;
    };

    let start = Instant::now();
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        emit_error(
            &app.writer,
            Some(id),
            &PayError::invalid_amount(
                "reconcile_reservation reason is required (1..=512 chars)".to_string(),
            ),
            start,
        )
        .await;
        return;
    }
    if reason.len() > RECONCILE_REASON_MAX_LEN {
        emit_error(
            &app.writer,
            Some(id),
            &PayError::invalid_amount(format!(
                "reconcile_reservation reason length {} exceeds max {RECONCILE_REASON_MAX_LEN}",
                reason.len()
            )),
            start,
        )
        .await;
        return;
    }

    let outcome = match action {
        ReconcileAction::Confirm => {
            app.spend_ledger
                .force_confirm(reservation_id, &reason)
                .await
        }
        ReconcileAction::Cancel => app.spend_ledger.force_cancel(reservation_id, &reason).await,
    };

    match outcome {
        Ok(ReconcileOutcome::Reconciled {
            previous_status,
            new_status,
        }) => {
            // Audit trail: every reconcile leaves a log line so operators can
            // later diff "what reservation was in what state when we acted".
            emit_log(
                app,
                "reservation_reconciled",
                Some(id.clone()),
                serde_json::json!({
                    "reservation_id": reservation_id,
                    "action": action.as_str(),
                    "reason": &reason,
                    "previous_status": previous_status,
                    "new_status": new_status,
                }),
            )
            .await;
            let _ = app
                .writer
                .send(Output::Reconciled {
                    id,
                    reservation_id,
                    action,
                    previous_status: previous_status.to_string(),
                    new_status: new_status.to_string(),
                    trace: trace_from(start),
                })
                .await;
        }
        Ok(ReconcileOutcome::NotFound) => {
            emit_error(
                &app.writer,
                Some(id),
                &PayError::invalid_amount(format!(
                    "reservation_not_found: no reservation with id {reservation_id}"
                )),
                start,
            )
            .await;
        }
        Ok(ReconcileOutcome::AlreadyTerminal { current_status }) => {
            emit_error_hint(
                &app.writer,
                Some(id),
                &PayError::Forbidden {
                    message: format!(
                        "reservation_terminal: reservation {reservation_id} is already {current_status}; refusing to flip terminal state"
                    ),
                    hint: Some(
                        "to override Confirmed/Cancelled state you must edit the ledger directly; reconcile only repairs Pending/Expired".to_string(),
                    ),
                },
                start,
                None,
            )
            .await;
        }
        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
    }
}
