use crate::cli::InteractiveInit;
use crate::config::VERSION;
use crate::handler::{self, App};
use crate::provider::remote;
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use agent_first_data::OutputFormat;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const OUTPUT_CHANNEL_CAPACITY: usize = 4096;

// ═══════════════════════════════════════════
// REPL State
// ═══════════════════════════════════════════

struct ReplState {
    active_wallet: Option<String>,
    active_label: Option<String>,
    active_network: Option<Network>,
    request_counter: u64,
    data_dir: String,
    output_format: OutputFormat,
    log_filters: Vec<String>,
    store: Option<Arc<StorageBackend>>,
}

impl ReplState {
    fn new(
        data_dir: String,
        output_format: OutputFormat,
        log_filters: Vec<String>,
        store: Option<Arc<StorageBackend>>,
    ) -> Self {
        Self {
            active_wallet: None,
            active_label: None,
            active_network: None,
            request_counter: 0,
            data_dir,
            output_format,
            log_filters,
            store,
        }
    }

    fn prompt(&self) -> String {
        match &self.active_label {
            Some(label) => format!("afpay({label})> "),
            None => match &self.active_wallet {
                Some(id) => format!("afpay({id})> "),
                None => "afpay> ".to_string(),
            },
        }
    }

    fn next_id(&mut self) -> String {
        self.request_counter += 1;
        format!("repl_{}", self.request_counter)
    }
}

// ═══════════════════════════════════════════
// Tab Completion
// ═══════════════════════════════════════════

struct CommandCompleter {
    _data_dir: String,
    store: Option<Arc<StorageBackend>>,
}

impl CommandCompleter {
    fn wallet_candidates(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(store) = &self.store {
            if let Ok(wallets) = store.list_wallet_metadata(None) {
                for w in wallets {
                    names.push(w.id.clone());
                    if let Some(label) = &w.label {
                        names.push(label.clone());
                    }
                }
            }
        }
        names
    }
}

const COMMANDS: &[&str] = &[
    "cashu", "ln", "sol", "evm", "btc", "wallet", "send", "receive", "balance", "use", "history",
    "limit", "output", "log", "help", "quit",
];

const OUTPUT_FORMATS: &[&str] = &["json", "yaml", "plain"];
const LOG_SUBCOMMANDS: &[&str] = &["off", "all", "startup", "cashu", "ln", "sol", "wallet"];

const CASHU_SUBCOMMANDS: &[&str] = &["send", "receive", "balance", "wallet", "restore"];
const CASHU_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const LN_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance"];
const LN_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const LN_BACKENDS: &[&str] = &["nwc", "phoenixd", "lnbits"];
const SOL_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance"];
const SOL_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const EVM_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance"];
const EVM_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const BTC_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance"];
const BTC_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const WALLET_TOP_SUBCOMMANDS: &[&str] = &["list", "close", "config"];
const HISTORY_SUBCOMMANDS: &[&str] = &["list", "status", "update"];
const LIMIT_SUBCOMMANDS: &[&str] = &["set", "get"];
const CURRENCIES: &[&str] = &["ln", "sol", "evm", "cashu", "btc"];
const DANGEROUS_SKIP_BALANCE_CHECK_FLAG: &str =
    "--dangerously-skip-balance-check-and-may-lose-money";

