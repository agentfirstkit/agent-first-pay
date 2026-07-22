//! `afpay skill` subcommand. Installs/uninstalls/reports status of the embedded
//! Agent Skill across Codex, Claude Code, opencode, and Hermes via the shared
//! `agent_first_data::skill` admin — the same implementation every spore uses.

use crate::args::{
    SkillAdminAction, SkillAdminOptions, SkillAdminRequest, SkillAgentSelection, SkillScope,
};
use agent_first_data::skill::{
    self, SkillAction, SkillAgentSelection as AfSelection, SkillOptions, SkillScope as AfScope,
    SkillSpec,
};
use agent_first_data::{OutputFormat, OutputOptions, json_result, render};
use serde_json::Value;
use std::io::Write;

/// The embedded skill this binary installs.
const SPEC: SkillSpec = SkillSpec {
    name: "agent-first-pay",
    source: include_str!("../skills/agent-first-pay/SKILL.md"),
    title: "Agent-First Pay",
    marker_slug: "afpay",
    assets: &[],
};

pub fn run(req: SkillAdminRequest) -> i32 {
    let (action, options) = split_action(req.action);
    let (code, value) = match skill::run_skill_admin(&SPEC, action, &options) {
        Ok(report) => match serde_json::to_value(&report) {
            Ok(value) => (0, Value::from(json_result(value).build())),
            Err(e) => (
                1,
                crate::output_fmt::cli_error_event(
                    &format!("failed to serialize skill report: {e}"),
                    None,
                ),
            ),
        },
        Err(err) => (
            1,
            crate::output_fmt::cli_error_event(&err.message, err.hint.as_deref()),
        ),
    };
    emit_value(&value, req.output);
    code
}

fn emit_value(value: &Value, output: OutputFormat) {
    let rendered = render(value, output, &OutputOptions::default());
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

fn split_action(action: SkillAdminAction) -> (SkillAction, SkillOptions) {
    match action {
        SkillAdminAction::Status(options) => (SkillAction::Status, convert_options(options)),
        SkillAdminAction::Install(options) => (SkillAction::Install, convert_options(options)),
        SkillAdminAction::Uninstall(options) => (SkillAction::Uninstall, convert_options(options)),
    }
}

fn convert_options(options: SkillAdminOptions) -> SkillOptions {
    SkillOptions {
        agent: convert_agent(options.agent),
        scope: convert_scope(options.scope),
        skills_dir: options.skills_dir,
        force: options.force,
    }
}

fn convert_agent(agent: SkillAgentSelection) -> AfSelection {
    match agent {
        SkillAgentSelection::All => AfSelection::All,
        SkillAgentSelection::Codex => AfSelection::Codex,
        SkillAgentSelection::ClaudeCode => AfSelection::ClaudeCode,
        SkillAgentSelection::Opencode => AfSelection::Opencode,
        SkillAgentSelection::Hermes => AfSelection::Hermes,
    }
}

fn convert_scope(scope: SkillScope) -> AfScope {
    match scope {
        SkillScope::Personal => AfScope::Personal,
        SkillScope::Workspace => AfScope::Workspace,
    }
}
