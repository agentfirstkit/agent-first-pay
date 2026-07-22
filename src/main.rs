#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
    )
)]

mod args;
mod config;
mod container;
mod handler;
mod mode;
mod output_fmt;
mod provider;
mod skill_admin;
mod spend;
mod store;
mod types;
mod writer;

use agent_first_data::{CliEmitter, OutputFormat};

#[tokio::main]
async fn main() {
    let _stream_redirect = install_stream_redirect_or_exit();
    let mode = match args::parse_args() {
        Ok(mode) => mode,
        Err(error) => {
            let stdout = std::io::stdout();
            let mut emitter =
                CliEmitter::new(stdout.lock(), OutputFormat::Json).with_strict_protocol();
            if emitter.emit_error("cli_error", &error.message).is_err() {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
    };

    mode::run(mode).await;
}

fn install_stream_redirect_or_exit()
-> Option<agent_first_data::stream_redirect::InstalledStreamRedirect> {
    match agent_first_data::stream_redirect::install_from_raw_args(std::env::args()) {
        Ok(redirect) => redirect,
        Err(err) => {
            let stdout = std::io::stdout();
            let mut emitter =
                CliEmitter::new(stdout.lock(), OutputFormat::Json).with_strict_protocol();
            if emitter.emit_error("cli_error", &err.to_string()).is_err() {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
    }
}