impl Completer for CommandCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let before = &line[..pos];
        let words: Vec<&str> = before.split_whitespace().collect();
        let partial = if before.ends_with(' ') {
            ""
        } else {
            words.last().copied().unwrap_or("")
        };
        let word_start = pos - partial.len();

        // word 0: command names
        if words.is_empty() || (words.len() == 1 && !before.ends_with(' ')) {
            let matches = filter_candidates(COMMANDS, partial);
            return Ok((word_start, matches));
        }

        let cmd = words[0];

        // "cashu" prefix: complete cashu subcommands
        if cmd == "cashu" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidates(CASHU_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            // "cashu wallet" → complete wallet subcommands
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidates(CASHU_WALLET_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            // Flags for cashu subcommands
            if words.len() >= 2 && partial.starts_with('-') {
                let cashu_sub = words[1];
                let flags = match cashu_sub {
                    "send" => vec![
                        "--wallet",
                        "--onchain-memo",
                        "--local-memo",
                        "--amount-sats",
                        "--cashu-mint",
                    ],
                    "receive" => vec!["--wallet"],
                    "balance" => vec!["--wallet", "--check"],
                    "restore" => vec!["--wallet"],
                    "wallet" => vec![
                        "--cashu-mint",
                        "--label",
                        "--wallet",
                        DANGEROUS_SKIP_BALANCE_CHECK_FLAG,
                    ],
                    _ => vec![],
                };
                let matches = filter_candidates(&flags, partial);
                return Ok((word_start, matches));
            }
            // Flag values for cashu subcommands
            let prev = if before.ends_with(' ') {
                words.last().copied().unwrap_or("")
            } else if words.len() >= 2 {
                words[words.len() - 2]
            } else {
                ""
            };
            if prev == "--wallet" {
                let candidates = self.wallet_candidates();
                let matches = filter_candidates_owned(&candidates, partial);
                return Ok((word_start, matches));
            }
            return Ok((pos, vec![]));
        }

        // "ln" prefix: complete ln subcommands
        if cmd == "ln" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidates(LN_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidates(LN_WALLET_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2 && partial.starts_with('-') {
                let ln_sub = words[1];
                let flags = match ln_sub {
                    "send" => vec!["--wallet", "--to", "--onchain-memo", "--local-memo"],
                    "receive" => vec![
                        "--wallet",
                        "--amount-sats",
                        "--wait-until-paid",
                        "--wait-timeout-s",
                        "--wait-poll-interval-ms",
                        "--qr-svg-file",
                    ],
                    "balance" => vec!["--wallet"],
                    "wallet" => vec![
                        "--backend",
                        "--nwc-uri-secret",
                        "--endpoint",
                        "--password-secret",
                        "--admin-key-secret",
                        "--label",
                        "--wallet",
                        DANGEROUS_SKIP_BALANCE_CHECK_FLAG,
                    ],
                    _ => vec![],
                };
                let matches = filter_candidates(&flags, partial);
                return Ok((word_start, matches));
            }
            let prev = if before.ends_with(' ') {
                words.last().copied().unwrap_or("")
            } else if words.len() >= 2 {
                words[words.len() - 2]
            } else {
                ""
            };
            if prev == "--wallet" {
                let candidates = self.wallet_candidates();
                let matches = filter_candidates_owned(&candidates, partial);
                return Ok((word_start, matches));
            }
            if prev == "--backend" {
                let matches = filter_candidates(LN_BACKENDS, partial);
                return Ok((word_start, matches));
            }
            return Ok((pos, vec![]));
        }

        // "sol" prefix: complete sol subcommands
        if cmd == "sol" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidates(SOL_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidates(SOL_WALLET_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2 && partial.starts_with('-') {
                let sol_sub = words[1];
                let flags = match sol_sub {
                    "send" => vec![
                        "--wallet",
                        "--to",
                        "--amount-lamports",
                        "--onchain-memo",
                        "--local-memo",
                    ],
                    "receive" => vec![
                        "--wallet",
                        "--onchain-memo",
                        "--wait",
                        "--wait-timeout-s",
                        "--wait-poll-interval-ms",
                        "--qr-svg-file",
                    ],
                    "balance" => vec!["--wallet"],
                    "wallet" => vec![
                        "--sol-rpc-endpoint",
                        "--label",
                        "--wallet",
                        DANGEROUS_SKIP_BALANCE_CHECK_FLAG,
                    ],
                    _ => vec![],
                };
                let matches = filter_candidates(&flags, partial);
                return Ok((word_start, matches));
            }
            let prev = if before.ends_with(' ') {
                words.last().copied().unwrap_or("")
            } else if words.len() >= 2 {
                words[words.len() - 2]
            } else {
                ""
            };
            if prev == "--wallet" {
                let candidates = self.wallet_candidates();
                let matches = filter_candidates_owned(&candidates, partial);
                return Ok((word_start, matches));
            }
            return Ok((pos, vec![]));
        }

        // "evm" prefix: complete evm subcommands
        if cmd == "evm" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidates(EVM_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidates(EVM_WALLET_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2 && partial.starts_with('-') {
                let evm_sub = words[1];
                let flags = match evm_sub {
                    "send" => vec![
                        "--wallet",
                        "--to",
                        "--amount-gwei",
                        "--amount-wei",
                        "--token",
                        "--onchain-memo",
                        "--local-memo",
                    ],
                    "receive" => vec![
                        "--wallet",
                        "--onchain-memo",
                        "--wait",
                        "--wait-timeout-s",
                        "--wait-poll-interval-ms",
                    ],
                    "balance" => vec!["--wallet"],
                    "wallet" => vec![
                        "--evm-rpc-endpoint",
                        "--chain-id",
                        "--label",
                        "--wallet",
                        DANGEROUS_SKIP_BALANCE_CHECK_FLAG,
                    ],
                    _ => vec![],
                };
                let matches = filter_candidates(&flags, partial);
                return Ok((word_start, matches));
            }
            let prev = if before.ends_with(' ') {
                words.last().copied().unwrap_or("")
            } else if words.len() >= 2 {
                words[words.len() - 2]
            } else {
                ""
            };
            if prev == "--wallet" {
                let candidates = self.wallet_candidates();
                let matches = filter_candidates_owned(&candidates, partial);
                return Ok((word_start, matches));
            }
            return Ok((pos, vec![]));
        }

        // "btc" prefix: complete btc subcommands
        if cmd == "btc" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidates(BTC_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidates(BTC_WALLET_SUBCOMMANDS, partial);
                return Ok((word_start, matches));
            }
            if words.len() >= 2 && partial.starts_with('-') {
                let btc_sub = words[1];
                let flags = match btc_sub {
                    "send" => vec![
                        "--wallet",
                        "--to",
                        "--amount",
                        "--onchain-memo",
                        "--local-memo",
                    ],
                    "receive" => vec![
                        "--wallet",
                        "--wait",
                        "--wait-timeout-s",
                        "--wait-poll-interval-ms",
                    ],
                    "balance" => vec!["--wallet"],
                    "wallet" => vec![
                        "--btc-network",
                        "--btc-address-type",
                        "--btc-esplora-url",
                        "--mnemonic-secret",
                        "--label",
                        "--wallet",
                        DANGEROUS_SKIP_BALANCE_CHECK_FLAG,
                    ],
                    _ => vec![],
                };
                let matches = filter_candidates(&flags, partial);
                return Ok((word_start, matches));
            }
            let prev = if before.ends_with(' ') {
                words.last().copied().unwrap_or("")
            } else if words.len() >= 2 {
                words[words.len() - 2]
            } else {
                ""
            };
            if prev == "--wallet" {
                let candidates = self.wallet_candidates();
                let matches = filter_candidates_owned(&candidates, partial);
                return Ok((word_start, matches));
            }
            return Ok((pos, vec![]));
        }

        // word 1: subcommands for non-cashu commands
        if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
            let subs = match cmd {
                "wallet" => Some(WALLET_TOP_SUBCOMMANDS),
                "history" => Some(HISTORY_SUBCOMMANDS),
                "limit" => Some(LIMIT_SUBCOMMANDS),
                "output" => Some(OUTPUT_FORMATS),
                "log" => Some(LOG_SUBCOMMANDS),
                "use" => {
                    let candidates = self.wallet_candidates();
                    let matches = filter_candidates_owned(&candidates, partial);
                    return Ok((word_start, matches));
                }
                _ => None,
            };
            if let Some(subs) = subs {
                let matches = filter_candidates(subs, partial);
                return Ok((word_start, matches));
            }
        }

        // Positional wallet ID for: wallet close <wallet_id>
        if cmd == "wallet"
            && words.len() >= 2
            && words[1] == "close"
            && ((words.len() == 2 && before.ends_with(' '))
                || (words.len() == 3 && !before.ends_with(' ')))
        {
            let candidates = self.wallet_candidates();
            let matches = filter_candidates_owned(&candidates, partial);
            return Ok((word_start, matches));
        }

        // word N: flag values or flag names
        let prev = if before.ends_with(' ') {
            words.last().copied().unwrap_or("")
        } else if words.len() >= 2 {
            words[words.len() - 2]
        } else {
            ""
        };

        // Complete flag values
        if prev == "--wallet" {
            let candidates = self.wallet_candidates();
            let matches = filter_candidates_owned(&candidates, partial);
            return Ok((word_start, matches));
        }
        if prev == "--network" {
            let matches = filter_candidates(CURRENCIES, partial);
            return Ok((word_start, matches));
        }

        // Complete flag names
        if partial.starts_with('-') {
            let flags = match cmd {
                "send" => vec![
                    "--wallet",
                    "--network",
                    "--to",
                    "--amount",
                    "--token",
                    "--onchain-memo",
                    "--local-memo",
                    "--cashu-mint",
                ],
                "receive" => vec![
                    "--wallet",
                    "--network",
                    "--cashu-token",
                    "--amount",
                    "--token",
                    "--onchain-memo",
                    "--wait",
                    "--wait-timeout-s",
                    "--wait-poll-interval-ms",
                    "--qr-svg-file",
                    "--ln-quote-id",
                ],
                "balance" => vec!["--wallet", "--network", "--cashu-check"],
                "wallet" => {
                    if words.len() >= 2 && words[1] == "close" {
                        vec![DANGEROUS_SKIP_BALANCE_CHECK_FLAG]
                    } else {
                        vec!["--network"]
                    }
                }
                "history" => vec![
                    "--wallet",
                    "--transaction-id",
                    "--onchain-memo",
                    "--local-memo",
                    "--limit",
                    "--offset",
                ],
                "limit" => vec!["--rule"],
                _ => vec![],
            };
            let matches = filter_candidates(&flags, partial);
            return Ok((word_start, matches));
        }

        Ok((pos, vec![]))
    }
}

