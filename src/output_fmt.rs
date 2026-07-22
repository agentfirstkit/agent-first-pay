use crate::types::Output;
use agent_first_data::{
    CliEmitter, CliEmitterError, OutputFormat, OutputOptions, ProtocolViolation, RedactionPolicy,
    Redactor,
};
use serde_json::Value;
use std::io::Write;

pub fn render_value_with_policy(value: &serde_json::Value, format: OutputFormat) -> String {
    let value = if value.get("kind").is_none() {
        protocol_event_value(value.clone()).unwrap_or_else(|_| value.clone())
    } else {
        value.clone()
    };
    if format == OutputFormat::Json
        && value.pointer("/result/code").and_then(Value::as_str) == Some("wallet_seed")
    {
        agent_first_data::render(&value, format, &RedactionPolicy::Off.into())
    } else {
        agent_first_data::render(&value, format, &OutputOptions::default())
    }
}

pub fn emit_output<W: Write>(
    writer: W,
    output: &Output,
    format: OutputFormat,
) -> Result<(), CliEmitterError> {
    let mut value = serde_json::to_value(output)
        .map_err(|error| CliEmitterError::Validation(output_serialization_violation(&error)))?;
    let trace = value
        .get("trace")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Value::Object(fields) = &mut value {
        fields.remove("trace");
    }

    let redaction = if code == "wallet_seed" {
        Redactor::new().policy(RedactionPolicy::Off)
    } else {
        Redactor::new()
    };
    let options = OutputOptions {
        redaction,
        ..OutputOptions::default()
    };
    let mut emitter = CliEmitter::with_options(writer, format, options).with_strict_protocol();

    match code.as_str() {
        "log" => {
            let timestamp_epoch_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or_default();
            if let Value::Object(fields) = &mut value {
                fields.remove("code");
                fields.insert(
                    "timestamp_epoch_ms".to_string(),
                    Value::from(timestamp_epoch_ms),
                );
            }
            let event = agent_first_data::json_log(value).trace(trace).build();
            emitter.emit(event)
        }
        "error" => {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("payment command failed")
                .to_string();
            let error_code = value
                .get("error_code")
                .and_then(Value::as_str)
                .unwrap_or("payment_error")
                .to_string();
            let hint = value
                .get("hint")
                .and_then(Value::as_str)
                .map(str::to_string);
            let retryable = value
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Value::Object(fields) = &mut value {
                fields.remove("code");
                fields.remove("error_code");
                fields.remove("error");
                fields.remove("hint");
                fields.remove("retryable");
            }
            let event = agent_first_data::json_error(&error_code, &message)
                .hint_if_some(hint.as_deref())
                .retryable_if(retryable)
                .fields(value)
                .trace(trace)
                .build()
                .map_err(CliEmitterError::Build)?;
            emitter.emit(event)
        }
        _ => {
            let event = agent_first_data::json_result(value).trace(trace).build();
            emitter.emit(event)
        }
    }
}

pub fn protocol_event(output: &Output) -> Result<Value, String> {
    let value = serde_json::to_value(output)
        .map_err(|error| format!("output serialization failed: {error}"))?;
    protocol_event_value(value)
}

fn protocol_event_value(mut value: Value) -> Result<Value, String> {
    let trace = value
        .get("trace")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Value::Object(fields) = &mut value {
        fields.remove("trace");
    }
    let event = match code.as_str() {
        "log" => {
            let timestamp_epoch_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or_default();
            if let Value::Object(fields) = &mut value {
                fields.remove("code");
                fields.insert(
                    "timestamp_epoch_ms".to_string(),
                    Value::from(timestamp_epoch_ms),
                );
            }
            agent_first_data::json_log(value).trace(trace).build()
        }
        "error" => {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("payment command failed")
                .to_string();
            let error_code = value
                .get("error_code")
                .and_then(Value::as_str)
                .unwrap_or("payment_error")
                .to_string();
            let hint = value
                .get("hint")
                .and_then(Value::as_str)
                .map(str::to_string);
            let retryable = value
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Value::Object(fields) = &mut value {
                fields.remove("code");
                fields.remove("error_code");
                fields.remove("error");
                fields.remove("hint");
                fields.remove("retryable");
            }
            agent_first_data::json_error(&error_code, &message)
                .hint_if_some(hint.as_deref())
                .retryable_if(retryable)
                .fields(value)
                .trace(trace)
                .build()
                .map_err(|error| error.to_string())?
        }
        _ => agent_first_data::json_result(value).trace(trace).build(),
    };
    agent_first_data::validate_protocol_event(event.as_value(), true)
        .map_err(|error| error.to_string())?;
    Ok(event.into_value())
}

pub fn cli_error_event(message: &str, hint: Option<&str>) -> Value {
    agent_first_data::json_error("cli_error", message)
        .hint_if_some(hint)
        .build()
        .map(Into::into)
        .unwrap_or(serde_json::Value::Null)
}

/// Build the `output_serialization_failed` protocol violation used when this
/// binary's own `Output` fails to serialize to JSON before AFDATA envelope
/// construction — a state bug, not something a caller can recover from, but
/// still surfaced through the standard `CliEmitterError::Validation` path.
fn output_serialization_violation(error: &serde_json::Error) -> ProtocolViolation {
    ProtocolViolation {
        rule: "output_serialization_failed",
        pointer: String::new(),
        message: format!("output serialization failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_remote_error_is_rendered_as_strict_afdata_event() {
        let rendered = render_value_with_policy(
            &serde_json::json!({
                "code": "error",
                "error_code": "wallet_not_found",
                "error": "wallet is missing",
                "retryable": false,
            }),
            OutputFormat::Json,
        );
        let event: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
        assert_eq!(event["kind"], "error");
        assert_eq!(event["error"]["code"], "wallet_not_found");
        assert_eq!(event["error"]["message"], "wallet is missing");
        assert!(event.get("error_code").is_none());
    }
}
