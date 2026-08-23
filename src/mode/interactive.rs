use super::InteractiveSessionRuntime;
use super::session::{HostMessageKind, InteractionHost, PlannedPayment, parse_session_command};
use rustyline::Editor;
use rustyline::error::ReadlineError;
use std::io::Write as _;

struct LineHost;

impl LineHost {
    fn emit_stdout(text: &str) {
        let _ = writeln!(std::io::stdout(), "{text}");
    }

    fn prompt_with_lines(lines: &[String], prompt: &str) -> Option<String> {
        for line in lines {
            let _ = writeln!(std::io::stdout(), "{line}");
        }
        let _ = write!(std::io::stdout(), "{prompt}");
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let mut buf = String::new();
        if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf).is_err() {
            return None;
        }
        Some(buf)
    }
}

impl InteractionHost for LineHost {
    fn emit(&mut self, _kind: HostMessageKind, text: String) {
        Self::emit_stdout(&text);
    }

    fn confirm_planned_payment(&mut self, plan: &PlannedPayment<'_>) -> bool {
        let lines = plan
            .lines()
            .into_iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>();
        match Self::prompt_with_lines(&lines, "  Confirm? [y/N]> ") {
            Some(buf) => matches!(buf.trim(), "y" | "Y" | "yes" | "YES"),
            None => false,
        }
    }

    fn prompt_deposit_claim(&mut self, _wallet: &str, _quote_id: &str) -> bool {
        let lines = vec![
            "Pay the invoice above, then press Enter to claim (or type 'skip')...".to_string(),
        ];
        match Self::prompt_with_lines(&lines, "") {
            Some(buf) => {
                let trimmed = buf.trim();
                !(trimmed == "skip" || trimmed == "s")
            }
            None => false,
        }
    }
}

pub(super) async fn run_interactive_ui(runtime: InteractiveSessionRuntime) {
    let InteractiveSessionRuntime {
        state,
        backend,
        completer,
        history_path,
        intro_messages,
        ..
    } = runtime;

    let mut state = state;
    let mut backend = backend;
    let mut host = LineHost;
    for message in intro_messages {
        host.emit(HostMessageKind::Notice, message);
    }

    let mut editor = match Editor::new() {
        Ok(editor) => editor,
        Err(error) => {
            let _ = writeln!(std::io::stdout(), "Failed to initialize editor: {error}");
            return;
        }
    };
    editor.set_helper(Some(completer));
    let _ = editor.load_history(&history_path);

    loop {
        let prompt = state.prompt();
        match editor.readline(&prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = editor.add_history_entry(trimmed);
                let cmd = match parse_session_command(trimmed, &mut state) {
                    Ok(cmd) => cmd,
                    Err(error) => {
                        if !error.is_empty() {
                            host.emit(HostMessageKind::Notice, error);
                        }
                        continue;
                    }
                };
                if backend.execute(&mut host, &mut state, cmd).await {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
            Err(error) => {
                host.emit(HostMessageKind::Notice, format!("Read error: {error}"));
                break;
            }
        }
    }

    let _ = std::fs::create_dir_all(&state.data_dir);
    let _ = editor.save_history(&history_path);
    host.emit(HostMessageKind::Notice, "Goodbye.".to_string());
}
