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
mod handler;
mod mode;
mod output_fmt;
mod provider;
mod spend;
mod store;
mod types;
mod writer;

use agent_first_data::OutputFormat;
use std::io::Write as _;

#[tokio::main]
async fn main() {
    let mode = match args::parse_args() {
        Ok(mode) => mode,
        Err(error) => {
            let value = agent_first_data::build_cli_error(&error.message, error.hint.as_deref());
            let rendered = agent_first_data::cli_output(&value, OutputFormat::Json);
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(2);
        }
    };

    mode::run(mode).await;
}
