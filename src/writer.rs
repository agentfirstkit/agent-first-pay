use crate::types::Output;
use agent_first_data::OutputFormat;
use tokio::sync::mpsc;

pub async fn writer_task(mut rx: mpsc::Receiver<Output>, format: OutputFormat) {
    while let Some(output) = rx.recv().await {
        if crate::output_fmt::emit_process_output(&output, format).is_err() {
            break;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::types::Trace;

    #[test]
    fn wallet_seed_json_is_not_redacted() {
        let value = serde_json::json!({
            "code": "wallet_seed",
            "mnemonic_secret": "raw secret",
            "trace": {"duration_ms": 0}
        });
        let rendered = crate::output_fmt::render_value_with_policy(&value, OutputFormat::Json);
        assert!(rendered.contains("\"mnemonic_secret\":\"raw secret\""));
    }

    #[test]
    fn non_wallet_seed_json_still_redacts_secret() {
        let value = serde_json::json!({
            "code": "balance",
            "mnemonic_secret": "raw secret",
            "trace": {"duration_ms": 0}
        });
        let rendered = crate::output_fmt::render_value_with_policy(&value, OutputFormat::Json);
        assert!(rendered.contains("\"mnemonic_secret\":\"***\""));
    }

    #[test]
    fn protocol_adapter_emits_strict_flat_error() {
        let output = Output::Error {
            id: Some("req-1".to_string()),
            error_code: "wallet_not_found".to_string(),
            error: "wallet is missing".to_string(),
            hint: Some("list wallets first".to_string()),
            retryable: false,
            retry_after_ms: None,
            trace: Trace::from_duration(3),
        };
        let event = crate::output_fmt::protocol_event(&output).expect("protocol event");
        agent_first_data::validate_protocol_event(&event, true).expect("strict AFDATA event");
        assert_eq!(event["kind"], "error");
        assert_eq!(event["error"]["code"], "wallet_not_found");
        assert_eq!(event["error"]["message"], "wallet is missing");
        assert_eq!(event["error"]["id"], "req-1");
        assert!(event["error"].get("details").is_none());
        assert!(event.get("error_code").is_none());
    }
}
