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

#[tokio::main]
async fn main() {
    let args::Parsed {
        mode,
        redirect: _stream_redirect,
    } = match args::parse_args() {
        Ok(parsed) => parsed,
        Err(error) => {
            let event =
                output_fmt::coded_error_event(error.code, &error.message, error.hint.as_deref());
            if output_fmt::emit_stderr_error(event).is_err() {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
    };

    mode::run(mode).await;
}
