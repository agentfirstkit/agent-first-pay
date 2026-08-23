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

#[cfg(feature = "rest")]
mod api;
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

use agent_first_data::OutputFormat;

#[tokio::main]
async fn main() {
    if let Err(error) = output_fmt::install_output_to(std::env::args()) {
        let event = output_fmt::coded_error_event("output_setup_failed", &error, None);
        if output_fmt::emit_process_event(event, OutputFormat::Json).is_err() {
            std::process::exit(4);
        }
        std::process::exit(2);
    }
    let _stream_redirect = install_stream_redirect_or_exit();
    let mode = match args::parse_args() {
        Ok(mode) => mode,
        Err(error) => {
            let event =
                output_fmt::coded_error_event(error.code, &error.message, error.hint.as_deref());
            if output_fmt::emit_process_event(event, OutputFormat::Json).is_err() {
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
            let event =
                output_fmt::coded_error_event("output_setup_failed", &err.to_string(), None);
            if output_fmt::emit_process_event(event, OutputFormat::Json).is_err() {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
    }
}