fn filter_candidates(options: &[&str], partial: &str) -> Vec<Pair> {
    options
        .iter()
        .filter(|s| s.starts_with(partial))
        .map(|s| Pair {
            display: s.to_string(),
            replacement: s.to_string(),
        })
        .collect()
}

fn filter_candidates_owned(options: &[String], partial: &str) -> Vec<Pair> {
    options
        .iter()
        .filter(|s| s.starts_with(partial))
        .map(|s| Pair {
            display: s.clone(),
            replacement: s.clone(),
        })
        .collect()
}

impl Hinter for CommandCompleter {
    type Hint = String;
}
impl Highlighter for CommandCompleter {}
impl Validator for CommandCompleter {}
impl Helper for CommandCompleter {}

// ═══════════════════════════════════════════
// Command Parsing
// ═══════════════════════════════════════════

#[allow(clippy::large_enum_variant)]
enum ReplCommand {
    Dispatch(Input),
    Use(String),
    Session(String, Vec<String>),
    Help,
    Quit,
}

fn parse_repl_command(line: &str, state: &mut ReplState) -> Result<ReplCommand, String> {
    let parsed =
        shell_words::split(line).map_err(|e| format!("invalid command line syntax: {e}"))?;
    let parts: Vec<&str> = parsed.iter().map(|s| s.as_str()).collect();
    if parts.is_empty() {
        return Err(String::new());
    }

    let cmd = parts[0];
    let args = &parts[1..];

    match cmd {
        "help" | "?" => Ok(ReplCommand::Help),
        "quit" | "exit" => Ok(ReplCommand::Quit),
        "output" | "log" => Ok(ReplCommand::Session(
            cmd.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        )),

        "use" => {
            if args.is_empty() {
                Ok(ReplCommand::Use(String::new()))
            } else {
                Ok(ReplCommand::Use(args[0].to_string()))
            }
        }

        _ => dispatch_to_cli(&parts, state),
    }
}

// ═══════════════════════════════════════════
// CLI Delegation
// ═══════════════════════════════════════════

fn dispatch_to_cli(parts: &[&str], state: &mut ReplState) -> Result<ReplCommand, String> {
    let mut argv: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    inject_wallet(&mut argv, state);
    let id = state.next_id();
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    match crate::cli::parse_subcommand(&refs, &id) {
        Ok(input) => Ok(ReplCommand::Dispatch(input)),
        Err(e) => Err(e),
    }
}

