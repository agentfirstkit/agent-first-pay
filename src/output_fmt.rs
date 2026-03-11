use agent_first_data::{OutputFormat, RedactionPolicy};

pub fn render_value_with_policy(value: &serde_json::Value, format: OutputFormat) -> String {
    if format == OutputFormat::Json
        && value.get("code").and_then(|v| v.as_str()) == Some("wallet_seed")
    {
        agent_first_data::output_json_with(value, RedactionPolicy::RedactionNone)
    } else {
        agent_first_data::cli_output(value, format)
    }
}
