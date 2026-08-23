use crate::types::Output;
use agent_first_data::{
    CliEmitter, CliEmitterError, OutputFormat, OutputOptions, ProtocolViolation, RedactionPolicy,
    Redactor,
};
use serde_json::Value;
use std::sync::OnceLock;

static OUTPUT_TO: OnceLock<agent_first_data::OutputTo> = OnceLock::new();

pub fn install_output_to<I, S>(args: I) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect::<Vec<_>>();
    let mut value = "split".to_string();
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            break;
        }
        if let Some(candidate) = arg.strip_prefix("--output-to=") {
            value = candidate.to_string();
        } else if arg == "--output-to" {
            index += 1;
            value = args.get(index).cloned().ok_or_else(|| {
                "--output-to requires a value: split, stdout, or stderr".to_string()
            })?;
        }
        index += 1;
    }
    let selector = agent_first_data::OutputTo::parse(&value)?;
    OUTPUT_TO
        .set(selector)
        .map_err(|_| "AFDATA output routing was initialized more than once".to_string())
}

#[must_use]
pub fn output_to() -> agent_first_data::OutputTo {
    OUTPUT_TO
        .get()
        .copied()
        .unwrap_or(agent_first_data::OutputTo::Split)
}

// AFDATA injects the raw outcomes this writes, so it owns the routing and the
// rule that a closed reader is success rather than failure.
pub fn write_process_result(text: &str) -> std::io::Result<()> {
    agent_first_data::write_raw(text, output_to())
}

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

pub fn emit_process_output(output: &Output, format: OutputFormat) -> Result<(), CliEmitterError> {
    let value = serde_json::to_value(output)
        .map_err(|error| CliEmitterError::Validation(output_serialization_violation(&error)))?;
    let event = protocol_event_value(value).map_err(|message| {
        CliEmitterError::Validation(ProtocolViolation {
            rule: "output_event_invalid",
            pointer: String::new(),
            message,
        })
    })?;
    let redaction = if event.pointer("/result/code").and_then(Value::as_str) == Some("wallet_seed")
    {
        Redactor::new().policy(RedactionPolicy::Off)
    } else {
        Redactor::new()
    };
    let options = OutputOptions {
        redaction,
        ..OutputOptions::default()
    };
    let mut emitter =
        CliEmitter::from_output_to_with(output_to(), format, options).with_strict_protocol();
    emitter.emit_validated_value(event)
}

pub fn emit_process_event(value: Value, format: OutputFormat) -> Result<(), CliEmitterError> {
    let mut emitter = CliEmitter::from_output_to(output_to(), format).with_strict_protocol();
    emitter.emit_validated_value(value)
}

pub fn emit_process_event_with_redaction(
    value: Value,
    format: OutputFormat,
    redaction: RedactionPolicy,
) -> Result<(), CliEmitterError> {
    let options = OutputOptions {
        redaction: Redactor::new().policy(redaction),
        ..OutputOptions::default()
    };
    let mut emitter =
        CliEmitter::from_output_to_with(output_to(), format, options).with_strict_protocol();
    emitter.emit_validated_value(value)
}

pub fn emit_process_value_with_policy(
    value: &Value,
    format: OutputFormat,
) -> Result<(), CliEmitterError> {
    let value = if value.get("kind").is_none() {
        protocol_event_value(value.clone()).map_err(|message| {
            CliEmitterError::Validation(ProtocolViolation {
                rule: "output_event_invalid",
                pointer: String::new(),
                message,
            })
        })?
    } else {
        value.clone()
    };
    let mut emitter = CliEmitter::from_output_to(output_to(), format).with_strict_protocol();
    emitter.emit_validated_value(value)
}

/// Redact a value afpay computed rather than emitted.
///
/// `protocol_event` is the seam for afpay's own `Output`s, and everything a
/// panel renders should cross it. A fee quote a panel asked a provider for is
/// the one thing that has no `Output` to cross it through, so it crosses here —
/// through the same redactor, on the same `_secret` convention. The alternative
/// is a panel deciding for itself which provider fields are sensitive, which is
/// how a second, laxer rule gets written.
pub fn redacted_value(value: &Value) -> Value {
    agent_first_data::redacted_value(value)
}

pub fn protocol_event(output: &Output) -> Result<Value, String> {
    let value = serde_json::to_value(output)
        .map_err(|error| format!("output serialization failed: {error}"))?;
    protocol_event_value(value).map(|event| agent_first_data::redacted_value(&event))
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

/// Build a terminal error event whose `code` names the failure.
///
/// There is no generic `cli_error` bucket: the closed-world parser reports
/// argv failures under `cli_unknown_argument` and its siblings, and everything
/// this process rejects afterwards names itself the same way.
pub fn coded_error_event(code: &str, message: &str, hint: Option<&str>) -> Value {
    agent_first_data::json_error(code, message)
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

    #[test]
    fn remote_protocol_event_redacts_secrets_in_log_fields() {
        let out = Output::Log {
            event: "wallet".to_string(),
            request_id: Some("req-1".to_string()),
            version: None,
            argv: None,
            config: None,
            args: Some(serde_json::json!({
                "admin_key_secret": "super-secret",
                "endpoint_url": "https://example.test/"
            })),
            env: None,
            trace: crate::types::Trace::from_duration(1),
        };
        let event = protocol_event(&out).expect("valid protocol event");
        assert_eq!(event["log"]["args"]["admin_key_secret"], "***");
        assert_eq!(
            event["log"]["args"]["endpoint_url"],
            "https://example.test/"
        );
    }
}