fn inject_wallet(argv: &mut Vec<String>, state: &ReplState) {
    if state.active_wallet.is_some() && !argv.iter().any(|a| a == "--wallet") {
        argv.push("--wallet".to_string());
        argv.push(state.active_wallet.clone().unwrap_or_default());
    }
}

// ═══════════════════════════════════════════
// QR Code Rendering
// ═══════════════════════════════════════════

#[cfg(feature = "interactive")]
fn render_qr_svg(data: &str) -> Result<String, String> {
    use qrcode::render::svg;
    use qrcode::QrCode;

    let code = QrCode::new(data.as_bytes()).map_err(|e| format!("QR encode error: {e}"))?;
    let rendered = code
        .render::<svg::Color<'_>>()
        .min_dimensions(320, 320)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .quiet_zone(true)
        .build();
    Ok(rendered)
}

fn write_qr_svg_file(data_dir: &str, kind: &str, payload: &str) -> Result<String, String> {
    let directory = Path::new(data_dir).join("qr-codes");
    std::fs::create_dir_all(&directory)
        .map_err(|e| format!("create qr-codes dir {}: {e}", directory.display()))?;

    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let file_name = format!("{kind}-{timestamp_ms}.svg");
    let file_path = directory.join(file_name);
    let svg = render_qr_svg(payload)?;
    std::fs::write(&file_path, svg)
        .map_err(|e| format!("write qr svg {}: {e}", file_path.display()))?;
    Ok(file_path.display().to_string())
}

// ═══════════════════════════════════════════
// Output Formatting (via agent_first_data)
// ═══════════════════════════════════════════

fn emit(output: &Output, format: OutputFormat) {
    let value = serde_json::to_value(output).unwrap_or(serde_json::Value::Null);
    let text = crate::output_fmt::render_value_with_policy(&value, format);
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn log_matches(filters: &[String], event: &str) -> bool {
    if filters.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    filters
        .iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}

// ═══════════════════════════════════════════
// Multi-step Flows
// ═══════════════════════════════════════════

fn confirm_send(wallet: &str, amount: u64, to: &str) -> bool {
    let target = if to.is_empty() {
        "P2P cashu token".to_string()
    } else if to.len() > 40 {
        format!("{}...", &to[..40])
    } else {
        to.to_string()
    };
    let _ = writeln!(
        std::io::stdout(),
        "  Send {amount} sats from {wallet} to {target}"
    );
    let _ = write!(std::io::stdout(), "  Confirm? [y/N]> ");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut buf = String::new();
    if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf).is_err() {
        return false;
    }
    matches!(buf.trim(), "y" | "Y" | "yes" | "YES")
}

fn confirm_send_with_fee(wallet: &str, amount: u64, fee: u64, fee_unit: &str) -> bool {
    let total = amount + fee;
    let _ = writeln!(
        std::io::stdout(),
        "  Send {amount} {fee_unit} from {wallet} as P2P cashu token"
    );
    if fee > 0 {
        let _ = writeln!(
            std::io::stdout(),
            "  Fee: {fee} {fee_unit}  (total: {total} {fee_unit})"
        );
    }
    let _ = write!(std::io::stdout(), "  Confirm? [y/N]> ");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut buf = String::new();
    if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf).is_err() {
        return false;
    }
    matches!(buf.trim(), "y" | "Y" | "yes" | "YES")
}

fn confirm_withdraw(
    wallet: &str,
    amount: u64,
    fee_estimate: u64,
    fee_unit: &str,
    to: &str,
) -> bool {
    let target = if to.len() > 40 {
        format!("{}...", &to[..40])
    } else {
        to.to_string()
    };
    let total = amount + fee_estimate;
    let _ = writeln!(
        std::io::stdout(),
        "  Pay {amount} {fee_unit} from {wallet} to {target}"
    );
    let _ = writeln!(
        std::io::stdout(),
        "  Fee estimate: {fee_estimate} {fee_unit}  (total: {total} {fee_unit})"
    );
    let _ = write!(std::io::stdout(), "  Confirm? [y/N]> ");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut buf = String::new();
    if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf).is_err() {
        return false;
    }
    matches!(buf.trim(), "y" | "Y" | "yes" | "YES")
}

struct OutputRenderContext<'a> {
    format: OutputFormat,
    log_filters: &'a [String],
    data_dir: &'a str,
}

async fn deposit_follow_up(
    app: &App,
    rx: &mut mpsc::Receiver<Output>,
    wallet: &str,
    quote_id: &str,
    request_id: &str,
    render: OutputRenderContext<'_>,
) {
    let _ = writeln!(
        std::io::stdout(),
        "Pay the invoice above, then press Enter to claim (or type 'skip')..."
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut buf = String::new();
    if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf).is_err() {
        return;
    }
    let trimmed = buf.trim();
    if trimmed == "skip" || trimmed == "s" {
        let _ = writeln!(
            std::io::stdout(),
            "Skipped. To claim later: receive --network cashu --wallet {wallet} --ln-quote-id {quote_id}"
        );
        return;
    }

    // Auto-claim
    let input = Input::ReceiveClaim {
        id: request_id.to_string(),
        wallet: wallet.to_string(),
        quote_id: quote_id.to_string(),
    };
    handler::dispatch(app, input).await;
    tokio::task::yield_now().await;
    collect_and_print(
        rx,
        render.format,
        render.log_filters,
        render.data_dir,
        false,
    );
}

