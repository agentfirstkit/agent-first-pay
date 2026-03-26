use crate::args::InteractiveFrontend;
use crate::handler::{self, App};
#[cfg(feature = "rpc")]
use crate::provider::remote;
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use agent_first_data::OutputFormat;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};
use std::io::Stdout;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

pub(super) const OUTPUT_CHANNEL_CAPACITY: usize = 4096;
pub(super) type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

// ═══════════════════════════════════════════
// Session State
// ═══════════════════════════════════════════

pub(super) struct SessionState {
    pub(super) active_wallet: Option<String>,
    pub(super) active_label: Option<String>,
    pub(super) active_network: Option<Network>,
    request_counter: u64,
    pub(super) data_dir: String,
    pub(super) output_format: OutputFormat,
    pub(super) log_filters: Vec<String>,
    pub(super) store: Option<Arc<StorageBackend>>,
}

impl SessionState {
    pub(super) fn new(
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

    pub(super) fn prompt(&self) -> String {
        match &self.active_label {
            Some(label) => format!("afpay({label})> "),
            None => match &self.active_wallet {
                Some(id) => format!("afpay({id})> "),
                None => "afpay> ".to_string(),
            },
        }
    }

    pub(super) fn next_id(&mut self) -> String {
        self.request_counter += 1;
        format!("session_{}", self.request_counter)
    }
}

// ═══════════════════════════════════════════
// Tab Completion
// ═══════════════════════════════════════════

pub(super) struct CommandCompleter {
    _data_dir: String,
    store: Option<Arc<StorageBackend>>,
}

impl CommandCompleter {
    pub(super) fn new(data_dir: String, store: Option<Arc<StorageBackend>>) -> Self {
        Self {
            _data_dir: data_dir,
            store,
        }
    }

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

    pub(super) fn completion_candidates(&self, line: &str, pos: usize) -> (usize, Vec<String>) {
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
            let matches = filter_candidate_strings(COMMANDS, partial);
            return (word_start, matches);
        }

        let cmd = words[0];

