#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Offline process checks for registry rejection and resolved output sinks.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_afpay"))
        .args(args)
        .env("AFPAY_HOME", root.join("home"))
        .env("AFUI_CONFIG_DIR", root.join("ui"))
        .output()
        .expect("run isolated CLI")
}

fn assert_error(output: &Output, exit_code: i32, code: &str) {
    assert_eq!(output.status.code(), Some(exit_code));
    assert!(output.stdout.is_empty(), "unexpected stdout: {output:?}");
    let event: Value = serde_json::from_slice(&output.stderr).expect("one JSON error on stderr");
    agent_first_data::validate_protocol_event(&event, true).expect("strict protocol error");
    assert_eq!(event["kind"], "error");
    assert_eq!(event["error"]["code"], code);
}

#[test]
fn cli_output_rejections_ignore_unresolved_output_options() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["wallet", "list", "--not-a-flag", "--output-to", "stdout"],
        vec!["wallet", "list", "--not-a-flag", "--output", "yaml"],
        vec!["wallet", "list", "--not-a-flag", "--output-to", "stderr"],
    ] {
        assert_error(&run(root.path(), &args), 2, "cli_unknown_argument");
    }
    assert_error(
        &run(root.path(), &["wallet", "list", "--output-to", "invalid"]),
        2,
        "cli_invalid_argument_value",
    );
    assert_error(
        &run(
            root.path(),
            &[
                "wallet",
                "list",
                "--output-to",
                "stdout",
                "--output-to",
                "stderr",
            ],
        ),
        2,
        "cli_duplicate_argument",
    );
    assert!(!root.path().join("home").exists());
}

#[test]
fn cli_output_rejections_never_open_file_sinks() {
    let root = tempfile::tempdir().unwrap();
    let stdout_file = root.path().join("rejected-stdout.jsonl");
    let stderr_file = root.path().join("rejected-stderr.jsonl");
    assert_error(
        &run(
            root.path(),
            &[
                "wallet",
                "list",
                "--not-a-flag",
                "--stdout-file",
                stdout_file.to_str().unwrap(),
                "--stderr-file",
                stderr_file.to_str().unwrap(),
            ],
        ),
        2,
        "cli_unknown_argument",
    );
    assert!(!stdout_file.exists());
    assert!(!stderr_file.exists());

    std::fs::write(&stdout_file, "preserve existing data\n").unwrap();
    assert_error(
        &run(
            root.path(),
            &[
                "wallet",
                "list",
                "--not-a-flag",
                "--stdout-file",
                stdout_file.to_str().unwrap(),
            ],
        ),
        2,
        "cli_unknown_argument",
    );
    assert_eq!(
        std::fs::read_to_string(&stdout_file).unwrap(),
        "preserve existing data\n"
    );
}

// File sinks are afdata stream redirection, which exists only on Unix; the
// Windows promise is pinned by `cli_output_file_sinks_are_refused_on_windows`.
#[cfg(unix)]
#[test]
fn cli_output_sink_failure_is_a_runtime_error_before_domain_io() {
    let root = tempfile::tempdir().unwrap();
    let missing_sink = root.path().join("missing-parent/output.jsonl");
    assert_error(
        &run(
            root.path(),
            &[
                "wallet",
                "list",
                "--output-to",
                "stdout",
                "--stdout-file",
                missing_sink.to_str().unwrap(),
            ],
        ),
        1,
        "output_setup_failed",
    );
    assert!(!root.path().join("home").exists());
}

#[cfg(unix)]
#[test]
fn cli_output_accepted_lifecycle_and_domain_calls_keep_their_sinks() {
    let root = tempfile::tempdir().unwrap();
    for (index, args, code) in [
        (0, vec!["--help"], "help"),
        (1, vec!["--version"], "version"),
        (2, vec!["wallet", "list"], "wallet_list"),
    ] {
        let sink = root.path().join(format!("accepted-{index}.jsonl"));
        let mut args = args;
        args.extend(["--stdout-file", sink.to_str().unwrap()]);
        let output = run(root.path(), &args);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stdout.is_empty());
        let events = std::fs::read_to_string(sink).unwrap();
        let found = events.lines().any(|line| {
            let event: Value = serde_json::from_str(line).expect("JSON event in accepted sink");
            agent_first_data::validate_protocol_event(&event, true).expect("strict event");
            event["result"]["code"] == code
        });
        assert!(found, "missing {code} in {events}");
    }

    let sink = root.path().join("help-on-stderr.jsonl");
    let output = run(
        root.path(),
        &[
            "--help",
            "--output-to",
            "stderr",
            "--stderr-file",
            sink.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let event: Value = serde_json::from_str(&std::fs::read_to_string(sink).unwrap()).unwrap();
    assert_eq!(event["result"]["code"], "help");

    let sink = root.path().join("cli.md");
    let output = run(
        root.path(),
        &["--docs", "--stdout-file", sink.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(std::fs::read_to_string(sink).unwrap().contains("afpay"));
}

#[cfg(windows)]
#[test]
fn cli_output_file_sinks_are_refused_on_windows() {
    let root = tempfile::tempdir().unwrap();
    for flag in ["--stdout-file", "--stderr-file"] {
        let sink = root.path().join("sink.jsonl");
        let output = run(
            root.path(),
            &["wallet", "list", flag, sink.to_str().unwrap()],
        );
        assert_error(&output, 1, "output_setup_failed");
        let event: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            event["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("only supported on Unix")),
            "{event}"
        );
        assert!(!sink.exists());
    }
    assert!(!root.path().join("home").exists());
}