fn collect_and_print(
    rx: &mut mpsc::Receiver<Output>,
    format: OutputFormat,
    log_filters: &[String],
    data_dir: &str,
    should_write_qr_svg_file: bool,
) {
    while let Ok(output) = rx.try_recv() {
        if let Output::Log { ref event, .. } = output {
            if !log_matches(log_filters, event) {
                continue;
            }
        }
        emit(&output, format);
        maybe_save_qr_svg_from_output(&output, data_dir, format, should_write_qr_svg_file);
    }
}

fn maybe_save_qr_svg_from_output(
    output: &Output,
    data_dir: &str,
    format: OutputFormat,
    should_write_qr_svg_file: bool,
) {
    if !should_write_qr_svg_file {
        return;
    }
    let qr_payload = match output {
        Output::ReceiveInfo { receive_info, .. } => wallet_deposit_qr_payload(
            receive_info.invoice.as_deref(),
            receive_info.address.as_deref(),
        ),
        _ => None,
    };
    if let Some((kind, payload)) = qr_payload {
        emit_qr_saved_message(kind, write_qr_svg_file(data_dir, kind, &payload), format);
    }
}

fn maybe_save_qr_svg_from_remote_value(
    value: &serde_json::Value,
    data_dir: &str,
    format: OutputFormat,
    should_write_qr_svg_file: bool,
) {
    if !should_write_qr_svg_file {
        return;
    }
    let code = value
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let qr_payload = match code {
        "wallet_deposit" => wallet_deposit_qr_payload(
            value
                .get("receive_info")
                .and_then(|v| v.get("invoice"))
                .and_then(|v| v.as_str()),
            value
                .get("receive_info")
                .and_then(|v| v.get("address"))
                .and_then(|v| v.as_str()),
        ),
        _ => None,
    };
    if let Some((kind, payload)) = qr_payload {
        emit_qr_saved_message(kind, write_qr_svg_file(data_dir, kind, &payload), format);
    }
}

fn emit_qr_saved_message(
    kind: &str,
    file_result: Result<String, String>,
    output_format: OutputFormat,
) {
    let value = match file_result {
        Ok(path) => serde_json::json!({
            "code": "log",
            "event": "qr",
            "args": {
                "kind": kind,
                "svg_file": path,
            },
            "trace": {"duration_ms": 0},
        }),
        Err(error) => serde_json::json!({
            "code": "error",
            "error_code": "internal_error",
            "error": format!("qr svg generation failed: {error}"),
            "retryable": false,
            "trace": {"duration_ms": 0},
        }),
    };
    let _ = writeln!(
        std::io::stdout(),
        "{}",
        crate::output_fmt::render_value_with_policy(&value, output_format)
    );
}

fn emit_remote_outputs_with_qr(
    outputs: &[serde_json::Value],
    data_dir: &str,
    format: OutputFormat,
    log_filters: &[String],
    should_write_qr_svg_file: bool,
) {
    for value in outputs {
        if let Some("log") = value.get("code").and_then(|v| v.as_str()) {
            if let Some(event) = value.get("event").and_then(|v| v.as_str()) {
                if !log_matches(log_filters, event) {
                    continue;
                }
            }
        }
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            crate::output_fmt::render_value_with_policy(value, format)
        );
        maybe_save_qr_svg_from_remote_value(value, data_dir, format, should_write_qr_svg_file);
    }
}

fn add_lightning_prefix(invoice: &str) -> String {
    if invoice.starts_with("lightning:") {
        invoice.to_string()
    } else if invoice.starts_with("lnbc")
        || invoice.starts_with("lntb")
        || invoice.starts_with("lnbcrt")
    {
        format!("lightning:{invoice}")
    } else {
        invoice.to_string()
    }
}

fn wallet_deposit_qr_payload(
    invoice: Option<&str>,
    address: Option<&str>,
) -> Option<(&'static str, String)> {
    if let Some(invoice) = invoice {
        return Some(("lightning_invoice", add_lightning_prefix(invoice)));
    }
    address.map(|value| ("receive_address", value.to_string()))
}

// ═══════════════════════════════════════════
// Help
// ═══════════════════════════════════════════

fn print_help() {
    let _ = write!(
        std::io::stdout(),
        "{}",
        crate::cli::subcommand_help(&["--help"])
    );
    let _ = writeln!(std::io::stdout());

    let _ = writeln!(
        std::io::stdout(),
        "\
Session:
  use <wallet_id|label>       Set active wallet (auto-injects --wallet)
  output <json|yaml|plain>    Switch output format
  log <filters|off>           Log filter (e.g. log startup,cashu)
  <cmd> --help                Detailed help for a command
  help                        This help
  quit                        Exit"
    );
}

// ═══════════════════════════════════════════
// Main Loop
// ═══════════════════════════════════════════

