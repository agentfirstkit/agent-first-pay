use crate::types::Output;
use agent_first_data::{OutputFormat, RedactionPolicy};
use std::io::Write;
use tokio::sync::mpsc;

pub async fn writer_task(mut rx: mpsc::Receiver<Output>, format: OutputFormat) {
    while let Some(output) = rx.recv().await {
        let value = serde_json::to_value(output).unwrap_or(serde_json::Value::Null);
        let rendered = render_value_with_policy(&value, format);

        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(rendered.as_bytes());
        if !rendered.ends_with('\n') {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }
}

fn render_value_with_policy(value: &serde_json::Value, format: OutputFormat) -> String {
    if format == OutputFormat::Json
        && value.get("code").and_then(|v| v.as_str()) == Some("wallet_seed")
    {
        agent_first_data::output_json_with(value, RedactionPolicy::RedactionNone)
    } else {
        agent_first_data::cli_output(value, format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallet_seed_json_is_not_redacted() {
        let value = serde_json::json!({
            "code": "wallet_seed",
            "mnemonic_secret": "raw secret",
            "trace": {"duration_ms": 0}
        });
        let rendered = render_value_with_policy(&value, OutputFormat::Json);
        assert!(rendered.contains("\"mnemonic_secret\":\"raw secret\""));
    }

    #[test]
    fn non_wallet_seed_json_still_redacts_secret() {
        let value = serde_json::json!({
            "code": "balance",
            "mnemonic_secret": "raw secret",
            "trace": {"duration_ms": 0}
        });
        let rendered = render_value_with_policy(&value, OutputFormat::Json);
        assert!(rendered.contains("\"mnemonic_secret\":\"***\""));
    }
}