        // "global" prefix
        if cmd == "global" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidate_strings(GLOBAL_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2
                && words[1] == "limit"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidate_strings(GLOBAL_LIMIT_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 3 && words[1] == "limit" && partial.starts_with('-') {
                let flags = vec!["--window", "--max-spend"];
                let matches = filter_candidate_strings(&flags, partial);
                return (word_start, matches);
            }
            if words.len() >= 2
                && words[1] == "config"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidate_strings(GLOBAL_CONFIG_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 3 && words[1] == "config" && partial.starts_with('-') {
                let flags = vec!["--log"];
                let matches = filter_candidate_strings(&flags, partial);
                return (word_start, matches);
            }
            return (pos, vec![]);
        }

        // "cashu" prefix: complete cashu subcommands
        if cmd == "cashu" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidate_strings(CASHU_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidate_strings(CASHU_WALLET_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2 && words[1] == "limit" {
                return network_limit_completion(
                    self, &words, before, partial, word_start, pos, "cashu",
                );
            }
            if words.len() >= 2 && words[1] == "config" {
                return network_config_completion(
                    self, &words, before, partial, word_start, pos, "cashu",
                );
            }
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
                let matches = filter_candidate_strings(&flags, partial);
                return (word_start, matches);
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
                let matches = filter_candidate_strings_owned(&candidates, partial);
                return (word_start, matches);
            }
            return (pos, vec![]);
        }

        // "ln" prefix: complete ln subcommands
        if cmd == "ln" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidate_strings(LN_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidate_strings(LN_WALLET_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2 && words[1] == "limit" {
                return network_limit_completion(
                    self, &words, before, partial, word_start, pos, "ln",
                );
            }
            if words.len() >= 2 && words[1] == "config" {
                return network_config_completion(
                    self, &words, before, partial, word_start, pos, "ln",
                );
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
                let matches = filter_candidate_strings(&flags, partial);
                return (word_start, matches);
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
                let matches = filter_candidate_strings_owned(&candidates, partial);
                return (word_start, matches);
            }
            if prev == "--backend" {
                let matches = filter_candidate_strings(LN_BACKENDS, partial);
                return (word_start, matches);
            }
            return (pos, vec![]);
        }

        // "sol" prefix: complete sol subcommands
        if cmd == "sol" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidate_strings(SOL_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidate_strings(SOL_WALLET_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2 && words[1] == "limit" {
                return network_limit_completion(
                    self, &words, before, partial, word_start, pos, "sol",
                );
            }
            if words.len() >= 2 && words[1] == "config" {
                return network_config_completion(
                    self, &words, before, partial, word_start, pos, "sol",
                );
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
                let matches = filter_candidate_strings(&flags, partial);
                return (word_start, matches);
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
                let matches = filter_candidate_strings_owned(&candidates, partial);
                return (word_start, matches);
            }
            return (pos, vec![]);
        }

        // "evm" prefix: complete evm subcommands
        if cmd == "evm" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidate_strings(EVM_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidate_strings(EVM_WALLET_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2 && words[1] == "limit" {
                return network_limit_completion(
                    self, &words, before, partial, word_start, pos, "evm",
                );
            }
            if words.len() >= 2 && words[1] == "config" {
                return network_config_completion(
                    self, &words, before, partial, word_start, pos, "evm",
                );
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
                let matches = filter_candidate_strings(&flags, partial);
                return (word_start, matches);
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
                let matches = filter_candidate_strings_owned(&candidates, partial);
                return (word_start, matches);
            }
            return (pos, vec![]);
        }

        // "btc" prefix: complete btc subcommands
        if cmd == "btc" {
            if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
                let matches = filter_candidate_strings(BTC_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2
                && words[1] == "wallet"
                && (words.len() == 2 || (words.len() == 3 && !before.ends_with(' ')))
            {
                let matches = filter_candidate_strings(BTC_WALLET_SUBCOMMANDS, partial);
                return (word_start, matches);
            }
            if words.len() >= 2 && words[1] == "limit" {
                return network_limit_completion(
                    self, &words, before, partial, word_start, pos, "btc",
                );
            }
            if words.len() >= 2 && words[1] == "config" {
                return network_config_completion(
                    self, &words, before, partial, word_start, pos, "btc",
                );
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
                let matches = filter_candidate_strings(&flags, partial);
                return (word_start, matches);
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
                let matches = filter_candidate_strings_owned(&candidates, partial);
                return (word_start, matches);
            }
            return (pos, vec![]);
        }

        if words.len() == 1 || (words.len() == 2 && !before.ends_with(' ')) {
            let subs = match cmd {
                "wallet" => Some(filter_candidate_strings(WALLET_TOP_SUBCOMMANDS, partial)),
                "history" => Some(filter_candidate_strings(HISTORY_SUBCOMMANDS, partial)),
                "limit" => Some(filter_candidate_strings(LIMIT_SUBCOMMANDS, partial)),
                "output" => Some(filter_candidate_strings(OUTPUT_FORMATS, partial)),
                "log" => Some(filter_candidate_strings(LOG_SUBCOMMANDS, partial)),
                "use" => {
                    let candidates = self.wallet_candidates();
                    let matches = filter_candidate_strings_owned(&candidates, partial);
                    return (word_start, matches);
                }
                _ => None,
            };
            if let Some(matches) = subs {
                return (word_start, matches);
            }
        }

        if cmd == "wallet"
            && words.len() >= 2
            && words[1] == "close"
            && ((words.len() == 2 && before.ends_with(' '))
                || (words.len() == 3 && !before.ends_with(' ')))
        {
            let candidates = self.wallet_candidates();
            let matches = filter_candidate_strings_owned(&candidates, partial);
            return (word_start, matches);
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
            let matches = filter_candidate_strings_owned(&candidates, partial);
            return (word_start, matches);
        }
        if prev == "--network" {
            let matches = filter_candidate_strings(CURRENCIES, partial);
            return (word_start, matches);
        }

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
                "limit" => vec!["--rule-id"],
                _ => vec![],
            };
            let matches = filter_candidate_strings(&flags, partial);
            return (word_start, matches);
        }

        (pos, vec![])
    }
}

const COMMANDS: &[&str] = &[
    "global", "cashu", "ln", "sol", "evm", "btc", "wallet", "send", "receive", "balance", "use",
    "history", "limit", "output", "log", "help", "quit",
];

const OUTPUT_FORMATS: &[&str] = &["json", "yaml", "plain"];
const LOG_SUBCOMMANDS: &[&str] = &["off", "all", "startup", "cashu", "ln", "sol", "wallet"];

const GLOBAL_SUBCOMMANDS: &[&str] = &["limit", "config"];
const GLOBAL_LIMIT_SUBCOMMANDS: &[&str] = &["add"];
const GLOBAL_CONFIG_SUBCOMMANDS: &[&str] = &["show", "set"];
const CASHU_SUBCOMMANDS: &[&str] = &[
    "send", "receive", "balance", "wallet", "restore", "limit", "config",
];
const CASHU_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const LN_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance", "limit", "config"];
const LN_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const LN_BACKENDS: &[&str] = &["nwc", "phoenixd", "lnbits"];
const SOL_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance", "limit", "config"];
const SOL_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const EVM_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance", "limit", "config"];
const EVM_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const BTC_SUBCOMMANDS: &[&str] = &["wallet", "send", "receive", "balance", "limit", "config"];
const BTC_WALLET_SUBCOMMANDS: &[&str] = &["create", "close", "list", "dangerously-show-seed"];
const WALLET_TOP_SUBCOMMANDS: &[&str] = &["list"];
const HISTORY_SUBCOMMANDS: &[&str] = &["list", "status", "update"];
const LIMIT_SUBCOMMANDS: &[&str] = &["remove", "list"];
const NETWORK_LIMIT_SUBCOMMANDS: &[&str] = &["add"];
const SIMPLE_CONFIG_SUBCOMMANDS: &[&str] = &["show", "set"];
const TOKEN_CONFIG_SUBCOMMANDS: &[&str] = &["show", "set", "token-add", "token-remove"];
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
        let (word_start, matches) = self.completion_candidates(line, pos);
        let pairs = matches
            .into_iter()
            .map(|candidate| Pair {
                display: candidate.clone(),
                replacement: candidate,
            })
            .collect();
        Ok((word_start, pairs))
    }
}

/// Shared completion logic for `<network> limit [--wallet <w>] add --flags...`
fn network_limit_completion(
    completer: &CommandCompleter,
    words: &[&str],
    before: &str,
    partial: &str,
    word_start: usize,
    pos: usize,
    network: &str,
) -> (usize, Vec<String>) {
    // Determine the effective subcommand position, skipping --wallet <val>
    // e.g. "cashu limit add" or "cashu limit --wallet w1 add"
    let mut sub_pos = 2; // first word after "limit"
    if words.len() > sub_pos && words[sub_pos] == "--wallet" {
        sub_pos += 2; // skip "--wallet" and its value
    }

    // Complete subcommand: "cashu limit <TAB>" or "cashu limit --wallet w1 <TAB>"
    if words.len() <= sub_pos || (words.len() == sub_pos + 1 && !before.ends_with(' ')) {
        // Also offer --wallet if not yet specified
        let mut candidates: Vec<String> =
            filter_candidate_strings(NETWORK_LIMIT_SUBCOMMANDS, partial);
        if !words.contains(&"--wallet") && "--wallet".starts_with(partial) {
            candidates.push("--wallet".to_string());
        }
        return (word_start, candidates);
    }

    // Complete --wallet value
    let prev = if before.ends_with(' ') {
        words.last().copied().unwrap_or("")
    } else if words.len() >= 2 {
        words[words.len() - 2]
    } else {
        ""
    };
    if prev == "--wallet" {
        let candidates = completer.wallet_candidates();
        let matches = filter_candidate_strings_owned(&candidates, partial);
        return (word_start, matches);
    }

    // Complete flags for "add"
    if partial.starts_with('-') {
        let flags = match network {
            "sol" | "evm" => vec!["--token", "--window", "--max-spend", "--wallet"],
            _ => vec!["--window", "--max-spend", "--wallet"],
        };
        let matches = filter_candidate_strings(&flags, partial);
        return (word_start, matches);
    }

    (pos, vec![])
}

/// Shared completion logic for `<network> config --wallet <w> show/set/...`
fn network_config_completion(
    completer: &CommandCompleter,
    words: &[&str],
    before: &str,
    partial: &str,
    word_start: usize,
    pos: usize,
    network: &str,
) -> (usize, Vec<String>) {
    let has_tokens = matches!(network, "sol" | "evm");
    let subcommands = if has_tokens {
        TOKEN_CONFIG_SUBCOMMANDS
    } else {
        SIMPLE_CONFIG_SUBCOMMANDS
    };

    // Skip --wallet <val> to find subcommand position
    let mut sub_pos = 2;
    if words.len() > sub_pos && words[sub_pos] == "--wallet" {
        sub_pos += 2;
    }

    // Complete subcommand or --wallet flag
    if words.len() <= sub_pos || (words.len() == sub_pos + 1 && !before.ends_with(' ')) {
        let mut candidates: Vec<String> = filter_candidate_strings(subcommands, partial);
        if !words.contains(&"--wallet") && "--wallet".starts_with(partial) {
            candidates.push("--wallet".to_string());
        }
        return (word_start, candidates);
    }

    // Complete --wallet value
    let prev = if before.ends_with(' ') {
        words.last().copied().unwrap_or("")
    } else if words.len() >= 2 {
        words[words.len() - 2]
    } else {
        ""
    };
    if prev == "--wallet" {
        let candidates = completer.wallet_candidates();
        let matches = filter_candidate_strings_owned(&candidates, partial);
        return (word_start, matches);
    }

    // Complete flags per network
    if partial.starts_with('-') {
        let flags = match network {
            "evm" => vec![
                "--label",
                "--rpc-endpoint",
                "--chain-id",
                "--symbol",
                "--address",
                "--decimals",
                "--wallet",
            ],
            "sol" => vec![
                "--label",
                "--rpc-endpoint",
                "--symbol",
                "--address",
                "--decimals",
                "--wallet",
            ],
            _ => vec!["--label", "--wallet"],
        };
        let matches = filter_candidate_strings(&flags, partial);
        return (word_start, matches);
    }

    (pos, vec![])
}

fn filter_candidate_strings(options: &[&str], partial: &str) -> Vec<String> {
    options
        .iter()
        .filter(|s| s.starts_with(partial))
        .map(|s| s.to_string())
        .collect()
}

fn filter_candidate_strings_owned(options: &[String], partial: &str) -> Vec<String> {
    options
        .iter()
        .filter(|s| s.starts_with(partial))
        .cloned()
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
pub(super) enum SessionCommand {
    Dispatch(Input),
    Use(String),
    Session(String, Vec<String>),
    Help,
    Quit,
}

pub(super) fn parse_session_command(
    line: &str,
    state: &mut SessionState,
) -> Result<SessionCommand, String> {
    let parsed =
        shell_words::split(line).map_err(|e| format!("invalid command line syntax: {e}"))?;
    let parts: Vec<&str> = parsed.iter().map(|s| s.as_str()).collect();
    if parts.is_empty() {
        return Err(String::new());
    }

    let cmd = parts[0];
    let args = &parts[1..];

    match cmd {
        "help" | "?" => Ok(SessionCommand::Help),
        "quit" | "exit" => Ok(SessionCommand::Quit),
        "output" | "log" => Ok(SessionCommand::Session(
            cmd.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        )),

        "use" => {
            if args.is_empty() {
                Ok(SessionCommand::Use(String::new()))
            } else {
                Ok(SessionCommand::Use(args[0].to_string()))
            }
        }

        _ => dispatch_to_cli(&parts, state),
    }
}

// ═══════════════════════════════════════════
// CLI Delegation
// ═══════════════════════════════════════════

fn dispatch_to_cli(parts: &[&str], state: &mut SessionState) -> Result<SessionCommand, String> {
    let mut argv: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    inject_wallet(&mut argv, state);
    let id = state.next_id();
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    match crate::args::parse_subcommand(&refs, &id) {
        Ok(input) => Ok(SessionCommand::Dispatch(input)),
        Err(e) => Err(e),
    }
}

fn inject_wallet(argv: &mut Vec<String>, state: &SessionState) {
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
// Session Backend
// ═══════════════════════════════════════════

pub(super) fn render_output(output: &Output, format: OutputFormat) -> String {
    let value = serde_json::to_value(output).unwrap_or(serde_json::Value::Null);
    crate::output_fmt::render_value_with_policy(&value, format)
}

fn render_value(value: &serde_json::Value, format: OutputFormat) -> String {
    crate::output_fmt::render_value_with_policy(value, format)
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

#[derive(Clone, Copy)]
pub(super) enum HostMessageKind {
    Output,
    Notice,
}

pub(super) trait InteractionHost {
    fn emit(&mut self, kind: HostMessageKind, text: String);
    fn confirm_send(&mut self, wallet: &str, amount: u64, to: &str) -> bool;
    fn confirm_send_with_fee(
        &mut self,
        wallet: &str,
        amount: u64,
        fee: u64,
        fee_unit: &str,
    ) -> bool;
    fn confirm_withdraw(
        &mut self,
        wallet: &str,
        amount: u64,
        fee_estimate: u64,
        fee_unit: &str,
        to: &str,
    ) -> bool;
    fn prompt_deposit_claim(&mut self, wallet: &str, quote_id: &str) -> bool;
}

pub(super) enum SessionBackend {
    Local {
        app: Arc<App>,
        rx: mpsc::Receiver<Output>,
    },
    #[cfg(feature = "rpc")]
    Remote { endpoint: String, secret: String },
}

impl SessionBackend {
    pub(super) fn connection_label(&self) -> String {
        match self {
            Self::Local { .. } => "local".to_string(),
            #[cfg(feature = "rpc")]
            Self::Remote { endpoint, .. } => format!("remote: {endpoint}"),
        }
    }

    pub(super) async fn execute<H: InteractionHost>(
        &mut self,
        host: &mut H,
        state: &mut SessionState,
        cmd: SessionCommand,
    ) -> bool {
        match self {
            Self::Local { app, rx } => execute_local_command(host, state, app, rx, cmd).await,
            #[cfg(feature = "rpc")]
            Self::Remote { endpoint, secret } => {
                execute_remote_command(host, state, endpoint, secret, cmd).await
            }
        }
    }

    /// Dispatch an Input and return structured outputs as JSON values.
    /// Unlike execute(), this bypasses InteractionHost and returns data directly.
    pub(super) async fn query(
        &mut self,
        _state: &mut SessionState,
        input: Input,
    ) -> Vec<serde_json::Value> {
        match self {
            Self::Local { app, rx } => {
                handler::dispatch(app, input).await;
                tokio::task::yield_now().await;
                let mut results = Vec::new();
                while let Ok(output) = rx.try_recv() {
                    if let Ok(value) = serde_json::to_value(&output) {
                        results.push(value);
                    }
                }
                results
            }
            #[cfg(feature = "rpc")]
            Self::Remote { endpoint, secret } => remote::rpc_call(endpoint, secret, &input).await,
        }
    }

    pub(super) fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    /// Spawn dispatch (Local). Results arrive via shared app.writer → rx.
    pub(super) fn spawn_local(&self, input: Input) -> tokio::task::JoinHandle<()> {
        let Self::Local { app, .. } = self else {
            unreachable!("spawn_local called on Remote backend")
        };
        let app = app.clone();
        tokio::spawn(async move {
            handler::dispatch(&app, input).await;
        })
    }

    /// Spawn query (Remote). Results returned via JoinHandle.
    #[cfg(feature = "rpc")]
    pub(super) fn spawn_remote(
        &self,
        input: Input,
    ) -> tokio::task::JoinHandle<Vec<serde_json::Value>> {
        let Self::Remote { endpoint, secret } = self else {
            unreachable!("spawn_remote called on Local backend")
        };
        let endpoint = endpoint.clone();
        let secret = secret.clone();
        tokio::spawn(async move { remote::rpc_call(&endpoint, &secret, &input).await })
    }

    /// Non-blocking drain of pending outputs from rx (Local only).
    pub(super) fn try_recv_outputs(&mut self) -> Vec<serde_json::Value> {
        let Self::Local { rx, .. } = self else {
            return vec![];
        };
        let mut results = Vec::new();
        while let Ok(output) = rx.try_recv() {
            if let Ok(value) = serde_json::to_value(&output) {
                results.push(value);
            }
        }
        results
    }
}

async fn execute_local_command<H: InteractionHost>(
    host: &mut H,
    state: &mut SessionState,
    app: &Arc<App>,
    rx: &mut mpsc::Receiver<Output>,
    cmd: SessionCommand,
) -> bool {
    match cmd {
        SessionCommand::Quit => return true,
        SessionCommand::Help => host.emit(HostMessageKind::Notice, help_text()),
        SessionCommand::Use(target) => resolve_use_local(host, state, &target),
        SessionCommand::Session(name, args) => {
            handle_session_command(host, state, &name, &args);
            app.config.write().await.log = state.log_filters.clone();
        }
        SessionCommand::Dispatch(input) => {
            let write_qr_svg_file_request = matches!(
                &input,
                Input::Receive {
                    write_qr_svg_file: true,
                    ..
                }
            );

            let needs_confirm = matches!(&input, Input::CashuSend { .. } | Input::Send { .. });
            if needs_confirm {
                let confirmed = match &input {
                    Input::CashuSend { wallet, amount, .. } => {
                        let wallet_name = wallet.as_deref().unwrap_or("");
                        let mut got_quote = false;
                        let mut confirmed = false;
                        for provider in app.providers.values() {
                            if let Ok(q) = provider.cashu_send_quote(wallet_name, amount).await {
                                got_quote = true;
                                confirmed = host.confirm_send_with_fee(
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
                            host.confirm_send(display, amount.value, "P2P cashu token")
                        } else {
                            confirmed
                        }
                    }
                    Input::Send { wallet, to, .. } => {
                        let wallet_name = wallet.as_deref().unwrap_or("");
                        let mut got_quote = false;
                        let mut confirmed = false;
                        for provider in app.providers.values() {
                            if let Ok(q) = provider.send_quote(wallet_name, to, None).await {
                                got_quote = true;
                                confirmed = host.confirm_withdraw(
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
                            host.emit(
                                HostMessageKind::Notice,
                                "  Could not get melt quote; skipping confirmation.".to_string(),
                            );
                            true
                        } else {
                            confirmed
                        }
                    }
                    _ => true,
                };
                if !confirmed {
                    host.emit(HostMessageKind::Notice, "Cancelled.".to_string());
                    return false;
                }
            }

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

            handler::dispatch(app, input).await;
            tokio::task::yield_now().await;

            let mut deposit_quote_id = None;
            while let Ok(output) = rx.try_recv() {
                if let Output::Log { ref event, .. } = output {
                    if !log_matches(&state.log_filters, event) {
                        continue;
                    }
                }
                if let Output::ReceiveInfo {
                    ref receive_info, ..
                } = output
                {
                    if let Some(qid) = &receive_info.quote_id {
                        deposit_quote_id = Some(qid.clone());
                    }
                }
                host.emit(
                    HostMessageKind::Output,
                    render_output(&output, state.output_format),
                );
                maybe_save_qr_svg_from_output(
                    host,
                    &output,
                    &state.data_dir,
                    state.output_format,
                    write_qr_svg_file_request,
                );
            }

            if do_follow_up {
                if let (Some(wallet), Some(quote_id)) = (deposit_wallet, deposit_quote_id) {
                    if host.prompt_deposit_claim(&wallet, &quote_id) {
                        let claim_id = state.next_id();
                        let input = Input::ReceiveClaim {
                            id: claim_id,
                            wallet,
                            quote_id,
                        };
                        handler::dispatch(app, input).await;
                        tokio::task::yield_now().await;
                        collect_and_emit(
                            host,
                            rx,
                            state.output_format,
                            &state.log_filters,
                            &state.data_dir,
                            false,
                        );
                    } else {
                        host.emit(
                            HostMessageKind::Notice,
                            format!(
                                "Skipped. To claim later: receive --network cashu --wallet {wallet} --ln-quote-id {quote_id}"
                            ),
                        );
                    }
                }
            }
        }
    }

    false
}

#[cfg(feature = "rpc")]
async fn execute_remote_command<H: InteractionHost>(
    host: &mut H,
    state: &mut SessionState,
    endpoint: &str,
    secret: &str,
    cmd: SessionCommand,
) -> bool {
    match cmd {
        SessionCommand::Quit => return true,
        SessionCommand::Help => host.emit(HostMessageKind::Notice, help_text()),
        SessionCommand::Use(target) => {
            resolve_use_remote(host, state, endpoint, secret, &target).await
        }
        SessionCommand::Session(name, args) => handle_session_command(host, state, &name, &args),
        SessionCommand::Dispatch(input) => {
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
                host,
                &outputs,
                &state.data_dir,
                state.output_format,
                &state.log_filters,
                write_qr_svg_file_request,
            );
        }
    }

    false
}

fn handle_session_command<H: InteractionHost>(
    host: &mut H,
    state: &mut SessionState,
    name: &str,
    args: &[String],
) {
    match name {
        "output" => match args.first().map(|s| s.as_str()) {
            Some("json") => {
                state.output_format = OutputFormat::Json;
                host.emit(HostMessageKind::Notice, "Output: json".to_string());
            }
            Some("yaml") => {
                state.output_format = OutputFormat::Yaml;
                host.emit(HostMessageKind::Notice, "Output: yaml".to_string());
            }
            Some("plain") => {
                state.output_format = OutputFormat::Plain;
                host.emit(HostMessageKind::Notice, "Output: plain".to_string());
            }
            _ => {
                host.emit(
                    HostMessageKind::Notice,
                    format!(
                        "Output: {:?}. Usage: output <json|yaml|plain>",
                        state.output_format
                    ),
                );
            }
        },
        "log" => {
            if args.is_empty() {
                if state.log_filters.is_empty() {
                    host.emit(
                        HostMessageKind::Notice,
                        "Log: off. Usage: log <filters...> (e.g. log startup,cashu) or log off"
                            .to_string(),
                    );
                } else {
                    host.emit(
                        HostMessageKind::Notice,
                        format!("Log: {}", state.log_filters.join(",")),
                    );
                }
            } else if args.len() == 1 && args[0] == "off" {
                state.log_filters.clear();
                host.emit(HostMessageKind::Notice, "Log: off".to_string());
            } else {
                let joined: Vec<&str> = args.iter().flat_map(|a| a.split(',')).collect();
                let as_strings: Vec<String> = joined.iter().map(|s| s.to_string()).collect();
                state.log_filters = agent_first_data::cli_parse_log_filters(&as_strings);
                host.emit(
                    HostMessageKind::Notice,
                    format!("Log: {}", state.log_filters.join(",")),
                );
            }
        }
        _ => {}
    }
}

fn resolve_use_local<H: InteractionHost>(host: &mut H, state: &mut SessionState, target: &str) {
    if target.is_empty() {
        state.active_wallet = None;
        state.active_label = None;
        state.active_network = None;
        host.emit(
            HostMessageKind::Notice,
            "Cleared active wallet.".to_string(),
        );
        return;
    }

    let Some(store) = &state.store else {
        host.emit(
            HostMessageKind::Notice,
            "No storage backend available.".to_string(),
        );
        return;
    };

    match store.list_wallet_metadata(None) {
        Ok(wallets) => {
            for wallet in &wallets {
                if wallet.id == target {
                    state.active_wallet = Some(wallet.id.clone());
                    state.active_label = wallet.label.clone();
                    state.active_network = Some(wallet.network);
                    let label_display = wallet
                        .label
                        .as_deref()
                        .map(|label| format!(" ({label})"))
                        .unwrap_or_default();
                    host.emit(
                        HostMessageKind::Notice,
                        format!("Active wallet: {}{label_display}", wallet.id),
                    );
                    return;
                }
                if wallet.label.as_deref() == Some(target) {
                    state.active_wallet = Some(wallet.id.clone());
                    state.active_label = Some(target.to_string());
                    state.active_network = Some(wallet.network);
                    host.emit(
                        HostMessageKind::Notice,
                        format!("Active wallet: {} ({target})", wallet.id),
                    );
                    return;
                }
            }

            host.emit(
                HostMessageKind::Notice,
                format!("Wallet not found: {target}"),
            );
            if !wallets.is_empty() {
                let mut lines = vec!["Available:".to_string()];
                for wallet in &wallets {
                    let label = wallet.label.as_deref().unwrap_or("-");
                    lines.push(format!("  {} ({label})", wallet.id));
                }
                host.emit(HostMessageKind::Notice, lines.join("\n"));
            }
        }
        Err(error) => {
            host.emit(
                HostMessageKind::Notice,
                format!("Error listing wallets: {error}"),
            );
        }
    }
}

#[cfg(feature = "rpc")]
async fn resolve_use_remote<H: InteractionHost>(
    host: &mut H,
    state: &mut SessionState,
    endpoint: &str,
    secret: &str,
    target: &str,
) {
    if target.is_empty() {
        state.active_wallet = None;
        state.active_label = None;
        state.active_network = None;
        host.emit(
            HostMessageKind::Notice,
            "Cleared active wallet.".to_string(),
        );
        return;
    }

    let list_input = Input::WalletList {
        id: state.next_id(),
        network: None,
    };
    let outputs = remote::rpc_call(endpoint, secret, &list_input).await;
    let mut found = false;
    for value in &outputs {
        if value.get("code").and_then(|v| v.as_str()) == Some("error") {
            emit_remote_outputs_with_qr(
                host,
                &outputs,
                &state.data_dir,
                state.output_format,
                &state.log_filters,
                false,
            );
            return;
        }

        if let Some(wallets) = value.get("wallets").and_then(|v| v.as_array()) {
            for wallet in wallets {
                let wallet_id = wallet.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let wallet_label = wallet.get("label").and_then(|v| v.as_str());
                if wallet_id == target || wallet_label == Some(target) {
                    state.active_wallet = Some(wallet_id.to_string());
                    state.active_label = wallet_label.map(|value| value.to_string());
                    state.active_network = wallet
                        .get("network")
                        .and_then(|v| v.as_str())
                        .and_then(network_from_str);
                    let label_display = wallet_label
                        .map(|label| format!(" ({label})"))
                        .unwrap_or_default();
                    host.emit(
                        HostMessageKind::Notice,
                        format!("Active wallet: {wallet_id}{label_display}"),
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

    if !found {
        host.emit(
            HostMessageKind::Notice,
            format!("Wallet not found: {target}"),
        );
    }
}

pub(super) fn network_from_str(value: &str) -> Option<Network> {
    match value {
        "ln" => Some(Network::Ln),
        "sol" => Some(Network::Sol),
        "evm" => Some(Network::Evm),
        "cashu" => Some(Network::Cashu),
        "btc" => Some(Network::Btc),
        _ => None,
    }
}

fn collect_and_emit<H: InteractionHost>(
    host: &mut H,
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
        host.emit(HostMessageKind::Output, render_output(&output, format));
        maybe_save_qr_svg_from_output(host, &output, data_dir, format, should_write_qr_svg_file);
    }
}

fn maybe_save_qr_svg_from_output<H: InteractionHost>(
    host: &mut H,
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
        emit_qr_saved_message(
            host,
            kind,
            write_qr_svg_file(data_dir, kind, &payload),
            format,
        );
    }
}

fn maybe_save_qr_svg_from_remote_value<H: InteractionHost>(
    host: &mut H,
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
        emit_qr_saved_message(
            host,
            kind,
            write_qr_svg_file(data_dir, kind, &payload),
            format,
        );
    }
}

fn emit_qr_saved_message<H: InteractionHost>(
    host: &mut H,
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

    host.emit(HostMessageKind::Output, render_value(&value, output_format));
}

fn emit_remote_outputs_with_qr<H: InteractionHost>(
    host: &mut H,
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
        host.emit(HostMessageKind::Output, render_value(value, format));
        maybe_save_qr_svg_from_remote_value(
            host,
            value,
            data_dir,
            format,
            should_write_qr_svg_file,
        );
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

fn help_text() -> String {
    format!(
        "{}\n\n{}",
        crate::args::subcommand_help(&["--help"]),
        "\
Session:
  use <wallet_id|label>       Set active wallet (auto-injects --wallet)
  output <json|yaml|plain>    Switch output format
  log <filters|off>           Log filter (e.g. log startup,cashu)
  <cmd> --help                Detailed help for a command
  help                        This help
  quit                        Exit"
    )
}

// ═══════════════════════════════════════════
// Main Loop
// ═══════════════════════════════════════════

pub(super) fn mode_name(frontend: InteractiveFrontend) -> &'static str {
    match frontend {
        InteractiveFrontend::Interactive => "interactive",
        InteractiveFrontend::Tui => "tui",
    }
}

pub(super) fn banner_hint(frontend: InteractiveFrontend) -> &'static str {
    match frontend {
        InteractiveFrontend::Interactive => {
            "Type 'help' for commands, Tab for completion, Ctrl-D to exit."
        }
        InteractiveFrontend::Tui => {
            "Tab switches panes, Enter edits, F5 runs actions, Ctrl-C exits."
        }
    }
}

pub(super) fn save_history_entries(path: &str, history: &[String]) {
    let content = history.join("\n");
    let _ = std::fs::write(path, content);
}

pub(super) fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(value.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_wallet_adds_active_wallet_once() {
        let state = SessionState {
            active_wallet: Some("wallet_123".to_string()),
            active_label: None,
            active_network: None,
            request_counter: 0,
            data_dir: "/tmp".to_string(),
            output_format: OutputFormat::Json,
            log_filters: vec![],
            store: None,
        };
        let mut argv = vec!["wallet".to_string(), "list".to_string()];
        inject_wallet(&mut argv, &state);
        assert_eq!(
            argv,
            vec![
                "wallet".to_string(),
                "list".to_string(),
                "--wallet".to_string(),
                "wallet_123".to_string()
            ]
        );
    }

    #[test]
    fn lightning_prefix_is_added_once() {
        assert_eq!(
            add_lightning_prefix("lnbc123"),
            "lightning:lnbc123".to_string()
        );
        assert_eq!(
            add_lightning_prefix("lightning:lnbc123"),
            "lightning:lnbc123".to_string()
        );
    }
}