pub async fn run_interactive(init: InteractiveInit) {
    let InteractiveInit {
        output,
        log,
        data_dir,
        rpc_endpoint,
        rpc_secret,
    } = init;

    // If rpc_endpoint is set, run in remote mode (no local App/providers)
    if let Some(ref endpoint) = rpc_endpoint {
        run_interactive_remote(
            endpoint,
            rpc_secret.as_deref(),
            output,
            &log,
            data_dir.as_deref(),
        )
        .await;
        return;
    }

    let resolved_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(std::io::stdout(), "config error: {e}");
            return;
        }
    };

    let data_dir_owned = config.data_dir.clone();
    let log_filters = agent_first_data::cli_parse_log_filters(&log);
    config.log = log_filters.clone();

    if let Some(startup) = crate::config::maybe_startup_log(
        &log_filters,
        false,
        None,
        Some(&config),
        serde_json::json!({
            "mode": "interactive",
            "backend": "local",
            "data_dir": config.data_dir,
        }),
    ) {
        emit(&startup, output);
    }

    let startup_errors = handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        emit(error_output, output);
    }
    if !startup_errors.is_empty() {
        return;
    }

    let (tx, mut rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let store = crate::store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, None, store));
    let store_ref = app.store.clone();

    let mut state = ReplState::new(
        data_dir_owned.clone(),
        output,
        log_filters,
        store_ref.clone(),
    );

    let completer = CommandCompleter {
        _data_dir: data_dir_owned.clone(),
        store: store_ref,
    };
    let mut rl = match Editor::new() {
        Ok(editor) => editor,
        Err(e) => {
            let _ = writeln!(std::io::stdout(), "Failed to initialize editor: {e}");
            return;
        }
    };
    rl.set_helper(Some(completer));

    let history_path = format!("{data_dir_owned}/.afpay_history");
    let _ = rl.load_history(&history_path);

    let _ = writeln!(std::io::stdout(), "afpay v{VERSION} interactive mode");
    let _ = writeln!(
        std::io::stdout(),
        "Type 'help' for commands, Tab for completion, Ctrl-D to exit.\n"
    );

    loop {
        let prompt = state.prompt();
        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(trimmed);

                let cmd = match parse_repl_command(trimmed, &mut state) {
                    Ok(c) => c,
                    Err(e) => {
                        if !e.is_empty() {
                            let _ = writeln!(std::io::stdout(), "{e}");
                        }
                        continue;
                    }
                };

                match cmd {
                    ReplCommand::Quit => break,
                    ReplCommand::Help => print_help(),
                    ReplCommand::Use(target) => {
                        resolve_use(&mut state, &target);
                    }
                    ReplCommand::Session(name, args) => {
                        handle_session_command(&mut state, &name, &args);
                        // Sync log_filters to config so handler can read them
                        app.config.write().await.log = state.log_filters.clone();
                    }
                    ReplCommand::Dispatch(input) => {
                        let write_qr_svg_file_request = matches!(
                            &input,
                            Input::Receive {
                                write_qr_svg_file: true,
                                ..
                            }
                        );

                        // Send/Pay confirmation
                        let needs_confirm =
                            matches!(&input, Input::CashuSend { .. } | Input::Send { .. });
                        if needs_confirm {
                            let confirmed = match &input {
                                Input::CashuSend { wallet, amount, .. } => {
                                    let w = wallet.as_deref().unwrap_or("");
                                    let mut got_quote = false;
                                    let mut ok = false;
                                    for provider in app.providers.values() {
                                        if let Ok(q) = provider.cashu_send_quote(w, amount).await {
                                            got_quote = true;
                                            ok = confirm_send_with_fee(
                                                &q.wallet,
                                                q.amount_native,
                                                q.fee_native,
                                                &q.fee_unit,
                                            );
                                            break;
                                        }
                                    }
                                    if !got_quote {
                                        let display = wallet.as_deref().unwrap_or("auto");
                                        confirm_send(display, amount.value, "P2P cashu token")
                                    } else {
                                        ok
                                    }
                                }
                                Input::Send { wallet, to, .. } => {
                                    let w = wallet.as_deref().unwrap_or("");
                                    let mut got_quote = false;
                                    let mut ok = false;
                                    for provider in app.providers.values() {
                                        if let Ok(q) = provider.send_quote(w, to, None).await {
                                            got_quote = true;
                                            ok = confirm_withdraw(
                                                &q.wallet,
                                                q.amount_native,
                                                q.fee_estimate_native,
                                                &q.fee_unit,
                                                to,
                                            );
                                            break;
                                        }
                                    }
                                    if !got_quote {
                                        let _ = writeln!(
                                            std::io::stdout(),
                                            "  Could not get melt quote; skipping confirmation."
                                        );
                                        true
                                    } else {
                                        ok
                                    }
                                }
                                _ => true,
                            };
                            if !confirmed {
                                let _ = writeln!(std::io::stdout(), "Cancelled.");
                                continue;
                            }
                        }

                        // Check if this is a deposit (for follow-up).
                        // Skip follow-up when request already asks handler-side wait-until-paid.
                        let (deposit_wallet, do_follow_up) = match &input {
                            Input::Receive {
                                wallet,
                                network,
                                wait_until_paid,
                                wait_timeout_s,
                                wait_poll_interval_ms,
                                ..
                            } => {
                                let wait_requested = *wait_until_paid
                                    || wait_timeout_s.is_some()
                                    || wait_poll_interval_ms.is_some();
                                let is_ln = *network == Some(Network::Ln);
                                (Some(wallet.clone()), !wait_requested && !is_ln)
                            }
                            _ => (None, false),
                        };
                        handler::dispatch(&app, input).await;
                        tokio::task::yield_now().await;

                        // Collect outputs
                        let mut deposit_quote_id = None;
                        while let Ok(output) = rx.try_recv() {
                            if let Output::Log { ref event, .. } = output {
                                if !log_matches(&state.log_filters, event) {
                                    continue;
                                }
                            }
                            // Capture quote_id for deposit follow-up
                            if let Output::ReceiveInfo {
                                ref receive_info, ..
                            } = output
                            {
                                if let Some(qid) = &receive_info.quote_id {
                                    deposit_quote_id = Some(qid.clone());
                                }
                            }
                            emit(&output, state.output_format);
                            maybe_save_qr_svg_from_output(
                                &output,
                                &state.data_dir,
                                state.output_format,
                                write_qr_svg_file_request,
                            );
                        }

                        // Deposit follow-up flow
                        if do_follow_up {
                            if let (Some(wallet), Some(quote_id)) =
                                (deposit_wallet, deposit_quote_id)
                            {
                                let claim_id = state.next_id();
                                let render = OutputRenderContext {
                                    format: state.output_format,
                                    log_filters: &state.log_filters,
                                    data_dir: &state.data_dir,
                                };
                                deposit_follow_up(
                                    &app, &mut rx, &wallet, &quote_id, &claim_id, render,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                break;
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(e) => {
                let _ = writeln!(std::io::stdout(), "Read error: {e}");
                break;
            }
        }
    }

    // Save history (ensure data dir exists)
    let _ = std::fs::create_dir_all(&data_dir_owned);
    let _ = rl.save_history(&history_path);
    let _ = writeln!(std::io::stdout(), "Goodbye.");
}

fn handle_session_command(state: &mut ReplState, name: &str, args: &[String]) {
    match name {
        "output" => match args.first().map(|s| s.as_str()) {
            Some("json") => {
                state.output_format = OutputFormat::Json;
                let _ = writeln!(std::io::stdout(), "Output: json");
            }
            Some("yaml") => {
                state.output_format = OutputFormat::Yaml;
                let _ = writeln!(std::io::stdout(), "Output: yaml");
            }
            Some("plain") => {
                state.output_format = OutputFormat::Plain;
                let _ = writeln!(std::io::stdout(), "Output: plain");
            }
            _ => {
                let _ = writeln!(
                    std::io::stdout(),
                    "Output: {:?}. Usage: output <json|yaml|plain>",
                    state.output_format
                );
            }
        },
        "log" => {
            if args.is_empty() {
                if state.log_filters.is_empty() {
                    let _ = writeln!(
                        std::io::stdout(),
                        "Log: off. Usage: log <filters...> (e.g. log startup,cashu) or log off"
                    );
                } else {
                    let _ = writeln!(std::io::stdout(), "Log: {}", state.log_filters.join(","));
                }
            } else if args.len() == 1 && args[0] == "off" {
                state.log_filters.clear();
                let _ = writeln!(std::io::stdout(), "Log: off");
            } else {
                let joined: Vec<&str> = args.iter().flat_map(|a| a.split(',')).collect();
                let as_strings: Vec<String> = joined.iter().map(|s| s.to_string()).collect();
                state.log_filters = agent_first_data::cli_parse_log_filters(&as_strings);
                let _ = writeln!(std::io::stdout(), "Log: {}", state.log_filters.join(","));
            }
        }
        _ => {}
    }
}

fn resolve_use(state: &mut ReplState, target: &str) {
    if target.is_empty() {
        state.active_wallet = None;
        state.active_label = None;
        state.active_network = None;
        let _ = writeln!(std::io::stdout(), "Cleared active wallet.");
        return;
    }

    let Some(store) = &state.store else {
        let _ = writeln!(std::io::stdout(), "No storage backend available.");
        return;
    };
    let wallets_result = store.list_wallet_metadata(None);

    match wallets_result {
        Ok(wallets) => {
            for w in &wallets {
                if w.id == target {
                    state.active_wallet = Some(w.id.clone());
                    state.active_label = w.label.clone();
                    state.active_network = Some(w.network);
                    let label_display = w
                        .label
                        .as_deref()
                        .map(|l| format!(" ({l})"))
                        .unwrap_or_default();
                    let _ = writeln!(std::io::stdout(), "Active wallet: {}{label_display}", w.id);
                    return;
                }
                if w.label.as_deref() == Some(target) {
                    state.active_wallet = Some(w.id.clone());
                    state.active_label = Some(target.to_string());
                    state.active_network = Some(w.network);
                    let _ = writeln!(std::io::stdout(), "Active wallet: {} ({target})", w.id);
                    return;
                }
            }
            let _ = writeln!(std::io::stdout(), "Wallet not found: {target}");
            if !wallets.is_empty() {
                let _ = writeln!(std::io::stdout(), "Available:");
                for w in &wallets {
                    let label = w.label.as_deref().unwrap_or("-");
                    let _ = writeln!(std::io::stdout(), "  {} ({label})", w.id);
                }
            }
        }
        Err(e) => {
            let _ = writeln!(std::io::stdout(), "Error listing wallets: {e}");
        }
    }
}

// ═══════════════════════════════════════════
// Remote Interactive Mode
// ═══════════════════════════════════════════

async fn run_interactive_remote(
    endpoint: &str,
    rpc_secret: Option<&str>,
    output: OutputFormat,
    log: &[String],
    data_dir: Option<&str>,
) {
    let (endpoint, secret) = remote::require_remote_args(Some(endpoint), rpc_secret, output);

    let log_filters = agent_first_data::cli_parse_log_filters(log);
    let resolved_dir = data_dir
        .map(ToString::to_string)
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut local_config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(std::io::stdout(), "config error: {e}");
            return;
        }
    };
    local_config.log = log_filters.clone();

    let store_ref = crate::store::create_storage_backend(&local_config).map(Arc::new);
    let mut state = ReplState::new(
        local_config.data_dir.clone(),
        output,
        log_filters,
        store_ref.clone(),
    );

    let completer = CommandCompleter {
        _data_dir: local_config.data_dir.clone(),
        store: store_ref,
    };
    let mut rl = match Editor::new() {
        Ok(editor) => editor,
        Err(e) => {
            let _ = writeln!(std::io::stdout(), "Failed to initialize editor: {e}");
            return;
        }
    };
    rl.set_helper(Some(completer));

    if let Some(startup) = crate::config::maybe_startup_log(
        &state.log_filters,
        false,
        None,
        Some(&local_config),
        serde_json::json!({
            "mode": "interactive",
            "backend": "remote",
            "rpc_endpoint": endpoint,
            "data_dir": local_config.data_dir,
        }),
    ) {
        emit(&startup, state.output_format);
    }

    // Version check remote endpoint to verify connectivity and compatibility
    let ping_outputs = remote::rpc_call(endpoint, secret, &Input::Version).await;
    for value in &ping_outputs {
        if value.get("code").and_then(|v| v.as_str()) == Some("error") {
            let err_msg = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            let err = Output::Error {
                id: None,
                error_code: "provider_unreachable".to_string(),
                error: format!("remote version check failed: {err_msg}"),
                hint: value
                    .get("hint")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                retryable: true,
                trace: Trace::from_duration(0),
            };
            emit(&err, output);
            return;
        }
        if value.get("code").and_then(|v| v.as_str()) == Some("version") {
            let remote_version = value
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if remote_version != VERSION {
                let err = Output::Error {
                    id: None,
                    error_code: "version_mismatch".to_string(),
                    error: format!("version mismatch: local v{VERSION}, remote v{remote_version}"),
                    hint: Some("upgrade both client and server to the same version".to_string()),
                    retryable: false,
                    trace: Trace::from_duration(0),
                };
                emit(&err, output);
                return;
            }
        }
    }

    let _ = writeln!(
        std::io::stdout(),
        "afpay v{VERSION} interactive mode (remote: {endpoint})"
    );
    let _ = writeln!(
        std::io::stdout(),
        "Type 'help' for commands, Tab for completion, Ctrl-D to exit.\n"
    );

    loop {
        let prompt = state.prompt();
        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(trimmed);

                let cmd = match parse_repl_command(trimmed, &mut state) {
                    Ok(c) => c,
                    Err(e) => {
                        if !e.is_empty() {
                            let _ = writeln!(std::io::stdout(), "{e}");
                        }
                        continue;
                    }
                };

                match cmd {
                    ReplCommand::Quit => break,
                    ReplCommand::Help => print_help(),
                    ReplCommand::Use(target) => {
                        if target.is_empty() {
                            state.active_wallet = None;
                            state.active_label = None;
                            state.active_network = None;
                            let _ = writeln!(std::io::stdout(), "Cleared active wallet.");
                        } else {
                            // In remote mode, resolve `use` via wallet_list RPC
                            let list_input = Input::WalletList {
                                id: state.next_id(),
                                network: None,
                            };
                            let outputs = remote::rpc_call(endpoint, secret, &list_input).await;
                            let mut found = false;
                            for val in &outputs {
                                if val.get("code").and_then(|v| v.as_str()) == Some("error") {
                                    remote::emit_remote_outputs(
                                        &outputs,
                                        state.output_format,
                                        &state.log_filters,
                                    );
                                    break;
                                }
                                if let Some(wallets) = val.get("wallets").and_then(|v| v.as_array())
                                {
                                    for w in wallets {
                                        let wid =
                                            w.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                        let wlabel = w.get("label").and_then(|v| v.as_str());
                                        if wid == target || wlabel == Some(&target) {
                                            state.active_wallet = Some(wid.to_string());
                                            state.active_label = wlabel.map(|s| s.to_string());
                                            let cur_str = w
                                                .get("network")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            state.active_network = match cur_str {
                                                "ln" => Some(Network::Ln),
                                                "sol" => Some(Network::Sol),
                                                "evm" => Some(Network::Evm),
                                                "cashu" => Some(Network::Cashu),
                                                "btc" => Some(Network::Btc),
                                                _ => None,
                                            };
                                            let label_display = wlabel
                                                .map(|l| format!(" ({l})"))
                                                .unwrap_or_default();
                                            let _ = writeln!(
                                                std::io::stdout(),
                                                "Active wallet: {wid}{label_display}"
                                            );
                                            found = true;
                                            break;
                                        }
                                    }
                                }
                                if found {
                                    break;
                                }
                            }
                            if !found
                                && !outputs.iter().any(|v| {
                                    v.get("code").and_then(|v| v.as_str()) == Some("error")
                                })
                            {
                                let _ = writeln!(std::io::stdout(), "Wallet not found: {target}");
                            }
                        }
                    }
                    ReplCommand::Session(name, args) => {
                        handle_session_command(&mut state, &name, &args);
                    }
                    ReplCommand::Dispatch(input) => {
                        let write_qr_svg_file_request = matches!(
                            &input,
                            Input::Receive {
                                write_qr_svg_file: true,
                                ..
                            }
                        );
                        let mut outputs = remote::rpc_call(endpoint, secret, &input).await;
                        remote::wrap_remote_limit_topology(&mut outputs, endpoint);
                        emit_remote_outputs_with_qr(
                            &outputs,
                            &state.data_dir,
                            state.output_format,
                            &state.log_filters,
                            write_qr_svg_file_request,
                        );
                    }
                }
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
            Err(e) => {
                let _ = writeln!(std::io::stdout(), "Read error: {e}");
                break;
            }
        }
    }

    let _ = writeln!(std::io::stdout(), "Goodbye.");
}
