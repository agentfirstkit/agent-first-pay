use super::session::{
    char_to_byte_index, mode_name, network_from_str, parse_session_command, save_history_entries,
    HostMessageKind, InteractionHost, SessionBackend, SessionState, TuiTerminal,
};
use super::InteractiveSessionRuntime;
use crate::args::InteractiveFrontend;
use crate::config::VERSION;
use crate::provider::remote;
use crate::store::PayStore;
use crate::types::{Input, Network};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::cmp::min;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::Duration;

#[cfg(test)]
const NETWORK_OPTIONS: &[&str] = &["cashu", "ln", "sol", "evm", "btc"];

#[allow(clippy::vec_init_then_push)] // push gated by #[cfg(feature)]
fn enabled_networks() -> Vec<Network> {
    let mut nets = Vec::new();
    #[cfg(feature = "cashu")]
    nets.push(Network::Cashu);
    #[cfg(any(feature = "ln-nwc", feature = "ln-phoenixd", feature = "ln-lnbits"))]
    nets.push(Network::Ln);
    #[cfg(feature = "sol")]
    nets.push(Network::Sol);
    #[cfg(feature = "evm")]
    nets.push(Network::Evm);
    #[cfg(any(
        feature = "btc-esplora",
        feature = "btc-core",
        feature = "btc-electrum"
    ))]
    nets.push(Network::Btc);
    nets
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum TuiMessageKind {
    Command,
    Output,
    Notice,
}

struct TuiMessage {
    kind: TuiMessageKind,
    text: String,
}

struct TuiModal {
    title: String,
    lines: Vec<String>,
    hint: String,
}

#[derive(Clone)]
struct TuiWalletEntry {
    id: String,
    label: Option<String>,
    network: Option<Network>,
}

impl TuiWalletEntry {
    fn display_short(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => {
                if self.id.len() > 10 {
                    format!("{}...", &self.id[..10])
                } else {
                    self.id.clone()
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Wallet tree (2-level: network group → wallets)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct WalletGroup {
    network: Network,
    expanded: bool,
    wallets: Vec<TuiWalletEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarItem {
    Group(usize),
    Wallet(usize, usize),
    LimitHeader,
    Limit(usize),
    ConfigHeader,
    DataHeader,
}

fn build_wallet_groups(wallets: Vec<TuiWalletEntry>) -> Vec<WalletGroup> {
    let mut groups: Vec<WalletGroup> = enabled_networks()
        .into_iter()
        .map(|network| WalletGroup {
            network,
            expanded: true,
            wallets: Vec::new(),
        })
        .collect();

    for wallet in wallets {
        if let Some(network) = wallet.network {
            if let Some(group) = groups.iter_mut().find(|g| g.network == network) {
                group.wallets.push(wallet);
            }
        }
    }

    // Sort wallets within each group by label/id
    for group in &mut groups {
        group.wallets.sort_by(|a, b| {
            let a_key = a.label.as_deref().unwrap_or(&a.id);
            let b_key = b.label.as_deref().unwrap_or(&b.id);
            a_key.cmp(b_key)
        });
    }

    groups
}

// ---------------------------------------------------------------------------
// Focus & view model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TuiFocus {
    Sidebar,
    Main,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TuiView {
    WalletDetail,
    GroupSummary,
    Send,
    Receive,
    WalletCreate,
    WalletClose,
    WalletShowSeed,
    History,
    HistoryDetail,
    Limits,
    LimitDetail,
    LimitAdd,
    WalletConfig,
    GlobalConfig,
    CommandResult,
    DataView,
}

// ---------------------------------------------------------------------------
// Data backup / restore state
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
enum DataOpStatus {
    Idle,
    Running,
    Done(String),
    Error(String),
}

struct DataPending {
    is_backup: bool,
    handle: tokio::task::JoinHandle<Result<String, String>>,
}

// ---------------------------------------------------------------------------
// Async pending query
// ---------------------------------------------------------------------------

enum PendingQueryKind {
    WalletDetail(String),
    GroupSummary(Network),
    History,
    HistoryDetail,
    Limits,
}

enum PendingQueryHandle {
    Local(tokio::task::JoinHandle<()>),
    Remote(tokio::task::JoinHandle<Vec<serde_json::Value>>),
}

struct PendingQuery {
    kind: PendingQueryKind,
    handle: PendingQueryHandle,
}

// ---------------------------------------------------------------------------
// Wallet detail data (auto-fetched)
// ---------------------------------------------------------------------------

struct WalletViewData {
    wallet_id: Option<String>,
    network: Option<String>,
    label: Option<String>,
    mint_url: Option<String>,
    address: Option<String>,
    backend: Option<String>,
    created_at: Option<String>,
    balance_text: Option<String>,
    balance_error: Option<String>,
}

impl WalletViewData {
    fn empty() -> Self {
        Self {
            wallet_id: None,
            network: None,
            label: None,
            mint_url: None,
            address: None,
            backend: None,
            created_at: None,
            balance_text: None,
            balance_error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Group summary data (per-network balance totals)
// ---------------------------------------------------------------------------

struct GroupSummaryData {
    network: String,
    wallet_count: usize,
    confirmed: u64,
    pending: u64,
    unit: String,
    errors: usize,
    wallets: Vec<GroupWalletLine>,
}

struct GroupWalletLine {
    label: String,
    balance: String,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// History display record (parsed from query output)
// ---------------------------------------------------------------------------

struct HistoryDisplayRecord {
    transaction_id: String,
    wallet: Option<String>,
    direction: String,
    amount: String,
    status: String,
    date: String,
    memo: Option<String>,
    local_memo: Option<String>,
}

// ---------------------------------------------------------------------------
// Limit display record
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct LimitDisplayRecord {
    rule_id: String,
    scope: String,
    max_spend: String,
    spent: String,
    remaining: String,
    window: String,
}

// ---------------------------------------------------------------------------
// Form field model (preserved from original)
// ---------------------------------------------------------------------------

#[derive(Clone)]
#[allow(dead_code)]
enum TuiFieldValue {
    Text(String),
    Choice {
        options: &'static [&'static str],
        selected: usize,
    },
    Toggle(bool),
}

impl TuiFieldValue {
    fn display_value(&self, placeholder: &str) -> (String, bool) {
        match self {
            Self::Text(value) if value.is_empty() => (placeholder.to_string(), true),
            Self::Text(value) => (value.clone(), false),
            Self::Choice { options, selected } => {
                let value = options.get(*selected).copied().unwrap_or_default();
                (value.to_string(), false)
            }
            Self::Toggle(value) => {
                let rendered = if *value { "yes" } else { "no" };
                (rendered.to_string(), false)
            }
        }
    }

    fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn as_text_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    fn choice_value(&self) -> Option<&str> {
        match self {
            Self::Choice { options, selected } => options.get(*selected).copied(),
            _ => None,
        }
    }

    fn toggle(&mut self) {
        if let Self::Toggle(value) = self {
            *value = !*value;
        }
    }

    fn cycle(&mut self, step: isize) {
        if let Self::Choice { options, selected } = self {
            if options.is_empty() {
                return;
            }
            let len = options.len() as isize;
            let current = *selected as isize;
            let next = (current + step).rem_euclid(len);
            *selected = next as usize;
        }
    }

    fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }
}

#[derive(Clone)]
struct TuiFormField {
    label: &'static str,
    hint: &'static str,
    placeholder: &'static str,
    required: bool,
    /// Locked fields are prefilled and cannot be edited by the user.
    locked: bool,
    value: TuiFieldValue,
}

#[allow(dead_code)]
impl TuiFormField {
    fn text(label: &'static str, hint: &'static str, placeholder: &'static str) -> Self {
        Self {
            label,
            hint,
            placeholder,
            required: false,
            locked: false,
            value: TuiFieldValue::Text(String::new()),
        }
    }

    fn choice(
        label: &'static str,
        hint: &'static str,
        options: &'static [&'static str],
        selected: usize,
    ) -> Self {
        Self {
            label,
            hint,
            placeholder: "",
            required: false,
            locked: false,
            value: TuiFieldValue::Choice { options, selected },
        }
    }

    fn toggle(label: &'static str, hint: &'static str, value: bool) -> Self {
        Self {
            label,
            hint,
            placeholder: "",
            required: false,
            locked: false,
            value: TuiFieldValue::Toggle(value),
        }
    }
}

// ---------------------------------------------------------------------------
// Form config
// ---------------------------------------------------------------------------

/// A subcommand variant within a form (e.g. cashu send vs send-to-ln).
struct FormVariant {
    label: &'static str,
    path: Vec<&'static str>,
    /// Only show fields with these long names. None = show all.
    keep_fields: Option<Vec<&'static str>>,
    /// Fields to prefill as locked text: (long_name, value).
    locked_values: Vec<(&'static str, &'static str)>,
}

struct TuiFormConfig {
    title: &'static str,
    /// Subcommand variants — if len > 1, a Choice field is shown at index 0.
    variants: Vec<FormVariant>,
    /// Currently selected variant index.
    variant_index: usize,
    /// Label for the variant choice field (e.g. "action", "backend").
    variant_label: &'static str,
    fields: Vec<TuiFormField>,
    selected_field: usize,
}

#[allow(dead_code)]
impl TuiFormConfig {
    fn reset(&mut self) {
        for field in &mut self.fields {
            match &mut field.value {
                TuiFieldValue::Text(t) => t.clear(),
                TuiFieldValue::Choice { selected, .. } => *selected = 0,
                TuiFieldValue::Toggle(v) => *v = false,
            }
        }
        self.selected_field = 0;
    }

    fn field_text(&self, index: usize) -> &str {
        self.fields
            .get(index)
            .and_then(|f| f.value.as_text())
            .unwrap_or_default()
    }

    fn field_choice(&self, index: usize) -> &str {
        self.fields
            .get(index)
            .and_then(|f| f.value.choice_value())
            .unwrap_or_default()
    }
}

/// Map a network name to the clap subcommand path for "send".
fn send_subcommand_path(network: &str) -> Vec<&'static str> {
    match network {
        "cashu" => vec!["cashu", "send"],
        "ln" => vec!["ln", "send"],
        "sol" => vec!["sol", "send"],
        "evm" => vec!["evm", "send"],
        "btc" => vec!["btc", "send"],
        _ => vec![],
    }
}

/// Map a network name to the clap subcommand path for "receive".
fn receive_subcommand_path(network: &str) -> Vec<&'static str> {
    match network {
        "cashu" => vec!["cashu", "receive-from-ln"],
        "ln" => vec!["ln", "receive"],
        "sol" => vec!["sol", "receive"],
        "evm" => vec!["evm", "receive"],
        "btc" => vec!["btc", "receive"],
        _ => vec![],
    }
}

/// Convert clap ArgInfo into form fields.
///
/// If `wallet_id` is `Some`, the `--wallet` field is prefilled and locked (not editable).
/// If `wallet_id` is `None`, the `--wallet` field is omitted entirely (handler auto-selects).
fn args_to_form_fields(
    args: &[crate::args::ArgInfo],
    wallet_id: Option<&str>,
    keep_fields: Option<&[&str]>,
    locked_values: &[(&str, &str)],
) -> Vec<TuiFormField> {
    args.iter()
        .filter_map(|a| {
            if a.long == "wallet" {
                // Wallet selected in sidebar → prefill locked; no wallet → omit field
                let wid = wallet_id?;
                let label: &'static str = Box::leak(a.long.replace('-', " ").into_boxed_str());
                let help: &'static str = Box::leak(a.help.clone().into_boxed_str());
                let mut f = TuiFormField::text(label, help, "");
                f.required = false;
                if let TuiFieldValue::Text(t) = &mut f.value {
                    *t = wid.to_string();
                }
                f.locked = true;
                return Some(f);
            }
            // Check for locked prefill (before keep_fields — locked values always included)
            if let Some((_, val)) = locked_values.iter().find(|(k, _)| *k == a.long) {
                let label: &'static str = Box::leak(a.long.replace('-', " ").into_boxed_str());
                let help: &'static str = Box::leak(a.help.clone().into_boxed_str());
                let mut f = TuiFormField::text(label, help, "");
                if let TuiFieldValue::Text(t) = &mut f.value {
                    *t = val.to_string();
                }
                f.locked = true;
                return Some(f);
            }
            // Filter by keep_fields if specified
            if let Some(keep) = keep_fields {
                if !keep.contains(&a.long.as_str()) {
                    return None;
                }
            }
            let label: &'static str = Box::leak(a.long.replace('-', " ").into_boxed_str());
            let help: &'static str = Box::leak(a.help.clone().into_boxed_str());
            let placeholder: &'static str = if a.required { "required" } else { "optional" };
            if a.is_flag {
                Some(TuiFormField::toggle(label, help, false))
            } else {
                let mut f = TuiFormField::text(label, help, placeholder);
                f.required = a.required;
                Some(f)
            }
        })
        .collect()
}

fn build_form_fields_labeled(
    variants: &[FormVariant],
    variant_index: usize,
    wallet_id: Option<&str>,
    choice_label: &'static str,
) -> Vec<TuiFormField> {
    let mut fields = Vec::new();
    // Show choice field: always for "backend" (even single option), otherwise only when multiple
    let show_choice = variants.len() > 1 || (variants.len() == 1 && choice_label != "action");
    if show_choice {
        let labels: Vec<&'static str> = variants.iter().map(|v| v.label).collect();
        let options: &'static [&'static str] = Box::leak(labels.into_boxed_slice());
        let mut field = TuiFormField::choice(
            choice_label,
            "\u{2190}/\u{2192} cycle",
            options,
            variant_index,
        );
        if variants.len() == 1 {
            field.locked = true;
        }
        fields.push(field);
    }
    let variant = &variants[variant_index];
    let args = crate::args::subcommand_args(&variant.path);
    fields.extend(args_to_form_fields(
        &args,
        wallet_id,
        variant.keep_fields.as_deref(),
        &variant.locked_values,
    ));
    fields
}

fn build_form_fields(
    variants: &[FormVariant],
    variant_index: usize,
    wallet_id: Option<&str>,
) -> Vec<TuiFormField> {
    build_form_fields_labeled(variants, variant_index, wallet_id, "action")
}

fn make_send_form(network: &str, wallet_id: Option<&str>) -> TuiFormConfig {
    let variants = match network {
        "cashu" => vec![
            FormVariant {
                label: "P2P token",
                path: vec!["cashu", "send"],
                keep_fields: None,
                locked_values: vec![],
            },
            FormVariant {
                label: "Lightning invoice",
                path: vec!["cashu", "send-to-ln"],
                keep_fields: None,
                locked_values: vec![],
            },
        ],
        _ => {
            let path = send_subcommand_path(network);
            vec![FormVariant {
                label: "send",
                path,
                keep_fields: None,
                locked_values: vec![],
            }]
        }
    };
    let fields = build_form_fields(&variants, 0, wallet_id);
    TuiFormConfig {
        title: "Send",
        variants,
        variant_index: 0,
        variant_label: "action",
        fields,
        selected_field: 0,
    }
}

fn make_receive_form(network: &str, wallet_id: Option<&str>) -> TuiFormConfig {
    let variants = match network {
        "cashu" => vec![
            FormVariant {
                label: "Claim token",
                path: vec!["cashu", "receive"],
                keep_fields: None,
                locked_values: vec![],
            },
            FormVariant {
                label: "LN invoice",
                path: vec!["cashu", "receive-from-ln"],
                keep_fields: None,
                locked_values: vec![],
            },
            FormVariant {
                label: "Claim LN quote",
                path: vec!["cashu", "receive-from-ln-claim"],
                keep_fields: None,
                locked_values: vec![],
            },
        ],
        _ => {
            let path = receive_subcommand_path(network);
            vec![FormVariant {
                label: "receive",
                path,
                keep_fields: None,
                locked_values: vec![],
            }]
        }
    };
    let fields = build_form_fields(&variants, 0, wallet_id);
    TuiFormConfig {
        title: "Receive",
        variants,
        variant_index: 0,
        variant_label: "action",
        fields,
        selected_field: 0,
    }
}

fn wallet_create_path(network: &str) -> Vec<&'static str> {
    match network {
        "cashu" => vec!["cashu", "wallet", "create"],
        "ln" => vec!["ln", "wallet", "create"],
        "sol" => vec!["sol", "wallet", "create"],
        "evm" => vec!["evm", "wallet", "create"],
        "btc" => vec!["btc", "wallet", "create"],
        _ => vec![],
    }
}

fn wallet_close_path(network: &str) -> Vec<&'static str> {
    match network {
        "cashu" => vec!["cashu", "wallet", "close"],
        "ln" => vec!["ln", "wallet", "close"],
        "sol" => vec!["sol", "wallet", "close"],
        "evm" => vec!["evm", "wallet", "close"],
        "btc" => vec!["btc", "wallet", "close"],
        _ => vec![],
    }
}

fn wallet_show_seed_path(network: &str) -> Vec<&'static str> {
    match network {
        "cashu" => vec!["cashu", "wallet", "dangerously-show-seed"],
        "ln" => vec!["ln", "wallet", "dangerously-show-seed"],
        "sol" => vec!["sol", "wallet", "dangerously-show-seed"],
        "evm" => vec!["evm", "wallet", "dangerously-show-seed"],
        "btc" => vec!["btc", "wallet", "dangerously-show-seed"],
        _ => vec![],
    }
}

fn make_wallet_create_form(network: &str) -> TuiFormConfig {
    let path = wallet_create_path(network);
    let variants = match network {
        #[allow(clippy::vec_init_then_push)] // push gated by #[cfg(feature)]
        "ln" => {
            let mut v = Vec::new();
            #[cfg(feature = "ln-nwc")]
            v.push(FormVariant {
                label: "nwc",
                path: path.clone(),
                keep_fields: Some(vec!["nwc-uri-secret", "label"]),
                locked_values: vec![("backend", "nwc")],
            });
            #[cfg(feature = "ln-phoenixd")]
            v.push(FormVariant {
                label: "phoenixd",
                path: path.clone(),
                keep_fields: Some(vec!["endpoint", "password-secret", "label"]),
                locked_values: vec![("backend", "phoenixd")],
            });
            #[cfg(feature = "ln-lnbits")]
            v.push(FormVariant {
                label: "lnbits",
                path: path.clone(),
                keep_fields: Some(vec!["endpoint", "admin-key-secret", "label"]),
                locked_values: vec![("backend", "lnbits")],
            });
            v
        }
        #[allow(clippy::vec_init_then_push)] // push gated by #[cfg(feature)]
        "btc" => {
            let mut v = Vec::new();
            #[cfg(feature = "btc-esplora")]
            v.push(FormVariant {
                label: "esplora",
                path: path.clone(),
                keep_fields: Some(vec![
                    "btc-network",
                    "btc-address-type",
                    "btc-esplora-url",
                    "mnemonic-secret",
                    "label",
                ]),
                locked_values: vec![("btc-backend", "esplora")],
            });
            #[cfg(feature = "btc-core")]
            v.push(FormVariant {
                label: "core-rpc",
                path: path.clone(),
                keep_fields: Some(vec![
                    "btc-network",
                    "btc-address-type",
                    "btc-core-url",
                    "btc-core-auth-secret",
                    "mnemonic-secret",
                    "label",
                ]),
                locked_values: vec![("btc-backend", "core-rpc")],
            });
            #[cfg(feature = "btc-electrum")]
            v.push(FormVariant {
                label: "electrum",
                path: path.clone(),
                keep_fields: Some(vec![
                    "btc-network",
                    "btc-address-type",
                    "btc-electrum-url",
                    "mnemonic-secret",
                    "label",
                ]),
                locked_values: vec![("btc-backend", "electrum")],
            });
            v
        }
        _ => vec![FormVariant {
            label: "create",
            path,
            keep_fields: None,
            locked_values: vec![],
        }],
    };
    let variant_label = match network {
        "ln" | "btc" => "backend",
        _ => "action",
    };
    let fields = build_form_fields_labeled(&variants, 0, None, variant_label);
    TuiFormConfig {
        title: "Wallet Create",
        variants,
        variant_index: 0,
        variant_label,
        fields,
        selected_field: 0,
    }
}

fn make_wallet_close_form(network: &str, wallet_id: &str) -> TuiFormConfig {
    let path = wallet_close_path(network);
    let variants = vec![FormVariant {
        label: "close",
        path,
        keep_fields: None,
        locked_values: vec![],
    }];
    let fields = build_form_fields(&variants, 0, Some(wallet_id));
    TuiFormConfig {
        title: "Wallet Close",
        variants,
        variant_index: 0,
        variant_label: "action",
        fields,
        selected_field: 0,
    }
}

fn make_wallet_show_seed_form(network: &str, wallet_id: &str) -> TuiFormConfig {
    let path = wallet_show_seed_path(network);
    let variants = vec![FormVariant {
        label: "show seed",
        path,
        keep_fields: None,
        locked_values: vec![],
    }];
    let fields = build_form_fields(&variants, 0, Some(wallet_id));
    TuiFormConfig {
        title: "Dangerously Show Seed",
        variants,
        variant_index: 0,
        variant_label: "action",
        fields,
        selected_field: 0,
    }
}

fn make_wallet_config_form(network: &str) -> TuiFormConfig {
    let net: &'static str = Box::leak(network.to_string().into_boxed_str());
    let variants = match network {
        "sol" | "evm" => vec![
            FormVariant {
                label: "set",
                path: vec![net, "config", "set"],
                keep_fields: None,
                locked_values: vec![],
            },
            FormVariant {
                label: "token add",
                path: vec![net, "config", "token-add"],
                keep_fields: None,
                locked_values: vec![],
            },
            FormVariant {
                label: "token remove",
                path: vec![net, "config", "token-remove"],
                keep_fields: None,
                locked_values: vec![],
            },
        ],
        // cashu, ln, btc — label only, no token management
        _ => vec![FormVariant {
            label: "set",
            path: vec![net, "config", "set"],
            keep_fields: None,
            locked_values: vec![],
        }],
    };
    let fields = build_form_fields(&variants, 0, None);
    TuiFormConfig {
        title: "Wallet Config",
        variants,
        variant_index: 0,
        variant_label: "action",
        fields,
        selected_field: 0,
    }
}

/// Build command for wallet config form. Injects `--wallet <id>` between
/// `config` and the subcommand (set/token-add/token-remove).
fn build_wallet_config_command(
    form: &TuiFormConfig,
    network: &str,
    wallet_id: &str,
) -> Result<String, String> {
    let variant = &form.variants[form.variant_index];
    let args = crate::args::subcommand_args(&variant.path);
    let base_cmd = build_command_from_form(form, &variant.path, &args)?;
    // base_cmd = "<network> config set --label foo" or "<network> config token-add --symbol ..."
    // Insert --wallet between "config" and the subcommand
    let config_prefix = format!("{network} config ");
    if let Some(rest) = base_cmd.strip_prefix(&config_prefix) {
        Ok(format!(
            "{network} config --wallet {} {rest}",
            shell_quote(wallet_id)
        ))
    } else {
        // Fallback: just prepend --wallet
        Ok(format!("{base_cmd} --wallet {}", shell_quote(wallet_id)))
    }
}

fn make_global_config_form() -> TuiFormConfig {
    let variants = vec![FormVariant {
        label: "set",
        path: vec!["global", "config", "set"],
        keep_fields: None,
        locked_values: vec![],
    }];
    let fields = build_form_fields(&variants, 0, None);
    TuiFormConfig {
        title: "Global Config",
        variants,
        variant_index: 0,
        variant_label: "action",
        fields,
        selected_field: 0,
    }
}

static LIMIT_NETWORKS: &[&str] = &["cashu", "ln", "sol", "evm", "btc"];

/// Does this network support a --token flag on limit add?
fn network_has_token(network: &str) -> bool {
    matches!(network, "sol" | "evm")
}

/// Inject manual "network" and "wallet" fields for limit add form variants.
/// - variant 0 (global): no extra fields
/// - variant 1 (network): add "network" choice field
/// - variant 2 (wallet): add "network" choice and "wallet" choice fields
///
/// Also strips the "token" field when the selected network doesn't support it.
/// `wallet_ids` supplies the available wallets for the selected network.
fn inject_limit_add_extra_fields(
    fields: &mut Vec<TuiFormField>,
    variant_index: usize,
    network_index: usize,
    wallet_ids: &[String],
) {
    if variant_index == 0 {
        return;
    }
    let net_field = {
        let mut f = TuiFormField::choice(
            "network",
            "\u{2190}/\u{2192} cycle",
            LIMIT_NETWORKS,
            network_index,
        );
        f.required = true;
        f
    };
    // Insert after the first field (the scope choice)
    fields.insert(1, net_field);
    if variant_index == 2 {
        if wallet_ids.is_empty() {
            // Fallback to text input when no wallets exist yet
            let mut wallet_field = TuiFormField::text("wallet", "Wallet ID", "no wallets");
            wallet_field.required = true;
            fields.insert(2, wallet_field);
        } else {
            let leaked: &'static [&'static str] = Box::leak(
                wallet_ids
                    .iter()
                    .map(|s| &*Box::leak(s.clone().into_boxed_str()))
                    .collect::<Vec<&'static str>>()
                    .into_boxed_slice(),
            );
            let mut wallet_field =
                TuiFormField::choice("wallet", "\u{2190}/\u{2192} cycle", leaked, 0);
            wallet_field.required = true;
            fields.insert(2, wallet_field);
        }
    }
    // Strip token field for sats-only networks
    let selected_net = LIMIT_NETWORKS
        .get(network_index)
        .copied()
        .unwrap_or("cashu");
    if !network_has_token(selected_net) {
        fields.retain(|f| f.label != "token");
    }
}

fn make_limit_add_form() -> TuiFormConfig {
    // All network/wallet variants use the same clap path for field extraction;
    // the actual command path is built dynamically based on the "network" and
    // "wallet" text fields added manually below.
    let variants = vec![
        FormVariant {
            label: "global (USD cents)",
            path: vec!["global", "limit", "add"],
            keep_fields: Some(vec!["window", "max-spend"]),
            locked_values: vec![],
        },
        FormVariant {
            label: "network",
            path: vec!["sol", "limit", "add"],
            keep_fields: Some(vec!["token", "window", "max-spend"]),
            locked_values: vec![],
        },
        FormVariant {
            label: "wallet",
            path: vec!["sol", "limit", "add"],
            keep_fields: Some(vec!["token", "window", "max-spend"]),
            locked_values: vec![],
        },
    ];
    let mut fields = build_form_fields_labeled(&variants, 0, None, "scope");
    inject_limit_add_extra_fields(&mut fields, 0, 0, &[]);

    TuiFormConfig {
        title: "Add Spend Limit",
        variants,
        variant_index: 0,
        variant_label: "scope",
        fields,
        selected_field: 0,
    }
}

/// Build command string for the limit add form. Handles the dynamic
/// network/wallet path that can't be expressed in static FormVariant paths.
fn build_limit_add_command(form: &TuiFormConfig) -> Result<String, String> {
    let variant = &form.variants[form.variant_index];

    match variant.label {
        "global (USD cents)" => {
            // Standard form building — path is ["global", "limit", "add"]
            let args = crate::args::subcommand_args(&variant.path);
            build_command_from_form(form, &variant.path, &args)
        }
        "network" | "wallet" => {
            // Extract network from the manual field (choice or text)
            let network = form
                .fields
                .iter()
                .find(|f| f.label == "network")
                .and_then(|f| match &f.value {
                    TuiFieldValue::Choice { options, selected } => {
                        options.get(*selected).map(|s| s.to_string())
                    }
                    TuiFieldValue::Text(t) => {
                        let v = t.trim();
                        if v.is_empty() {
                            None
                        } else {
                            Some(v.to_string())
                        }
                    }
                    _ => None,
                })
                .ok_or_else(|| "network is required".to_string())?;

            // Build: <network> limit [--wallet <w>] add <flags>
            let ref_path: Vec<&str> = vec![&network, "limit", "add"];
            let args = crate::args::subcommand_args(&ref_path);
            let flags_cmd = build_command_from_form(form, &ref_path, &args)?;
            // flags_cmd = "<network> limit add --token ... --window ... --max-spend ..."

            if variant.label == "wallet" {
                let wallet = form
                    .fields
                    .iter()
                    .find(|f| f.label == "wallet")
                    .and_then(|f| match &f.value {
                        TuiFieldValue::Choice { options, selected } => {
                            options.get(*selected).map(|s| s.to_string())
                        }
                        TuiFieldValue::Text(t) => {
                            let v = t.trim();
                            if v.is_empty() {
                                None
                            } else {
                                Some(v.to_string())
                            }
                        }
                        _ => None,
                    })
                    .ok_or_else(|| "wallet is required for wallet scope".to_string())?;
                // Insert --wallet between "limit" and "add"
                Ok(flags_cmd.replacen(
                    &format!("{network} limit add"),
                    &format!("{network} limit --wallet {} add", shell_quote(&wallet)),
                    1,
                ))
            } else {
                Ok(flags_cmd)
            }
        }
        _ => Err("unknown limit variant".to_string()),
    }
}

fn build_form_command_from_variant(form: &TuiFormConfig) -> Result<String, String> {
    let variant = &form.variants[form.variant_index];
    let path = &variant.path;
    let args = crate::args::subcommand_args(path);
    let mut cmd = build_command_from_form(form, path, &args)?;
    // Append locked values that aren't already in the command
    // (locked_values bypass the choice field which doesn't emit args)
    for (k, v) in &variant.locked_values {
        let flag = format!("--{k}");
        if !cmd.contains(&flag) {
            cmd.push_str(&format!(" {flag} {}", shell_quote(v)));
        }
    }
    Ok(cmd)
}

/// Build a CLI command string from form values, matching fields to clap ArgInfo by long name.
fn build_command_from_form(
    form: &TuiFormConfig,
    path: &[&str],
    args: &[crate::args::ArgInfo],
) -> Result<String, String> {
    let mut parts: Vec<String> = path.iter().map(|s| s.to_string()).collect();

    for arg in args {
        let field_label = arg.long.replace('-', " ");
        let Some(field) = form.fields.iter().find(|f| f.label == field_label) else {
            continue;
        };
        match &field.value {
            TuiFieldValue::Text(text) => {
                let v = text.trim();
                if v.is_empty() {
                    if arg.required {
                        return Err(format!("--{} is required.", arg.long));
                    }
                    continue;
                }
                if arg.positional_index.is_some() {
                    parts.push(shell_quote(v));
                } else {
                    parts.push(format!("--{}", arg.long));
                    parts.push(shell_quote(v));
                }
            }
            TuiFieldValue::Toggle(on) => {
                if *on {
                    parts.push(format!("--{}", arg.long));
                }
            }
            TuiFieldValue::Choice { .. } => {}
        }
    }

    Ok(parts.join(" "))
}

// ---------------------------------------------------------------------------
// TuiApp
// ---------------------------------------------------------------------------

struct TuiApp {
    frontend: InteractiveFrontend,
    connection_label: String,

    // Sidebar
    focus: TuiFocus,
    wallet_groups: Vec<WalletGroup>,
    sidebar_cursor: SidebarItem,

    // Right pane
    view: TuiView,
    wallet_data: WalletViewData,
    group_summary: Option<GroupSummaryData>,

    // Send/Receive forms
    send_form: TuiFormConfig,
    receive_form: TuiFormConfig,
    wallet_create_form: TuiFormConfig,
    wallet_close_form: TuiFormConfig,
    wallet_show_seed_form: TuiFormConfig,
    limit_add_form: TuiFormConfig,
    wallet_config_form: TuiFormConfig,
    /// Network for the active wallet config form (used in command building).
    config_network: String,
    /// Wallet ID for the active wallet config form (used in command building).
    config_wallet_id: String,
    field_cursor_chars: usize,
    form_on_submit: bool,

    // History
    history_records: Vec<HistoryDisplayRecord>,
    selected_history: usize,
    /// True when history is at network level (show wallet column).
    history_is_network: bool,
    /// Detail text for the selected history entry.
    history_detail_text: Option<String>,

    // Limits
    limit_records: Vec<LimitDisplayRecord>,
    selected_limit: usize,

    // Async query
    pending_query: Option<PendingQuery>,

    // Global config
    global_config_form: TuiFormConfig,

    // Output (: commands and action results)
    messages: Vec<TuiMessage>,
    output_scroll: usize,

    // Modal
    modal: Option<TuiModal>,

    // Command history
    history: Vec<String>,

    // Data backup / restore
    data_dir: String,
    data_backup_output: String,
    data_restore_archive: String,
    data_restore_overwrite: bool,
    data_restore_pg_url: String,
    data_backup_status: DataOpStatus,
    data_restore_status: DataOpStatus,
    data_pending: Option<DataPending>,
    /// 0 = Backup mode, 1 = Restore mode
    data_cursor: usize,
    /// 0 = mode-choice row, 1..=n = input fields
    data_field_cursor: usize,
    /// True when cursor is on the submit button row (like form_on_submit)
    data_on_submit: bool,
    /// Char-level cursor position within the current text field
    data_cursor_chars: usize,

    // Last copyable value extracted from output (token/invoice/address)
    last_copyable: Option<String>,
}

impl TuiApp {
    fn new(frontend: InteractiveFrontend, history: Vec<String>) -> Self {
        Self {
            frontend,
            connection_label: "local".to_string(),
            focus: TuiFocus::Sidebar,
            wallet_groups: Vec::new(),
            sidebar_cursor: SidebarItem::Group(0),
            view: TuiView::WalletDetail,
            wallet_data: WalletViewData::empty(),
            group_summary: None,
            send_form: make_send_form("cashu", None),
            receive_form: make_receive_form("cashu", None),
            wallet_create_form: make_wallet_create_form("cashu"),
            wallet_close_form: make_wallet_close_form("cashu", ""),
            wallet_show_seed_form: make_wallet_show_seed_form("cashu", ""),
            limit_add_form: make_limit_add_form(),
            wallet_config_form: make_wallet_config_form("cashu"),
            config_network: "cashu".to_string(),
            config_wallet_id: String::new(),
            field_cursor_chars: 0,
            form_on_submit: false,
            history_records: Vec::new(),
            selected_history: 0,
            history_is_network: false,
            history_detail_text: None,
            limit_records: Vec::new(),
            selected_limit: 0,
            pending_query: None,
            global_config_form: make_global_config_form(),
            messages: Vec::new(),
            output_scroll: 0,
            modal: None,
            history,
            data_dir: String::new(),
            data_backup_output: String::new(),
            data_restore_archive: String::new(),
            data_restore_overwrite: false,
            data_restore_pg_url: String::new(),
            data_backup_status: DataOpStatus::Idle,
            data_restore_status: DataOpStatus::Idle,
            data_pending: None,
            data_cursor: 0,
            data_field_cursor: 0, // start on mode-choice row so user can pick Backup/Restore first
            data_on_submit: false,
            data_cursor_chars: 0,
            last_copyable: None,
        }
    }

    /// Get wallet ID from sidebar cursor (None if on a group/limit).
    fn sidebar_wallet_id(&self) -> Option<&str> {
        match self.sidebar_cursor {
            SidebarItem::Wallet(gi, wi) => self
                .wallet_groups
                .get(gi)
                .and_then(|g| g.wallets.get(wi))
                .map(|w| w.id.as_str()),
            _ => None,
        }
    }

    /// Derive form context from sidebar selection.
    ///
    /// Returns `(network, wallet_id)`:
    /// - `Wallet` selected → network from wallet, wallet_id = Some (locked in form)
    /// - `Group` selected  → network from group,  wallet_id = None  (omitted from form)
    fn form_context(&self) -> (String, Option<String>) {
        match self.sidebar_cursor {
            SidebarItem::Wallet(gi, wi) => {
                let group = &self.wallet_groups[gi];
                let wallet = &group.wallets[wi];
                let network = group.network.to_string().to_lowercase();
                (network, Some(wallet.id.clone()))
            }
            SidebarItem::Group(gi) => {
                let network = self
                    .wallet_groups
                    .get(gi)
                    .map(|g| g.network.to_string().to_lowercase())
                    .unwrap_or_else(|| "cashu".into());
                (network, None)
            }
            _ => {
                // Fallback: first network group
                ("cashu".to_string(), None)
            }
        }
    }

    /// Wallet IDs for a given network name (from current sidebar groups).
    fn wallet_ids_for_network(&self, network: &str) -> Vec<String> {
        self.wallet_groups
            .iter()
            .find(|g| g.network.to_string().eq_ignore_ascii_case(network))
            .map(|g| g.wallets.iter().map(|w| w.id.clone()).collect())
            .unwrap_or_default()
    }

    fn sync_session(&mut self, _state: &SessionState, backend: &SessionBackend) {
        self.connection_label = backend.connection_label();
    }

    fn set_wallets(&mut self, wallets: Vec<TuiWalletEntry>) {
        self.wallet_groups = build_wallet_groups(wallets);
        // Restore cursor to valid position
        if self.wallet_groups.is_empty() {
            self.sidebar_cursor = SidebarItem::Group(0);
        } else if let SidebarItem::Group(gi) = self.sidebar_cursor {
            if gi >= self.wallet_groups.len() {
                self.sidebar_cursor = SidebarItem::Group(0);
            }
        } else if let SidebarItem::Wallet(gi, _) = self.sidebar_cursor {
            if gi >= self.wallet_groups.len() {
                self.sidebar_cursor = SidebarItem::Group(0);
            }
        }
    }

    // -- Messages --

    fn push_message(&mut self, kind: TuiMessageKind, text: String) {
        self.messages.push(TuiMessage { kind, text });
        self.output_scroll = 0;
    }

    fn push_notice(&mut self, text: impl Into<String>) {
        self.push_message(TuiMessageKind::Notice, text.into());
    }

    fn clear_messages(&mut self) {
        self.messages.clear();
        self.output_scroll = 0;
    }

    fn record_history(&mut self, value: String) {
        if self.history.last() != Some(&value) {
            self.history.push(value);
        }
    }

    // -- Sidebar navigation --

    fn sidebar_items(&self) -> Vec<SidebarItem> {
        let mut items = Vec::new();
        for (gi, group) in self.wallet_groups.iter().enumerate() {
            items.push(SidebarItem::Group(gi));
            if group.expanded {
                for (wi, _) in group.wallets.iter().enumerate() {
                    items.push(SidebarItem::Wallet(gi, wi));
                }
            }
        }
        // Limits section
        if !self.limit_records.is_empty() || !items.is_empty() {
            items.push(SidebarItem::LimitHeader);
            for (li, _) in self.limit_records.iter().enumerate() {
                items.push(SidebarItem::Limit(li));
            }
        }
        // Config section
        items.push(SidebarItem::ConfigHeader);
        // Data section
        items.push(SidebarItem::DataHeader);
        items
    }

    /// Move sidebar cursor and return the action to auto-trigger for the new position.
    fn sidebar_move(&mut self, step: isize) -> TuiAction {
        let items = self.sidebar_items();
        if items.is_empty() {
            return TuiAction::None;
        }
        let current = items
            .iter()
            .position(|item| *item == self.sidebar_cursor)
            .unwrap_or(0);
        let next = (current as isize + step).clamp(0, items.len() as isize - 1) as usize;
        self.sidebar_cursor = items[next];
        self.sidebar_auto_action()
    }

    /// Determine the auto-action for the current sidebar cursor position.
    fn sidebar_auto_action(&self) -> TuiAction {
        match self.sidebar_cursor {
            SidebarItem::Group(gi) => {
                if let Some(group) = self.wallet_groups.get(gi) {
                    TuiAction::SelectGroup(group.network)
                } else {
                    TuiAction::None
                }
            }
            SidebarItem::Wallet(gi, wi) => {
                if let Some(wallet) = self.wallet_groups.get(gi).and_then(|g| g.wallets.get(wi)) {
                    TuiAction::SelectWallet(wallet.id.clone())
                } else {
                    TuiAction::None
                }
            }
            SidebarItem::LimitHeader => TuiAction::FetchLimits,
            SidebarItem::Limit(li) => TuiAction::ShowLimitDetail(li),
            SidebarItem::ConfigHeader => TuiAction::ShowGlobalConfig,
            SidebarItem::DataHeader => TuiAction::ShowDataView,
        }
    }

    /// Enter key on sidebar: toggle group expand/collapse, or no-op for others.
    fn sidebar_enter(&mut self) {
        if let SidebarItem::Group(gi) = self.sidebar_cursor {
            if let Some(group) = self.wallet_groups.get_mut(gi) {
                group.expanded = !group.expanded;
            }
        }
    }

    // -- View switching --

    fn switch_view(&mut self, view: TuiView) {
        self.view = view;
        self.focus = TuiFocus::Main;
        self.form_on_submit = false;
    }

    // -- Current form --

    fn current_form(&self) -> Option<&TuiFormConfig> {
        match self.view {
            TuiView::Send => Some(&self.send_form),
            TuiView::Receive => Some(&self.receive_form),
            TuiView::WalletCreate => Some(&self.wallet_create_form),
            TuiView::WalletClose => Some(&self.wallet_close_form),
            TuiView::WalletShowSeed => Some(&self.wallet_show_seed_form),
            TuiView::LimitAdd => Some(&self.limit_add_form),
            TuiView::WalletConfig => Some(&self.wallet_config_form),
            TuiView::GlobalConfig => Some(&self.global_config_form),
            _ => None,
        }
    }

    fn current_form_mut(&mut self) -> Option<&mut TuiFormConfig> {
        match self.view {
            TuiView::Send => Some(&mut self.send_form),
            TuiView::Receive => Some(&mut self.receive_form),
            TuiView::WalletCreate => Some(&mut self.wallet_create_form),
            TuiView::WalletClose => Some(&mut self.wallet_close_form),
            TuiView::WalletShowSeed => Some(&mut self.wallet_show_seed_form),
            TuiView::LimitAdd => Some(&mut self.limit_add_form),
            TuiView::WalletConfig => Some(&mut self.wallet_config_form),
            TuiView::GlobalConfig => Some(&mut self.global_config_form),
            _ => None,
        }
    }

    fn current_field(&self) -> Option<&TuiFormField> {
        self.current_form()
            .and_then(|f| f.fields.get(f.selected_field))
    }

    fn current_field_mut(&mut self) -> Option<&mut TuiFormField> {
        if let Some(form) = self.current_form_mut() {
            let idx = form.selected_field;
            form.fields.get_mut(idx)
        } else {
            None
        }
    }

    fn current_field_is_text(&self) -> bool {
        self.current_field()
            .map(|f| f.value.is_text())
            .unwrap_or(false)
    }

    // -- Field editing --

    fn select_field(&mut self, step: isize) {
        if let Some(form) = self.current_form_mut() {
            let len = form.fields.len() as isize;
            if len == 0 {
                return;
            }
            let cur = form.selected_field as isize;
            let next = (cur + step).clamp(0, len - 1);
            form.selected_field = next as usize;
        }
        self.sync_field_cursor();
    }

    fn sync_field_cursor(&mut self) {
        self.field_cursor_chars = self
            .current_field()
            .and_then(|f| f.value.as_text())
            .map(|t| t.chars().count())
            .unwrap_or(0);
    }

    fn is_current_field_locked(&self) -> bool {
        self.current_field().is_some_and(|f| f.locked)
    }

    fn insert_into_field(&mut self, ch: char) {
        if self.is_current_field_locked() {
            return;
        }
        let cursor = self.field_cursor_chars;
        if let Some(field) = self.current_field_mut() {
            if let Some(text) = field.value.as_text_mut() {
                let byte_index = char_to_byte_index(text, cursor);
                text.insert(byte_index, ch);
                self.field_cursor_chars = cursor + 1;
            }
        }
    }

    fn field_backspace(&mut self) {
        if self.is_current_field_locked() || self.field_cursor_chars == 0 {
            return;
        }
        let cursor = self.field_cursor_chars;
        if let Some(field) = self.current_field_mut() {
            if let Some(text) = field.value.as_text_mut() {
                let end = char_to_byte_index(text, cursor);
                let start = char_to_byte_index(text, cursor - 1);
                text.replace_range(start..end, "");
                self.field_cursor_chars = cursor - 1;
            }
        }
    }

    fn field_delete(&mut self) {
        if self.is_current_field_locked() {
            return;
        }
        let cursor = self.field_cursor_chars;
        if let Some(field) = self.current_field_mut() {
            if let Some(text) = field.value.as_text_mut() {
                if cursor >= text.chars().count() {
                    return;
                }
                let start = char_to_byte_index(text, cursor);
                let end = char_to_byte_index(text, cursor + 1);
                text.replace_range(start..end, "");
            }
        }
    }

    fn cycle_current_field(&mut self, step: isize) {
        let is_limit_add = self.view == TuiView::LimitAdd;

        // Check if we're on the variant choice field (index 0 when variants > 1)
        let is_variant_choice = self.current_form().is_some()
            && self
                .current_form()
                .is_some_and(|f| f.variants.len() > 1 && f.selected_field == 0);

        // Check if we're on the network choice field in limit add form
        let is_limit_network_choice = is_limit_add
            && !is_variant_choice
            && self.current_form().is_some_and(|f| {
                f.fields
                    .get(f.selected_field)
                    .is_some_and(|field| field.label == "network")
            });

        if let Some(field) = self.current_field_mut() {
            match &mut field.value {
                TuiFieldValue::Choice { .. } => field.value.cycle(step),
                TuiFieldValue::Toggle(_) => field.value.toggle(),
                TuiFieldValue::Text(_) => {}
            }
        }

        // Rebuild form fields when variant changes
        if is_variant_choice {
            let (_, wallet_id) = self.form_context();
            let default_wallets = if is_limit_add {
                // Default network (index 0 = cashu) wallet list
                self.wallet_ids_for_network(LIMIT_NETWORKS[0])
            } else {
                vec![]
            };
            let form = match self.current_form_mut() {
                Some(f) => f,
                None => return,
            };
            // Read the new variant index from the choice field
            let new_idx = match &form.fields[0].value {
                TuiFieldValue::Choice { selected, .. } => *selected,
                _ => return,
            };
            form.variant_index = new_idx;
            let label = form.variant_label;
            form.fields =
                build_form_fields_labeled(&form.variants, new_idx, wallet_id.as_deref(), label);
            // Limit add form: re-inject manual network/wallet fields
            if is_limit_add {
                inject_limit_add_extra_fields(&mut form.fields, new_idx, 0, &default_wallets);
            }
        }

        // Rebuild limit add fields when network choice changes (show/hide token)
        if is_limit_network_choice {
            let (_, wallet_id) = self.form_context();
            // Read the new network index before mutable borrow
            let net_idx = self
                .current_form()
                .and_then(|f| f.fields.iter().find(|f| f.label == "network"))
                .and_then(|f| match &f.value {
                    TuiFieldValue::Choice { selected, .. } => Some(*selected),
                    _ => None,
                })
                .unwrap_or(0);
            let net_name = LIMIT_NETWORKS.get(net_idx).copied().unwrap_or("cashu");
            let wallets = self.wallet_ids_for_network(net_name);
            let form = match self.current_form_mut() {
                Some(f) => f,
                None => return,
            };
            let var_idx = form.variant_index;
            let label = form.variant_label;
            form.fields =
                build_form_fields_labeled(&form.variants, var_idx, wallet_id.as_deref(), label);
            inject_limit_add_extra_fields(&mut form.fields, var_idx, net_idx, &wallets);
            // Re-select the network field
            form.selected_field = form
                .fields
                .iter()
                .position(|f| f.label == "network")
                .unwrap_or(1);
        }
    }

    fn build_form_command(&self) -> Result<String, String> {
        match self.current_form() {
            Some(form) if self.view == TuiView::LimitAdd => build_limit_add_command(form),
            Some(form) if self.view == TuiView::WalletConfig => {
                build_wallet_config_command(form, &self.config_network, &self.config_wallet_id)
            }
            Some(form) => build_form_command_from_variant(form),
            None => Err("No form active.".to_string()),
        }
    }

    // -- Populate wallet detail from query result --

    fn populate_wallet_detail(
        &mut self,
        wallet_id: &str,
        wallets: &[TuiWalletEntry],
        balance_outputs: &[serde_json::Value],
    ) {
        let entry = wallets.iter().find(|w| w.id == wallet_id);
        self.wallet_data = WalletViewData::empty();
        self.wallet_data.wallet_id = Some(wallet_id.to_string());

        if let Some(entry) = entry {
            self.wallet_data.network = entry.network.map(|n| n.to_string());
            self.wallet_data.label = entry.label.clone();
        }

        // Parse balance from query outputs
        for value in balance_outputs {
            let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("");
            if code == "error" {
                self.wallet_data.balance_error = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                continue;
            }
            if code == "wallet_balances" {
                if let Some(items) = value.get("wallets").and_then(|v| v.as_array()) {
                    for item in items {
                        let wid = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if wid != wallet_id {
                            continue;
                        }
                        // Pick up metadata from balance response
                        if self.wallet_data.address.is_none() {
                            self.wallet_data.address = item
                                .get("address")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        if self.wallet_data.mint_url.is_none() {
                            self.wallet_data.mint_url = item
                                .get("mint_url")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        if self.wallet_data.backend.is_none() {
                            self.wallet_data.backend = item
                                .get("backend")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        if self.wallet_data.created_at.is_none() {
                            if let Some(ts) =
                                item.get("created_at_epoch_s").and_then(|v| v.as_u64())
                            {
                                self.wallet_data.created_at = Some(format_epoch(ts));
                            }
                        }

                        if let Some(err) = item.get("error").and_then(|v| v.as_str()) {
                            self.wallet_data.balance_error = Some(err.to_string());
                            continue;
                        }

                        if let Some(bal) = item.get("balance") {
                            let confirmed =
                                bal.get("confirmed").and_then(|v| v.as_u64()).unwrap_or(0);
                            let pending = bal.get("pending").and_then(|v| v.as_u64()).unwrap_or(0);
                            let unit = bal.get("unit").and_then(|v| v.as_str()).unwrap_or("sat");

                            let mut lines = vec![format!("{confirmed} {unit}")];
                            if pending > 0 {
                                lines.push(format!("{pending} {unit} (pending)"));
                            }

                            // Show additional fields (e.g. fee_credit_sats for phoenixd)
                            if let Some(obj) = bal.as_object() {
                                for (key, val) in obj {
                                    if matches!(key.as_str(), "confirmed" | "pending" | "unit") {
                                        continue;
                                    }
                                    if let Some(v) = val.as_u64() {
                                        if v > 0 {
                                            let display_key = key.replace('_', " ");
                                            lines.push(format!("{v} ({display_key})"));
                                        }
                                    }
                                }
                            }

                            self.wallet_data.balance_text = Some(lines.join("\n"));
                        }
                    }
                }
            }
        }
    }

    // -- Populate history from query result --

    fn populate_history(&mut self, outputs: &[serde_json::Value]) {
        self.history_records.clear();
        self.selected_history = 0;
        for value in outputs {
            let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("");
            if code == "history" {
                if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
                    for item in items {
                        let dir = item
                            .get("direction")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let arrow = if dir == "send" {
                            "\u{2193}"
                        } else {
                            "\u{2191}"
                        };
                        let amount_val = item
                            .get("amount")
                            .and_then(|v| v.get("value"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let token = item
                            .get("amount")
                            .and_then(|v| v.get("token"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("sat");
                        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let ts = item
                            .get("created_at_epoch_s")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let memo = item
                            .get("onchain_memo")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let local_memo = item.get("local_memo").and_then(|v| {
                            if let Some(obj) = v.as_object() {
                                let parts: Vec<String> = obj
                                    .iter()
                                    .map(|(k, v)| {
                                        let val = v.as_str().unwrap_or("");
                                        format!("{k}: {val}")
                                    })
                                    .collect();
                                if parts.is_empty() {
                                    None
                                } else {
                                    Some(parts.join(", "))
                                }
                            } else {
                                v.as_str().map(|s| s.to_string())
                            }
                        });
                        let wallet = item
                            .get("wallet")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let transaction_id = item
                            .get("transaction_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.history_records.push(HistoryDisplayRecord {
                            transaction_id,
                            wallet,
                            direction: arrow.to_string(),
                            amount: format!("{amount_val} {token}"),
                            status: status.to_string(),
                            date: format_epoch(ts),
                            memo,
                            local_memo,
                        });
                    }
                }
            }
            if code == "error" {
                let err = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                self.push_notice(format!("History error: {err}"));
            }
        }
    }

    // -- Populate limits from query result --

    fn populate_limits(&mut self, outputs: &[serde_json::Value]) {
        self.limit_records.clear();
        self.selected_limit = 0;
        for value in outputs {
            let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("");
            if code == "limit_status" {
                if let Some(limits) = value.get("limits").and_then(|v| v.as_array()) {
                    for lim in limits {
                        let rule_id = lim
                            .get("rule_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let scope = lim
                            .get("scope")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string();
                        let max_spend = lim.get("max_spend").and_then(|v| v.as_u64()).unwrap_or(0);
                        let spent = lim.get("spent").and_then(|v| v.as_u64()).unwrap_or(0);
                        let remaining = lim.get("remaining").and_then(|v| v.as_u64()).unwrap_or(0);
                        let window_s = lim.get("window_s").and_then(|v| v.as_u64()).unwrap_or(0);
                        let token = lim.get("token").and_then(|v| v.as_str()).unwrap_or("sat");
                        let network = lim.get("network").and_then(|v| v.as_str()).unwrap_or("");
                        let scope_label = if !network.is_empty() {
                            format!("{scope}/{network}")
                        } else {
                            scope
                        };
                        self.limit_records.push(LimitDisplayRecord {
                            rule_id,
                            scope: scope_label,
                            max_spend: format!("{max_spend} {token}"),
                            spent: format!("{spent}"),
                            remaining: format!("{remaining}"),
                            window: format_duration(window_s),
                        });
                    }
                }
            }
        }
    }

    // -- Populate group summary from query result --

    fn populate_group_summary(&mut self, network: Network, outputs: &[serde_json::Value]) {
        let network_str = network.to_string();
        let default_unit = match network {
            Network::Sol => "lamports",
            Network::Evm => "wei",
            _ => "sat",
        };
        let mut summary = GroupSummaryData {
            network: network_str,
            wallet_count: 0,
            confirmed: 0,
            pending: 0,
            unit: default_unit.to_string(),
            errors: 0,
            wallets: Vec::new(),
        };

        for value in outputs {
            let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("");
            if code == "wallet_balances" {
                // Use summary field if available
                if let Some(sums) = value.get("summary").and_then(|v| v.as_array()) {
                    for s in sums {
                        summary.wallet_count =
                            s.get("wallet_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        summary.confirmed =
                            s.get("confirmed").and_then(|v| v.as_u64()).unwrap_or(0);
                        summary.pending = s.get("pending").and_then(|v| v.as_u64()).unwrap_or(0);
                        summary.unit = s
                            .get("unit")
                            .and_then(|v| v.as_str())
                            .unwrap_or("sat")
                            .to_string();
                        summary.errors =
                            s.get("errors").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    }
                }
                // Also build per-wallet lines
                if let Some(items) = value.get("wallets").and_then(|v| v.as_array()) {
                    for item in items {
                        let label = item
                            .get("label")
                            .and_then(|v| v.as_str())
                            .or_else(|| item.get("id").and_then(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string();
                        let error = item
                            .get("error")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let balance = if let Some(bal) = item.get("balance") {
                            let c = bal.get("confirmed").and_then(|v| v.as_u64()).unwrap_or(0);
                            let p = bal.get("pending").and_then(|v| v.as_u64()).unwrap_or(0);
                            let u = bal.get("unit").and_then(|v| v.as_str()).unwrap_or("sat");
                            if p > 0 {
                                format!("{c} {u} (+{p} pending)")
                            } else {
                                format!("{c} {u}")
                            }
                        } else {
                            "N/A".to_string()
                        };
                        summary.wallets.push(GroupWalletLine {
                            label,
                            balance,
                            error,
                        });
                    }
                    if summary.wallet_count == 0 {
                        summary.wallet_count = summary.wallets.len();
                    }
                }
            }
        }
        self.group_summary = Some(summary);
    }

    // -- Rendering --

    fn draw(&mut self, terminal: &mut TuiTerminal) -> io::Result<()> {
        terminal.draw(|frame| self.render(frame)).map(|_| ())
    }

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();

        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(1)])
            .split(area);

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(22), Constraint::Min(30)])
            .split(outer[0]);

        self.render_sidebar(frame, columns[0]);
        self.render_main(frame, columns[1]);

        self.render_status_bar(frame, outer[1]);

        self.position_cursor(frame, columns[1], outer[1]);

        if let Some(modal) = &self.modal {
            let modal_area = centered_rect(60, 35, area);
            frame.render_widget(Clear, modal_area);
            let mut text = Vec::new();
            for line in &modal.lines {
                text.push(Line::from(line.clone()));
            }
            text.push(Line::from(""));
            text.push(Line::styled(
                modal.hint.clone(),
                Style::default().fg(Color::Cyan),
            ));
            let widget = Paragraph::new(text)
                .block(
                    Block::default()
                        .title(modal.title.clone())
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(widget, modal_area);
        }
    }

    fn render_sidebar(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == TuiFocus::Sidebar;
        let border_style = if is_focused {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut items: Vec<ListItem<'static>> = Vec::new();
        for (gi, group) in self.wallet_groups.iter().enumerate() {
            let arrow = if group.expanded {
                "\u{25bc}"
            } else {
                "\u{25b6}"
            };
            let group_selected = is_focused && self.sidebar_cursor == SidebarItem::Group(gi);
            let group_style = if group_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            };
            items.push(ListItem::new(Line::styled(
                format!("{arrow} {}", group.network),
                group_style,
            )));

            if group.expanded {
                for (wi, wallet) in group.wallets.iter().enumerate() {
                    let is_selected =
                        is_focused && self.sidebar_cursor == SidebarItem::Wallet(gi, wi);
                    let style = if is_selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Green)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    items.push(ListItem::new(Line::styled(
                        format!("    {}", wallet.display_short()),
                        style,
                    )));
                }
            }
        }

        if items.is_empty() {
            items.push(ListItem::new(Line::styled(
                "(no wallets)",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Divider + Limits section
        items.push(ListItem::new(Line::styled(
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            Style::default().fg(Color::DarkGray),
        )));

        let limit_header_selected = is_focused && self.sidebar_cursor == SidebarItem::LimitHeader;
        let limit_header_style = if limit_header_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
        let limit_count = self.limit_records.len();
        items.push(ListItem::new(Line::styled(
            format!("\u{25b8} Limits ({limit_count})"),
            limit_header_style,
        )));

        for (li, lim) in self.limit_records.iter().enumerate() {
            let is_selected = is_focused && self.sidebar_cursor == SidebarItem::Limit(li);
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let short = format!("  {} {}", lim.scope, lim.max_spend);
            items.push(ListItem::new(Line::styled(short, style)));
        }

        // Config section
        let config_selected = is_focused && self.sidebar_cursor == SidebarItem::ConfigHeader;
        let config_style = if config_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
        items.push(ListItem::new(Line::styled("\u{25b8} Config", config_style)));

        // Data section
        let data_selected = is_focused && self.sidebar_cursor == SidebarItem::DataHeader;
        let data_style = if data_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
        items.push(ListItem::new(Line::styled("\u{25b8} Data", data_style)));

        let widget = List::new(items).block(
            Block::default()
                .title("Wallets")
                .borders(Borders::ALL)
                .border_style(border_style),
        );
        frame.render_widget(widget, area);
    }

    fn render_main(&self, frame: &mut Frame, area: Rect) {
        match self.view {
            TuiView::WalletDetail => self.render_wallet_detail(frame, area),
            TuiView::GroupSummary => self.render_group_summary(frame, area),
            TuiView::Send
            | TuiView::Receive
            | TuiView::WalletCreate
            | TuiView::WalletClose
            | TuiView::WalletShowSeed
            | TuiView::LimitAdd
            | TuiView::WalletConfig
            | TuiView::GlobalConfig => self.render_form_view(frame, area),
            TuiView::History => self.render_history(frame, area),
            TuiView::HistoryDetail => self.render_history_detail(frame, area),
            TuiView::Limits | TuiView::LimitDetail => self.render_limits(frame, area),
            TuiView::CommandResult => self.render_command_result(frame, area),
            TuiView::DataView => self.render_data_view(frame, area),
        }
    }

    fn render_group_summary(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.focus == TuiFocus::Main {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        let dim = Style::default().fg(Color::DarkGray);
        let bold = Style::default().add_modifier(Modifier::BOLD);

        if let Some(s) = &self.group_summary {
            if s.wallet_count == 0 {
                lines.push(Line::from(""));
                lines.push(Line::styled("  No wallets. Press c to create one.", dim));
            } else {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("  Total       ", dim),
                    Span::styled(format!("{} {}", s.confirmed, s.unit), bold),
                    if s.pending > 0 {
                        Span::styled(
                            format!(" (+{} pending)", s.pending),
                            Style::default().fg(Color::Yellow),
                        )
                    } else {
                        Span::raw("")
                    },
                    if s.errors > 0 {
                        Span::styled(
                            format!("  ({} errors)", s.errors),
                            Style::default().fg(Color::Red),
                        )
                    } else {
                        Span::raw("")
                    },
                ]));

                lines.push(Line::from(""));
                lines.push(Line::styled(
                    format!("  -- {} Wallets --", s.wallet_count),
                    dim,
                ));

                for w in &s.wallets {
                    if let Some(err) = &w.error {
                        lines.push(Line::from(vec![
                            Span::raw(format!("  {:<14}", w.label)),
                            Span::styled(format!("error: {err}"), Style::default().fg(Color::Red)),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::raw(format!("  {:<14}", w.label)),
                            Span::styled(w.balance.clone(), bold),
                        ]));
                    }
                }
            }
        } else {
            lines.push(Line::styled(
                "  Loading...",
                Style::default().fg(Color::DarkGray),
            ));
        }

        let title = self
            .group_summary
            .as_ref()
            .map(|s| format!("{} Summary", s.network))
            .unwrap_or_else(|| "Summary".to_string());

        let widget = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, area);
    }

    fn render_wallet_detail(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == TuiFocus::Main;
        let border_style = if is_focused {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let d = &self.wallet_data;
        let mut lines: Vec<Line<'static>> = Vec::new();

        if d.wallet_id.is_none() {
            lines.push(Line::styled(
                "Select a wallet from the sidebar.",
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            let dim = Style::default().fg(Color::DarkGray);
            let normal = Style::default();
            let bold = Style::default().add_modifier(Modifier::BOLD);

            if let Some(net) = &d.network {
                lines.push(Line::from(vec![
                    Span::styled("  Network     ", dim),
                    Span::styled(net.clone(), normal),
                ]));
            }
            if let Some(label) = &d.label {
                lines.push(Line::from(vec![
                    Span::styled("  Label       ", dim),
                    Span::styled(label.clone(), normal),
                ]));
            }
            // Show address only if it differs from mint_url (cashu uses mint URL as address)
            if let Some(addr) = &d.address {
                if d.mint_url.as_deref() != Some(addr) {
                    let short = if addr.len() > 30 {
                        format!("{}...", &addr[..30])
                    } else {
                        addr.clone()
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  Address     ", dim),
                        Span::styled(short, normal),
                    ]));
                }
            }
            if let Some(mint) = &d.mint_url {
                lines.push(Line::from(vec![
                    Span::styled("  Mint        ", dim),
                    Span::styled(mint.clone(), normal),
                ]));
            }
            if let Some(backend) = &d.backend {
                lines.push(Line::from(vec![
                    Span::styled("  Backend     ", dim),
                    Span::styled(backend.clone(), normal),
                ]));
            }
            if let Some(created) = &d.created_at {
                lines.push(Line::from(vec![
                    Span::styled("  Created     ", dim),
                    Span::styled(created.clone(), normal),
                ]));
            }

            lines.push(Line::from(""));

            if let Some(bal_text) = &d.balance_text {
                let mut first = true;
                for line in bal_text.lines() {
                    let label = if first {
                        "  Balance     "
                    } else {
                        "              "
                    };
                    first = false;
                    lines.push(Line::from(vec![
                        Span::styled(label, dim),
                        Span::styled(line.to_string(), bold),
                    ]));
                }
            } else if let Some(err) = &d.balance_error {
                lines.push(Line::from(vec![
                    Span::styled("  Balance     ", dim),
                    Span::styled(format!("error: {err}"), Style::default().fg(Color::Red)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("  Balance     ", dim),
                    Span::styled("loading...", Style::default().fg(Color::DarkGray)),
                ]));
            }

            // Show recent command output below if any
            if !self.messages.is_empty() {
                lines.push(Line::from(""));
                let msg_height = area.height.saturating_sub(lines.len() as u16 + 3) as usize;
                for msg_line in self.message_lines(msg_height) {
                    lines.push(msg_line);
                }
            }
        }

        let base = d
            .label
            .clone()
            .or_else(|| {
                d.wallet_id.as_ref().map(|id| {
                    if id.len() > 16 {
                        format!("{}...", &id[..16])
                    } else {
                        id.clone()
                    }
                })
            })
            .unwrap_or_else(|| "Wallet".to_string());
        let title = match &d.network {
            Some(net) => format!("{net}/{base}"),
            None => base,
        };

        let widget = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, area);
    }

    fn render_form_view(&self, frame: &mut Frame, area: Rect) {
        let form = match self.current_form() {
            Some(f) => f,
            None => return,
        };
        let is_focused = self.focus == TuiFocus::Main;
        let border_style = if is_focused {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut lines: Vec<Line<'static>> = Vec::new();

        let max_label = form
            .fields
            .iter()
            .map(|f| f.label.chars().count())
            .max()
            .unwrap_or(0);

        for (index, field) in form.fields.iter().enumerate() {
            let is_selected = is_focused && form.selected_field == index;
            let (value, is_placeholder) = field.value.display_value(field.placeholder);

            let label_style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if field.required {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Cyan)
            };

            let value_style = if field.locked {
                Style::default().fg(Color::DarkGray)
            } else if is_selected && field.value.is_text() {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if is_placeholder {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };

            let padded = format!("  {:width$}  ", field.label, width = max_label);

            let value_display = match &field.value {
                TuiFieldValue::Choice { .. } => format!("[{value} \u{25be}]"),
                TuiFieldValue::Toggle(_) => format!("[{value}]"),
                TuiFieldValue::Text(_) => value,
            };

            lines.push(Line::from(vec![
                Span::styled(padded, label_style),
                Span::styled(value_display, value_style),
                Span::raw("  "),
                Span::styled(field.hint, Style::default().fg(Color::DarkGray)),
            ]));
        }

        // Preview
        lines.push(Line::from(""));
        match self.build_form_command() {
            Ok(cmd) => lines.push(Line::from(vec![
                Span::styled("  > ", Style::default().fg(Color::Cyan)),
                Span::raw(cmd),
            ])),
            Err(err) => lines.push(Line::from(vec![
                Span::styled("  > ", Style::default().fg(Color::Red)),
                Span::styled(err, Style::default().fg(Color::Red)),
            ])),
        }

        let submit_style = if self.form_on_submit {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        lines.push(Line::styled(
            "            [ Enter to submit ]",
            submit_style,
        ));

        // Show result output below form if any
        if !self.messages.is_empty() {
            lines.push(Line::from(""));
            let remaining = area.height.saturating_sub(lines.len() as u16 + 3) as usize;
            for msg_line in self.message_lines(remaining) {
                lines.push(msg_line);
            }
        }

        let (ctx_network, ctx_wallet) = self.form_context();
        let ctx_label = ctx_wallet.as_deref().unwrap_or(&ctx_network);
        let title = format!("{} \u{00b7} {}", form.title, ctx_label);
        let widget = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, area);
    }

    fn render_history(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.focus == TuiFocus::Main {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.history_records.is_empty() {
            lines.push(Line::styled(
                "  No transactions.",
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            let visible_height = area.height.saturating_sub(2) as usize;
            // Scroll window to keep selected item visible
            let start = if self.selected_history >= visible_height {
                self.selected_history - visible_height + 1
            } else {
                0
            };
            let end = min(self.history_records.len(), start + visible_height);

            for (i, rec) in self.history_records[start..end].iter().enumerate() {
                let abs_i = start + i;
                let is_selected = self.focus == TuiFocus::Main && abs_i == self.selected_history;
                let dir_style = if rec.direction == "\u{2191}" {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Red)
                };
                let status_style = match rec.status.as_str() {
                    "confirmed" => Style::default().fg(Color::Green),
                    "pending" => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::Red),
                };
                let status_icon = match rec.status.as_str() {
                    "confirmed" => "\u{2713}", // ✓
                    "pending" => "\u{25cb}",   // ○
                    "failed" => "\u{2717}",    // ✗
                    _ => "?",
                };
                let prefix = if is_selected { "> " } else { "  " };
                let mut spans = vec![Span::styled(
                    prefix.to_string(),
                    if is_selected {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default()
                    },
                )];
                if self.history_is_network {
                    if let Some(w) = &rec.wallet {
                        // Resolve wallet label from sidebar groups
                        let label = self
                            .wallet_groups
                            .iter()
                            .flat_map(|g| &g.wallets)
                            .find(|e| e.id == *w)
                            .map(|e| e.display_short())
                            .unwrap_or_else(|| {
                                if w.len() > 10 {
                                    format!("{}...", &w[..10])
                                } else {
                                    w.clone()
                                }
                            });
                        spans.push(Span::styled(
                            format!("{label:<12} "),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                spans.extend([
                    Span::styled(rec.direction.clone(), dir_style),
                    Span::raw(format!(" {:>12}  ", rec.amount)),
                    Span::styled(rec.date.clone(), Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled(status_icon.to_string(), status_style),
                ]);
                // Prefer local_memo (user-written), fall back to onchain_memo
                let display_memo = rec.local_memo.as_deref().or(rec.memo.as_deref());
                if let Some(memo) = display_memo {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        memo.to_string(),
                        Style::default().fg(Color::Cyan),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }

        let (hist_net, hist_wallet) = self.form_context();
        let hist_label = hist_wallet.as_deref().unwrap_or(&hist_net);
        let title = format!("History \u{00b7} {}", hist_label);
        let widget = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, area);
    }

    fn render_history_detail(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.focus == TuiFocus::Main {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let dim = Style::default().fg(Color::DarkGray);

        let mut lines: Vec<Line<'static>> = Vec::new();
        if let Some(text) = &self.history_detail_text {
            for line in text.lines() {
                lines.push(Line::from(format!("  {line}")));
            }
        } else {
            lines.push(Line::styled("  Loading...", dim));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled("  Esc: back to history", dim));

        let widget = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title("Transaction Detail")
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, area);
    }

    fn render_limits(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.focus == TuiFocus::Main {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut items: Vec<ListItem<'static>> = Vec::new();
        if self.limit_records.is_empty() {
            items.push(ListItem::new(Line::styled(
                "  No spend limits configured.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (i, lim) in self.limit_records.iter().enumerate() {
                let is_selected = self.focus == TuiFocus::Main && self.selected_limit == i;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                items.push(ListItem::new(Line::styled(
                    format!(
                        "  {:10}  {}/{:<6}  spent {} / {}",
                        lim.scope, lim.max_spend, lim.window, lim.spent, lim.remaining
                    ),
                    style,
                )));
            }
        }

        let widget = List::new(items).block(
            Block::default()
                .title("Spend Limits")
                .borders(Borders::ALL)
                .border_style(border_style),
        );
        frame.render_widget(widget, area);
    }

    fn render_command_result(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.focus == TuiFocus::Main {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let dim = Style::default().fg(Color::DarkGray);

        // If there's a copyable value (token/invoice/address), split the area:
        // top = output log, bottom = copyable strip
        if let Some(ref copyable) = self.last_copyable {
            let copyable_lines = copyable
                .as_bytes()
                .chunks(area.width.saturating_sub(2) as usize)
                .count()
                .max(1) as u16;
            let strip_height = copyable_lines + 2; // +2 for border
            let strip_height = strip_height.min(area.height.saturating_sub(4));
            let output_height = area.height.saturating_sub(strip_height);

            let chunks = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Length(output_height),
                    ratatui::layout::Constraint::Length(strip_height),
                ])
                .split(area);

            let log_height = output_height.saturating_sub(2) as usize;
            let log_widget = Paragraph::new(Text::from(self.message_lines(log_height)))
                .block(
                    Block::default()
                        .title("Output")
                        .borders(Borders::ALL)
                        .border_style(border_style),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(log_widget, chunks[0]);

            // Token/invoice strip — no padding so mouse-selection is clean
            let token_title = " token — y to copy ";
            let inner_w = chunks[1].width.saturating_sub(2) as usize;
            let token_lines: Vec<Line<'static>> = copyable
                .as_bytes()
                .chunks(inner_w.max(1))
                .map(|chunk| {
                    let s = String::from_utf8_lossy(chunk).into_owned();
                    Line::raw(s)
                })
                .collect();
            let token_widget = Paragraph::new(Text::from(token_lines)).block(
                Block::default()
                    .title(token_title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            );
            frame.render_widget(token_widget, chunks[1]);

            let hint = Line::from(vec![
                Span::styled("  y ", Style::default().fg(Color::Cyan)),
                Span::styled("copy  ", dim),
            ]);
            let _ = hint; // rendered via block title above
        } else {
            let output_height = area.height.saturating_sub(2) as usize;
            let widget = Paragraph::new(Text::from(self.message_lines(output_height)))
                .block(
                    Block::default()
                        .title("Output")
                        .borders(Borders::ALL)
                        .border_style(border_style),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(widget, area);
        }
    }

    fn render_data_view(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == TuiFocus::Main;
        let border_style = if is_focused {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let dim = Style::default().fg(Color::DarkGray);
        let cyan = Style::default().fg(Color::Cyan);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let green = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let yellow = Style::default().fg(Color::Yellow);
        let red = Style::default().fg(Color::Red);
        let selected_label = Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let selected_value = Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD);

        let mut lines: Vec<Line<'static>> = Vec::new();
        // constant label width for alignment (matches form view)
        let label_w = 12usize;

        // ── Mode choice row (field 0) ────────────────────────────────────────
        let mode_label = format!("  {:width$}  ", "mode", width = label_w);
        let mode_options = ["Backup", "Restore"];
        let mode_text = format!("[{} \u{25be}]", mode_options[self.data_cursor]);
        let (mode_lbl_style, mode_val_style) = if is_focused && self.data_field_cursor == 0 {
            (selected_label, selected_value)
        } else {
            (cyan, bold)
        };
        lines.push(Line::from(vec![
            Span::styled(mode_label, mode_lbl_style),
            Span::styled(mode_text, mode_val_style),
            Span::raw("  "),
            Span::styled(
                "\u{2190}\u{2192} or \u{21b5} switch mode, \u{2193} to fields",
                dim,
            ),
        ]));
        lines.push(Line::from(""));

        // ── Fields for current mode ──────────────────────────────────────────
        let status = if self.data_cursor == 0 {
            &self.data_backup_status
        } else {
            &self.data_restore_status
        };

        let render_text_field = |lines: &mut Vec<Line<'static>>,
                                 label: &'static str,
                                 value: &str,
                                 placeholder: &'static str,
                                 field_idx: usize,
                                 is_focused: bool,
                                 data_field_cursor: usize,
                                 data_on_submit: bool| {
            let lbl = format!("  {:width$}  ", label, width = label_w);
            let (is_placeholder, display) = if value.is_empty() {
                (true, placeholder.to_string())
            } else {
                (false, value.to_string())
            };
            let is_selected = is_focused && !data_on_submit && data_field_cursor == field_idx;
            let lbl_style = if is_selected {
                selected_label
            } else if placeholder == "required" {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let val_style = if is_selected {
                selected_value
            } else if is_placeholder {
                dim
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(lbl, lbl_style),
                Span::styled(display, val_style),
            ]));
        };

        if self.data_cursor == 0 {
            // Backup: output path (optional)
            render_text_field(
                &mut lines,
                "output",
                &self.data_backup_output.clone(),
                "auto-generated",
                1,
                is_focused,
                self.data_field_cursor,
                self.data_on_submit,
            );
        } else {
            // Restore: archive (required), overwrite (toggle), pg-url (optional)
            render_text_field(
                &mut lines,
                "archive",
                &self.data_restore_archive.clone(),
                "required",
                1,
                is_focused,
                self.data_field_cursor,
                self.data_on_submit,
            );
            // Overwrite toggle (field 2)
            let ovr_lbl = format!("  {:width$}  ", "overwrite", width = label_w);
            let ovr_val = if self.data_restore_overwrite {
                "yes"
            } else {
                "no"
            };
            let is_ovr = is_focused && !self.data_on_submit && self.data_field_cursor == 2;
            lines.push(Line::from(vec![
                Span::styled(ovr_lbl, if is_ovr { selected_label } else { cyan }),
                Span::styled(
                    format!("[{ovr_val}]"),
                    if is_ovr {
                        selected_value
                    } else {
                        Style::default()
                    },
                ),
                Span::raw("  "),
                Span::styled("\u{2190}\u{2192} toggle", dim),
            ]));
            render_text_field(
                &mut lines,
                "pg-url",
                &self.data_restore_pg_url.clone(),
                "optional",
                3,
                is_focused,
                self.data_field_cursor,
                self.data_on_submit,
            );
        }

        lines.push(Line::from(""));

        // ── Submit button ────────────────────────────────────────────────────
        let btn_label = if self.data_cursor == 0 {
            "  [ Run Backup  ]"
        } else {
            "  [ Run Restore ]"
        };
        let btn_style = if is_focused && self.data_on_submit {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            bold
        };
        let status_span = match status {
            DataOpStatus::Idle => Span::styled("  \u{25cb} idle", dim),
            DataOpStatus::Running => Span::styled("  \u{25cf} running\u{2026}", yellow),
            DataOpStatus::Done(msg) => Span::styled(format!("  \u{2713} {msg}"), green),
            DataOpStatus::Error(msg) => Span::styled(format!("  \u{2717} {msg}"), red),
        };
        lines.push(Line::from(vec![
            Span::styled(btn_label.to_string(), btn_style),
            status_span,
        ]));
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "  \u{2191}\u{2193} fields  \u{21b5} run  Esc back",
            dim,
        ));

        let widget = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title("Data")
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, area);
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        let dim = Style::default().fg(Color::DarkGray);
        let key = Style::default().fg(Color::Cyan);
        let sep = Span::styled(" \u{2502} ", dim);

        // Show what the sidebar cursor is on
        let cursor_label = match self.sidebar_cursor {
            SidebarItem::Wallet(gi, wi) => {
                let group = &self.wallet_groups[gi];
                let wallet = &group.wallets[wi];
                let net = group.network.to_string().to_lowercase();
                let name = wallet.display_short();
                format!("{net}/{name}")
            }
            SidebarItem::Group(gi) => self
                .wallet_groups
                .get(gi)
                .map(|g| g.network.to_string().to_lowercase())
                .unwrap_or_default(),
            SidebarItem::LimitHeader => "limits".to_string(),
            SidebarItem::Limit(li) => self
                .limit_records
                .get(li)
                .map(|r| r.rule_id.clone())
                .unwrap_or_else(|| "limit".to_string()),
            SidebarItem::ConfigHeader => "config".to_string(),
            SidebarItem::DataHeader => "data".to_string(),
        };

        let mut spans = vec![
            Span::styled(format!(" {} ", self.connection_label), dim),
            sep.clone(),
            Span::styled(cursor_label, Style::default().fg(Color::Green)),
            Span::raw(" "),
            sep.clone(),
        ];

        let on_wallet = matches!(self.sidebar_cursor, SidebarItem::Wallet(_, _));
        let on_group_with_wallets = match self.sidebar_cursor {
            SidebarItem::Group(gi) => self
                .wallet_groups
                .get(gi)
                .is_some_and(|g| !g.wallets.is_empty()),
            _ => false,
        };

        // Context-aware hotkey hints based on sidebar cursor
        if self.current_form().is_some() {
            spans.extend([
                Span::styled("Enter", key),
                Span::styled(" submit ", dim),
                Span::styled("Esc", key),
                Span::styled(" cancel ", dim),
            ]);
        } else if on_wallet {
            // On a specific wallet
            spans.extend([
                Span::styled("s", key),
                Span::styled(" send ", dim),
                Span::styled("r", key),
                Span::styled(" recv ", dim),
                Span::styled("h", key),
                Span::styled(" hist ", dim),
                Span::styled("x", key),
                Span::styled(" close ", dim),
                Span::styled("D", key),
                Span::styled(" seed ", dim),
                Span::styled("e", key),
                Span::styled(" config ", dim),
            ]);
        } else if on_group_with_wallets {
            // On a network group that has wallets
            spans.extend([
                Span::styled("c", key),
                Span::styled(" create ", dim),
                Span::styled("s", key),
                Span::styled(" send ", dim),
                Span::styled("r", key),
                Span::styled(" recv ", dim),
                Span::styled("h", key),
                Span::styled(" hist ", dim),
            ]);
        } else if matches!(self.sidebar_cursor, SidebarItem::LimitHeader) {
            spans.extend([Span::styled("a", key), Span::styled(" add ", dim)]);
        } else if matches!(self.sidebar_cursor, SidebarItem::Limit(_)) {
            spans.extend([Span::styled("d", key), Span::styled(" delete ", dim)]);
        } else if matches!(self.sidebar_cursor, SidebarItem::ConfigHeader) {
            // No special hotkeys for config — just view
        } else if matches!(self.sidebar_cursor, SidebarItem::DataHeader) {
            spans.extend([Span::styled("Tab", key), Span::styled(" edit ", dim)]);
        } else {
            // On an empty network group
            spans.extend([Span::styled("c", key), Span::styled(" create ", dim)]);
        }

        if self.current_form().is_none() {
            spans.extend([Span::styled("R", key), Span::styled(" refresh ", dim)]);
        }
        if self.last_copyable.is_some() {
            spans.extend([Span::styled("y", key), Span::styled(" copy ", dim)]);
        }
        spans.extend([
            sep.clone(),
            Span::styled(format!("v{VERSION} {} ", mode_name(self.frontend)), dim),
            sep,
            Span::styled("q", key),
            Span::styled(" quit", dim),
        ]);

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn position_cursor(&self, frame: &mut Frame, main_area: Rect, _status_area: Rect) {
        if self.focus == TuiFocus::Main && self.current_form().is_some() {
            if let Some(form) = self.current_form() {
                if let Some(field) = form.fields.get(form.selected_field) {
                    if let Some(text) = field.value.as_text() {
                        let max_label = form
                            .fields
                            .iter()
                            .map(|f| f.label.chars().count())
                            .max()
                            .unwrap_or(0);
                        let label_width = max_label as u16 + 4; // "  label  "
                        let cursor_offset =
                            min(self.field_cursor_chars, text.chars().count()) as u16;
                        let inner_x = main_area.x.saturating_add(1);
                        let inner_y = main_area.y.saturating_add(1);
                        let line_offset = form.selected_field as u16;
                        let max_x = main_area
                            .x
                            .saturating_add(main_area.width.saturating_sub(2))
                            .saturating_sub(1);
                        let cx = min(inner_x.saturating_add(label_width + cursor_offset), max_x);
                        let cy = inner_y.saturating_add(line_offset);
                        frame.set_cursor_position((cx, cy));
                    }
                }
            }
        } else if self.view == TuiView::DataView
            && self.focus == TuiFocus::Main
            && !self.data_on_submit
            && data_current_field_is_text(self)
        {
            // DataView text field cursor: label_w=12, "  label  " = 12+4=16 chars
            let label_width: u16 = 12 + 4;
            let text_len = data_field_text_len(self);
            let cursor_offset = min(self.data_cursor_chars, text_len) as u16;
            let inner_x = main_area.x.saturating_add(1);
            let inner_y = main_area.y.saturating_add(1);
            // Line layout: row0=mode, row1=empty, then fields start at row2
            // For backup mode: field1 (output) is at line 2 in the paragraph
            // For restore mode: field1 (archive)=line2, field2 (overwrite)=line3, field3 (pg-url)=line4
            let field_line: u16 = match (self.data_cursor, self.data_field_cursor) {
                (0, 1) => 2,
                (1, 1) => 2,
                (1, 3) => 4,
                _ => return,
            };
            let max_x = main_area
                .x
                .saturating_add(main_area.width.saturating_sub(2))
                .saturating_sub(1);
            let cx = min(inner_x.saturating_add(label_width + cursor_offset), max_x);
            let cy = inner_y.saturating_add(field_line);
            frame.set_cursor_position((cx, cy));
        }
    }

    fn message_lines(&self, height: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for message in &self.messages {
            let style = match message.kind {
                TuiMessageKind::Command => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                TuiMessageKind::Output => Style::default(),
                TuiMessageKind::Notice => Style::default().fg(Color::Yellow),
            };
            let text = if message.text.is_empty() {
                vec![String::new()]
            } else {
                message.text.lines().map(ToString::to_string).collect()
            };
            for line in text {
                let rendered = if matches!(message.kind, TuiMessageKind::Command) {
                    format!("> {line}")
                } else {
                    format!("  {line}")
                };
                lines.push(Line::styled(rendered, style));
            }
        }

        if lines.is_empty() {
            return lines;
        }

        let visible = height.max(1);
        let max_offset = lines.len().saturating_sub(visible);
        let scroll = self.output_scroll.min(max_offset);
        let start = max_offset.saturating_sub(scroll);
        let end = min(lines.len(), start + visible);
        lines[start..end].to_vec()
    }

    // -- Modals --

    fn prompt_yes_no(
        &mut self,
        terminal: &mut TuiTerminal,
        title: &str,
        lines: Vec<String>,
    ) -> bool {
        self.modal = Some(TuiModal {
            title: title.to_string(),
            lines,
            hint: "Press y to confirm, n or Esc to cancel".to_string(),
        });
        let result = loop {
            let _ = self.draw(terminal);
            match event::read() {
                Ok(Event::Key(key)) => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => break true,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Enter => {
                        break false
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => break false,
            }
        };
        self.modal = None;
        result
    }

    fn prompt_claim(&mut self, terminal: &mut TuiTerminal) -> bool {
        self.modal = Some(TuiModal {
            title: "Claim Deposit".to_string(),
            lines: vec!["Pay the invoice above, then claim the receive quote.".to_string()],
            hint: "Press Enter to claim, s or Esc to skip".to_string(),
        });
        let result = loop {
            let _ = self.draw(terminal);
            match event::read() {
                Ok(Event::Key(key)) => match key.code {
                    KeyCode::Enter => break true,
                    KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Esc => break false,
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => break false,
            }
        };
        self.modal = None;
        result
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Try to copy `text` to the system clipboard via pbcopy/wl-copy/xclip.
/// Returns true if the copy likely succeeded.
fn try_copy_to_clipboard(text: &str) -> bool {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    // macOS
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }
    // Wayland
    if let Ok(mut child) = Command::new("wl-copy")
        .arg(text)
        .stdin(Stdio::null())
        .spawn()
    {
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }
    // X11
    if let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}

fn shell_quote(value: &str) -> String {
    let safe = value.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@' | ',' | '=')
    });
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn format_epoch(epoch_s: u64) -> String {
    if epoch_s == 0 {
        return "unknown".to_string();
    }
    // Simple UTC date formatting without chrono dependency
    let days = epoch_s / 86400;
    let secs_in_day = epoch_s % 86400;
    let hours = secs_in_day / 3600;
    let minutes = (secs_in_day % 3600) / 60;

    // Days since 1970-01-01
    let mut y = 1970i64;
    let mut remaining_days = days as i64;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days: &[i64] = if leap {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining_days < md {
            m = i;
            break;
        }
        remaining_days -= md;
    }
    let d = remaining_days + 1;
    format!("{y:04}-{:02}-{:02} {hours:02}:{minutes:02}", m + 1, d)
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

// ---------------------------------------------------------------------------
// TuiHost (preserved)
// ---------------------------------------------------------------------------

struct TuiHost<'a> {
    app: &'a mut TuiApp,
    terminal: &'a mut TuiTerminal,
}

impl InteractionHost for TuiHost<'_> {
    fn emit(&mut self, kind: HostMessageKind, text: String) {
        let message_kind = match kind {
            HostMessageKind::Output => TuiMessageKind::Output,
            HostMessageKind::Notice => TuiMessageKind::Notice,
        };
        // Extract long copyable values (token, bolt11 invoice, address, seed phrase)
        if matches!(kind, HostMessageKind::Output) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                let copyable = v
                    .get("token")
                    .or_else(|| v.get("bolt11"))
                    .or_else(|| v.get("invoice"))
                    .or_else(|| v.get("address"))
                    .and_then(|val| val.as_str())
                    .filter(|s| s.len() > 20)
                    .map(ToString::to_string);
                if let Some(val) = copyable {
                    self.app.last_copyable = Some(val);
                }
            }
        }
        self.app.push_message(message_kind, text);
    }

    fn confirm_send(&mut self, wallet: &str, amount: u64, to: &str) -> bool {
        let target = if to.is_empty() {
            "P2P cashu token".to_string()
        } else if to.len() > 40 {
            format!("{}...", &to[..40])
        } else {
            to.to_string()
        };
        self.app.prompt_yes_no(
            self.terminal,
            "Confirm Send",
            vec![format!("Send {amount} sats from {wallet} to {target}")],
        )
    }

    fn confirm_send_with_fee(
        &mut self,
        wallet: &str,
        amount: u64,
        fee: u64,
        fee_unit: &str,
    ) -> bool {
        let total = amount + fee;
        let mut lines = vec![format!(
            "Send {amount} {fee_unit} from {wallet} as P2P cashu token"
        )];
        if fee > 0 {
            lines.push(format!(
                "Fee: {fee} {fee_unit}  (total: {total} {fee_unit})"
            ));
        }
        self.app
            .prompt_yes_no(self.terminal, "Confirm Cashu Send", lines)
    }

    fn confirm_withdraw(
        &mut self,
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
        self.app.prompt_yes_no(
            self.terminal,
            "Confirm Payment",
            vec![
                format!("Pay {amount} {fee_unit} from {wallet} to {target}"),
                format!("Fee estimate: {fee_estimate} {fee_unit}  (total: {total} {fee_unit})"),
            ],
        )
    }

    fn prompt_deposit_claim(&mut self, _wallet: &str, _quote_id: &str) -> bool {
        self.app.prompt_claim(self.terminal)
    }
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum TuiAction {
    None,
    Submit(String),
    SelectWallet(String),
    SelectGroup(Network),
    FetchHistory {
        wallet: Option<String>,
        network: Option<Network>,
    },
    FetchLimits,
    FetchHistoryDetail(String), // transaction_id
    ShowLimitDetail(usize),
    ShowGlobalConfig,
    ShowDataView,
    RunDataOp,
    RefreshWallets,
    Quit,
}

fn handle_tui_key(app: &mut TuiApp, key: KeyEvent) -> TuiAction {
    // 1. Global Ctrl
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('d') => return TuiAction::Quit,
            KeyCode::Char('l') => {
                app.clear_messages();
                return TuiAction::None;
            }
            _ => {}
        }
    }

    // 2. Data view form handling (when DataView is focused)
    if app.view == TuiView::DataView && app.focus == TuiFocus::Main {
        return handle_data_form_key(app, key);
    }

    // 3. Form editing (Send/Receive/WalletCreate/WalletClose/WalletShowSeed)
    if app.focus == TuiFocus::Main && app.current_form().is_some() {
        return handle_form_key(app, key);
    }

    // 4. Hotkeys (when not editing text)
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        if let Some(action) = handle_hotkey(app, key.code) {
            return action;
        }
    }

    // 5. Focus-specific
    match app.focus {
        TuiFocus::Sidebar => handle_sidebar_key(app, key),
        TuiFocus::Main => handle_main_key(app, key),
    }
}

fn handle_hotkey(app: &mut TuiApp, code: KeyCode) -> Option<TuiAction> {
    let on_wallet = app.sidebar_wallet_id().is_some();
    let on_group_with_wallets = match app.sidebar_cursor {
        SidebarItem::Group(gi) => app
            .wallet_groups
            .get(gi)
            .is_some_and(|g| !g.wallets.is_empty()),
        _ => false,
    };
    match code {
        KeyCode::Char('s') if on_wallet || on_group_with_wallets => {
            let (network, wallet_id) = app.form_context();
            app.send_form = make_send_form(&network, wallet_id.as_deref());
            app.switch_view(TuiView::Send);
            app.sync_field_cursor();
            Some(TuiAction::None)
        }
        KeyCode::Char('r') if on_wallet || on_group_with_wallets => {
            let (network, wallet_id) = app.form_context();
            app.receive_form = make_receive_form(&network, wallet_id.as_deref());
            app.switch_view(TuiView::Receive);
            app.sync_field_cursor();
            Some(TuiAction::None)
        }
        KeyCode::Char('c') if matches!(app.sidebar_cursor, SidebarItem::Group(_)) => {
            let (network, _) = app.form_context();
            app.wallet_create_form = make_wallet_create_form(&network);
            app.switch_view(TuiView::WalletCreate);
            app.sync_field_cursor();
            Some(TuiAction::None)
        }
        KeyCode::Char('x') if on_wallet => {
            let (network, wallet_id) = app.form_context();
            if let Some(wid) = wallet_id {
                app.wallet_close_form = make_wallet_close_form(&network, &wid);
                app.switch_view(TuiView::WalletClose);
                app.sync_field_cursor();
                Some(TuiAction::None)
            } else {
                None
            }
        }
        KeyCode::Char('D') if on_wallet => {
            let (network, wallet_id) = app.form_context();
            if let Some(wid) = wallet_id {
                app.wallet_show_seed_form = make_wallet_show_seed_form(&network, &wid);
                app.switch_view(TuiView::WalletShowSeed);
                app.sync_field_cursor();
                Some(TuiAction::None)
            } else {
                None
            }
        }
        KeyCode::Char('e') if on_wallet => {
            let (network, wallet_id) = app.form_context();
            if let Some(wid) = wallet_id {
                app.wallet_config_form = make_wallet_config_form(&network);
                app.config_network = network;
                app.config_wallet_id = wid;
                app.switch_view(TuiView::WalletConfig);
                app.sync_field_cursor();
                Some(TuiAction::None)
            } else {
                None
            }
        }
        KeyCode::Char('h') if on_wallet || on_group_with_wallets => {
            app.history_records.clear();
            app.selected_history = 0;
            let (wallet, network) = match app.sidebar_cursor {
                SidebarItem::Wallet(gi, wi) => {
                    let wid = app.wallet_groups[gi].wallets[wi].id.clone();
                    app.history_is_network = false;
                    (Some(wid), None)
                }
                SidebarItem::Group(gi) => {
                    let net = app.wallet_groups[gi].network;
                    app.history_is_network = true;
                    (None, Some(net))
                }
                _ => {
                    app.history_is_network = false;
                    (None, None)
                }
            };
            app.switch_view(TuiView::History);
            Some(TuiAction::FetchHistory { wallet, network })
        }
        KeyCode::Char('a')
            if matches!(app.view, TuiView::Limits | TuiView::LimitDetail)
                || matches!(
                    app.sidebar_cursor,
                    SidebarItem::LimitHeader | SidebarItem::Limit(_)
                ) =>
        {
            app.limit_add_form = make_limit_add_form();
            app.switch_view(TuiView::LimitAdd);
            app.sync_field_cursor();
            Some(TuiAction::None)
        }
        KeyCode::Char('d') if matches!(app.sidebar_cursor, SidebarItem::Limit(_)) => {
            if let SidebarItem::Limit(li) = app.sidebar_cursor {
                if let Some(lim) = app.limit_records.get(li) {
                    let cmd = format!("limit remove --rule-id {}", shell_quote(&lim.rule_id));
                    return Some(TuiAction::Submit(cmd));
                }
            }
            None
        }
        KeyCode::Char('y') => {
            if let Some(ref val) = app.last_copyable.clone() {
                if try_copy_to_clipboard(val) {
                    app.push_notice("copied to clipboard".to_string());
                } else {
                    app.push_notice("clipboard unavailable (no pbcopy/wl-copy/xclip)".to_string());
                }
            }
            Some(TuiAction::None)
        }
        KeyCode::Char('q') => Some(TuiAction::Quit),
        KeyCode::Char('R') => Some(TuiAction::RefreshWallets),
        _ => None,
    }
}

fn handle_sidebar_key(app: &mut TuiApp, key: KeyEvent) -> TuiAction {
    match key.code {
        KeyCode::Up => app.sidebar_move(-1),
        KeyCode::Down => app.sidebar_move(1),
        KeyCode::Enter => {
            // Enter on group: toggle expand/collapse
            app.sidebar_enter();
            TuiAction::None
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.focus = TuiFocus::Main;
            TuiAction::None
        }
        _ => TuiAction::None,
    }
}

fn handle_main_key(app: &mut TuiApp, key: KeyEvent) -> TuiAction {
    match key.code {
        KeyCode::Enter if app.view == TuiView::History => {
            if let Some(rec) = app.history_records.get(app.selected_history) {
                if !rec.transaction_id.is_empty() {
                    let tid = rec.transaction_id.clone();
                    return TuiAction::FetchHistoryDetail(tid);
                }
            }
            TuiAction::None
        }
        KeyCode::Esc if app.view == TuiView::HistoryDetail => {
            app.view = TuiView::History;
            app.history_detail_text = None;
            TuiAction::None
        }
        KeyCode::Esc => {
            if app.view != TuiView::WalletDetail && app.view != TuiView::GroupSummary {
                app.clear_messages();
                app.focus = TuiFocus::Sidebar;
                return app.sidebar_auto_action();
            }
            TuiAction::None
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.focus = TuiFocus::Sidebar;
            TuiAction::None
        }
        KeyCode::Up => {
            match app.view {
                TuiView::History => {
                    app.selected_history = app.selected_history.saturating_sub(1);
                }
                TuiView::Limits => {
                    app.selected_limit = app.selected_limit.saturating_sub(1);
                }
                TuiView::CommandResult | TuiView::WalletDetail => {
                    app.output_scroll = app.output_scroll.saturating_add(1);
                }
                _ => {}
            }
            TuiAction::None
        }
        KeyCode::Down => {
            match app.view {
                TuiView::History => {
                    if !app.history_records.is_empty() {
                        app.selected_history =
                            min(app.selected_history + 1, app.history_records.len() - 1);
                    }
                }
                TuiView::Limits => {
                    if !app.limit_records.is_empty() {
                        app.selected_limit =
                            min(app.selected_limit + 1, app.limit_records.len() - 1);
                    }
                }
                TuiView::CommandResult | TuiView::WalletDetail => {
                    app.output_scroll = app.output_scroll.saturating_sub(1);
                }
                _ => {}
            }
            TuiAction::None
        }
        KeyCode::PageUp => {
            app.output_scroll = app.output_scroll.saturating_add(8);
            app.selected_history = app.selected_history.saturating_sub(8);
            TuiAction::None
        }
        KeyCode::PageDown => {
            app.output_scroll = app.output_scroll.saturating_sub(8);
            if !app.history_records.is_empty() {
                app.selected_history = min(app.selected_history + 8, app.history_records.len() - 1);
            }
            TuiAction::None
        }
        _ => TuiAction::None,
    }
}

// ── Data view form helpers ──────────────────────────────────────────────────

fn data_max_field(app: &TuiApp) -> usize {
    if app.data_cursor == 0 {
        1
    } else {
        3
    }
}

fn data_current_field_is_text(app: &TuiApp) -> bool {
    !app.data_on_submit
        && match (app.data_cursor, app.data_field_cursor) {
            (_, 0) => false, // mode choice
            (0, 1) => true,  // backup: output path
            (1, 1) => true,  // restore: archive
            (1, 2) => false, // restore: overwrite toggle
            (1, 3) => true,  // restore: pg-url
            _ => false,
        }
}

fn data_field_text_len(app: &TuiApp) -> usize {
    match (app.data_cursor, app.data_field_cursor) {
        (0, 1) => app.data_backup_output.chars().count(),
        (1, 1) => app.data_restore_archive.chars().count(),
        (1, 3) => app.data_restore_pg_url.chars().count(),
        _ => 0,
    }
}

fn data_field_insert(app: &mut TuiApp, ch: char) {
    let cursor = app.data_cursor_chars;
    match (app.data_cursor, app.data_field_cursor) {
        (0, 1) => {
            let idx = char_to_byte_index(&app.data_backup_output, cursor);
            app.data_backup_output.insert(idx, ch);
            app.data_cursor_chars = cursor + 1;
        }
        (1, 1) => {
            let idx = char_to_byte_index(&app.data_restore_archive, cursor);
            app.data_restore_archive.insert(idx, ch);
            app.data_cursor_chars = cursor + 1;
        }
        (1, 3) => {
            let idx = char_to_byte_index(&app.data_restore_pg_url, cursor);
            app.data_restore_pg_url.insert(idx, ch);
            app.data_cursor_chars = cursor + 1;
        }
        _ => {}
    }
}

fn data_field_backspace(app: &mut TuiApp) {
    let cursor = app.data_cursor_chars;
    if cursor == 0 {
        return;
    }
    match (app.data_cursor, app.data_field_cursor) {
        (0, 1) => {
            let end = char_to_byte_index(&app.data_backup_output, cursor);
            let start = char_to_byte_index(&app.data_backup_output, cursor - 1);
            app.data_backup_output.drain(start..end);
            app.data_cursor_chars = cursor - 1;
        }
        (1, 1) => {
            let end = char_to_byte_index(&app.data_restore_archive, cursor);
            let start = char_to_byte_index(&app.data_restore_archive, cursor - 1);
            app.data_restore_archive.drain(start..end);
            app.data_cursor_chars = cursor - 1;
        }
        (1, 3) => {
            let end = char_to_byte_index(&app.data_restore_pg_url, cursor);
            let start = char_to_byte_index(&app.data_restore_pg_url, cursor - 1);
            app.data_restore_pg_url.drain(start..end);
            app.data_cursor_chars = cursor - 1;
        }
        _ => {}
    }
}

fn handle_data_form_key(app: &mut TuiApp, key: KeyEvent) -> TuiAction {
    // On submit button
    if app.data_on_submit {
        match key.code {
            KeyCode::Enter | KeyCode::F(5) => {
                app.data_on_submit = false;
                return TuiAction::RunDataOp;
            }
            KeyCode::Up => {
                app.data_on_submit = false;
            }
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => {
                app.data_on_submit = false;
                app.focus = TuiFocus::Sidebar;
                return TuiAction::None;
            }
            _ => {}
        }
        return TuiAction::None;
    }

    let is_text = data_current_field_is_text(app);
    let max_field = data_max_field(app);

    match key.code {
        KeyCode::Up => {
            if app.data_field_cursor > 0 {
                app.data_field_cursor -= 1;
                app.data_cursor_chars = data_field_text_len(app);
            }
        }
        KeyCode::Down => {
            if app.data_field_cursor >= max_field {
                app.data_on_submit = true;
            } else {
                app.data_field_cursor += 1;
                app.data_cursor_chars = 0;
            }
        }
        KeyCode::Left if app.data_field_cursor == 0 => {
            // On mode row: switch to Backup
            app.data_cursor = 0;
            app.data_field_cursor = app.data_field_cursor.min(data_max_field(app));
        }
        KeyCode::Right if app.data_field_cursor == 0 => {
            // On mode row: switch to Restore
            app.data_cursor = 1;
        }
        KeyCode::Left | KeyCode::Right if !is_text => {
            // On overwrite toggle or mode row: cycle
            if app.data_cursor == 1 && app.data_field_cursor == 2 {
                app.data_restore_overwrite = !app.data_restore_overwrite;
            } else if app.data_field_cursor == 0 {
                app.data_cursor = if app.data_cursor == 0 { 1 } else { 0 };
                app.data_field_cursor = app.data_field_cursor.min(data_max_field(app));
            }
        }
        KeyCode::Left if is_text => {
            app.data_cursor_chars = app.data_cursor_chars.saturating_sub(1);
        }
        KeyCode::Right if is_text => {
            let len = data_field_text_len(app);
            app.data_cursor_chars = min(app.data_cursor_chars + 1, len);
        }
        KeyCode::Home => {
            app.data_cursor_chars = 0;
        }
        KeyCode::End => {
            app.data_cursor_chars = data_field_text_len(app);
        }
        KeyCode::Backspace => {
            data_field_backspace(app);
        }
        KeyCode::Enter => {
            if app.data_field_cursor == 0 {
                // On mode row: toggle and jump to first field
                app.data_cursor = if app.data_cursor == 0 { 1 } else { 0 };
                app.data_field_cursor = 1;
                app.data_cursor_chars = 0;
            } else if !is_text {
                // Toggle
                if app.data_cursor == 1 && app.data_field_cursor == 2 {
                    app.data_restore_overwrite = !app.data_restore_overwrite;
                }
            } else {
                // Advance to next field
                if app.data_field_cursor >= max_field {
                    app.data_on_submit = true;
                } else {
                    app.data_field_cursor += 1;
                    app.data_cursor_chars = 0;
                }
            }
        }
        KeyCode::Char(' ') if !is_text => {
            if app.data_cursor == 1 && app.data_field_cursor == 2 {
                app.data_restore_overwrite = !app.data_restore_overwrite;
            }
        }
        KeyCode::F(5) => {
            return TuiAction::RunDataOp;
        }
        KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => {
            app.focus = TuiFocus::Sidebar;
            return TuiAction::None;
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if is_text {
                data_field_insert(app, ch);
            }
        }
        _ => {}
    }
    TuiAction::None
}

fn handle_form_key(app: &mut TuiApp, key: KeyEvent) -> TuiAction {
    // When cursor is on the submit line
    if app.form_on_submit {
        match key.code {
            KeyCode::Up => {
                app.form_on_submit = false;
                TuiAction::None
            }
            KeyCode::Enter | KeyCode::F(5) => {
                app.form_on_submit = false;
                match app.build_form_command() {
                    Ok(cmd) => TuiAction::Submit(cmd),
                    Err(err) => {
                        app.push_notice(err);
                        TuiAction::None
                    }
                }
            }
            KeyCode::Esc => {
                app.form_on_submit = false;
                app.clear_messages();
                app.focus = TuiFocus::Sidebar;
                app.sidebar_auto_action()
            }
            KeyCode::Tab | KeyCode::BackTab => {
                app.form_on_submit = false;
                app.focus = TuiFocus::Sidebar;
                TuiAction::None
            }
            _ => TuiAction::None,
        }
    } else {
        let is_text = app.current_field_is_text();

        match key.code {
            KeyCode::Up => {
                app.select_field(-1);
                TuiAction::None
            }
            KeyCode::Down => {
                let is_last = app
                    .current_form()
                    .map(|f| f.selected_field >= f.fields.len().saturating_sub(1))
                    .unwrap_or(true);
                if is_last {
                    app.form_on_submit = true;
                    return TuiAction::None;
                }
                app.select_field(1);
                TuiAction::None
            }
            KeyCode::Left => {
                if is_text {
                    app.field_cursor_chars = app.field_cursor_chars.saturating_sub(1);
                } else {
                    app.cycle_current_field(-1);
                }
                TuiAction::None
            }
            KeyCode::Right => {
                if is_text {
                    if let Some(field) = app.current_field() {
                        if let Some(text) = field.value.as_text() {
                            app.field_cursor_chars =
                                min(app.field_cursor_chars + 1, text.chars().count());
                        }
                    }
                } else {
                    app.cycle_current_field(1);
                }
                TuiAction::None
            }
            KeyCode::Home => {
                app.field_cursor_chars = 0;
                TuiAction::None
            }
            KeyCode::End => {
                if let Some(field) = app.current_field() {
                    if let Some(text) = field.value.as_text() {
                        app.field_cursor_chars = text.chars().count();
                    }
                }
                TuiAction::None
            }
            KeyCode::Backspace => {
                app.field_backspace();
                TuiAction::None
            }
            KeyCode::Delete => {
                app.field_delete();
                TuiAction::None
            }
            KeyCode::Enter => {
                if !is_text {
                    app.cycle_current_field(1);
                    TuiAction::None
                } else {
                    app.select_field(1);
                    // If we were on the last field, move to submit
                    let is_now_same = app
                        .current_form()
                        .map(|f| f.selected_field >= f.fields.len().saturating_sub(1))
                        .unwrap_or(true);
                    if is_now_same {
                        app.form_on_submit = true;
                    }
                    TuiAction::None
                }
            }
            KeyCode::F(5) => match app.build_form_command() {
                Ok(cmd) => TuiAction::Submit(cmd),
                Err(err) => {
                    app.push_notice(err);
                    TuiAction::None
                }
            },
            KeyCode::Esc => {
                app.clear_messages();
                app.focus = TuiFocus::Sidebar;
                app.sidebar_auto_action()
            }
            KeyCode::Tab | KeyCode::BackTab => {
                app.focus = TuiFocus::Sidebar;
                TuiAction::None
            }
            KeyCode::Char(' ') if !is_text => {
                app.cycle_current_field(1);
                TuiAction::None
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if is_text {
                    app.insert_into_field(ch);
                }
                TuiAction::None
            }
            _ => TuiAction::None,
        }
    } // else (not on submit)
}

// ---------------------------------------------------------------------------
// Wallet fetching (preserved)
// ---------------------------------------------------------------------------

fn local_wallets(state: &SessionState) -> Result<Vec<TuiWalletEntry>, String> {
    let Some(store) = &state.store else {
        return Ok(Vec::new());
    };
    store
        .list_wallet_metadata(None)
        .map(|wallets| {
            wallets
                .into_iter()
                .map(|wallet| TuiWalletEntry {
                    id: wallet.id,
                    label: wallet.label,
                    network: Some(wallet.network),
                })
                .collect()
        })
        .map_err(|error| format!("wallet refresh failed: {error}"))
}

async fn remote_wallets(
    state: &mut SessionState,
    endpoint: &str,
    secret: &str,
) -> Result<Vec<TuiWalletEntry>, String> {
    let input = Input::WalletList {
        id: state.next_id(),
        network: None,
    };
    let outputs = remote::rpc_call(endpoint, secret, &input).await;
    let mut wallets = Vec::new();
    for value in outputs {
        if value.get("code").and_then(|v| v.as_str()) == Some("error") {
            let error = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(format!("wallet refresh failed: {error}"));
        }
        if value.get("code").and_then(|v| v.as_str()) != Some("wallet_list") {
            continue;
        }
        let Some(items) = value.get("wallets").and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            wallets.push(TuiWalletEntry {
                id,
                label: item
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string()),
                network: item
                    .get("network")
                    .and_then(|v| v.as_str())
                    .and_then(network_from_str),
            });
        }
    }
    Ok(wallets)
}

async fn refresh_wallets(
    app: &mut TuiApp,
    state: &mut SessionState,
    backend: &SessionBackend,
) -> Result<(), String> {
    let wallets = match backend {
        SessionBackend::Local { .. } => local_wallets(state)?,
        SessionBackend::Remote { endpoint, secret } => {
            remote_wallets(state, endpoint, secret).await?
        }
    };
    app.set_wallets(wallets);
    Ok(())
}

fn all_wallets_flat(app: &TuiApp) -> Vec<TuiWalletEntry> {
    app.wallet_groups
        .iter()
        .flat_map(|g| g.wallets.iter().cloned())
        .collect()
}

fn spawn_pending(backend: &SessionBackend, input: Input, kind: PendingQueryKind) -> PendingQuery {
    let handle = if backend.is_local() {
        PendingQueryHandle::Local(backend.spawn_local(input))
    } else {
        PendingQueryHandle::Remote(backend.spawn_remote(input))
    };
    PendingQuery { kind, handle }
}

// ---------------------------------------------------------------------------
// Command execution (preserved)
// ---------------------------------------------------------------------------

fn command_should_refresh_wallets(command: &str) -> bool {
    let trimmed = command.trim();
    trimmed == "use"
        || trimmed.starts_with("use ")
        || trimmed.starts_with("wallet ")
        || trimmed.contains("wallet create")
        || trimmed.contains("wallet close")
}

async fn run_submitted_command(
    app: &mut TuiApp,
    terminal: &mut TuiTerminal,
    backend: &mut SessionBackend,
    state: &mut SessionState,
    line: String,
) -> bool {
    if line.trim().is_empty() {
        return false;
    }

    app.record_history(line.clone());
    app.push_message(TuiMessageKind::Command, line.clone());

    let cmd = match parse_session_command(&line, state) {
        Ok(cmd) => cmd,
        Err(error) => {
            if !error.is_empty() {
                app.push_notice(error);
            }
            return false;
        }
    };

    let mut host = TuiHost { app, terminal };
    let should_quit = backend.execute(&mut host, state, cmd).await;

    if command_should_refresh_wallets(&line) {
        let refresh_result = refresh_wallets(host.app, state, backend).await;
        if let Err(error) = refresh_result {
            host.app.push_notice(error);
        }
    }

    should_quit
}

// ---------------------------------------------------------------------------
// History and file utils (preserved)
// ---------------------------------------------------------------------------

fn load_history_entries(path: &str) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn save_history(path: &str, history: &[String]) {
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    save_history_entries(path, history);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(super) async fn run_tui_ui(runtime: InteractiveSessionRuntime) {
    let InteractiveSessionRuntime {
        frontend,
        state,
        backend,
        completer: _,
        history_path,
        intro_messages,
    } = runtime;

    let mut state = state;
    let mut backend = backend;
    let history = load_history_entries(&history_path);

    let mut stdout = io::stdout();
    if let Err(error) = enable_raw_mode() {
        let _ = writeln!(std::io::stdout(), "Failed to enable raw mode: {error}");
        return;
    }
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        let _ = writeln!(
            std::io::stdout(),
            "Failed to enter alternate screen: {error}"
        );
        return;
    }

    let backend_ui = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend_ui) {
        Ok(terminal) => terminal,
        Err(error) => {
            let mut stdout = io::stdout();
            let _ = disable_raw_mode();
            let _ = execute!(stdout, LeaveAlternateScreen);
            let _ = writeln!(std::io::stdout(), "Failed to initialize terminal: {error}");
            return;
        }
    };

    let mut app = TuiApp::new(frontend, history);
    app.data_dir = state.data_dir.clone();
    // intro_messages are not shown — status bar provides hotkey hints
    let _ = intro_messages;
    if let Err(error) = refresh_wallets(&mut app, &mut state, &backend).await {
        app.push_notice(error);
    }
    // Fetch limits for sidebar display
    {
        let input = Input::LimitList {
            id: state.next_id(),
        };
        let outputs = backend.query(&mut state, input).await;
        app.populate_limits(&outputs);
    }

    let mut should_quit = false;
    while !should_quit {
        app.sync_session(&state, &backend);
        if let Err(error) = app.draw(&mut terminal) {
            app.push_notice(format!("Draw error: {error}"));
            break;
        }

        // Check pending async query
        if let Some(pq) = &app.pending_query {
            let finished = match &pq.handle {
                PendingQueryHandle::Local(h) => h.is_finished(),
                PendingQueryHandle::Remote(h) => h.is_finished(),
            };
            if finished {
                let Some(pq) = app.pending_query.take() else {
                    unreachable!()
                };
                let outputs = match pq.handle {
                    PendingQueryHandle::Local(h) => {
                        let _ = h.await;
                        backend.try_recv_outputs()
                    }
                    PendingQueryHandle::Remote(h) => h.await.unwrap_or_default(),
                };
                match pq.kind {
                    PendingQueryKind::WalletDetail(wid) => {
                        let wallets = all_wallets_flat(&app);
                        app.populate_wallet_detail(&wid, &wallets, &outputs);
                    }
                    PendingQueryKind::GroupSummary(net) => {
                        app.populate_group_summary(net, &outputs);
                    }
                    PendingQueryKind::History => app.populate_history(&outputs),
                    PendingQueryKind::HistoryDetail => {
                        let text = outputs
                            .iter()
                            .find_map(|v| {
                                if v.get("code").and_then(|c| c.as_str()) == Some("history_status")
                                {
                                    Some(serde_json::to_string_pretty(v).unwrap_or_default())
                                } else if v.get("code").and_then(|c| c.as_str()) == Some("error") {
                                    Some(
                                        v.get("error")
                                            .and_then(|e| e.as_str())
                                            .unwrap_or("unknown error")
                                            .to_string(),
                                    )
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_else(|| "No detail available".to_string());
                        app.history_detail_text = Some(text);
                    }
                    PendingQueryKind::Limits => app.populate_limits(&outputs),
                }
            }
        }

        // Check pending data operation (backup / restore)
        if let Some(dp) = &app.data_pending {
            if dp.handle.is_finished() {
                let Some(dp) = app.data_pending.take() else {
                    unreachable!()
                };
                let result = dp
                    .handle
                    .await
                    .unwrap_or_else(|e| Err(format!("task panicked: {e}")));
                if dp.is_backup {
                    app.data_backup_status = match result {
                        Ok(path) => DataOpStatus::Done(path),
                        Err(e) => DataOpStatus::Error(e),
                    };
                } else {
                    app.data_restore_status = match result {
                        Ok(_) => DataOpStatus::Done("restored".to_string()),
                        Err(e) => DataOpStatus::Error(e),
                    };
                }
            }
        }

        // Cursor visibility
        let show_cursor = (app.focus == TuiFocus::Main
            && app.current_form().is_some()
            && app.current_field_is_text())
            || (app.view == TuiView::DataView
                && app.focus == TuiFocus::Main
                && !app.data_on_submit
                && data_current_field_is_text(&app));
        if show_cursor {
            let _ = terminal.show_cursor();
        } else {
            let _ = terminal.hide_cursor();
        }

        let polled = event::poll(Duration::from_millis(200)).unwrap_or(false);
        if !polled {
            continue;
        }

        match event::read() {
            Ok(Event::Key(key)) => match handle_tui_key(&mut app, key) {
                TuiAction::None => {}
                TuiAction::Quit => should_quit = true,
                TuiAction::RefreshWallets => {
                    if let Err(error) = refresh_wallets(&mut app, &mut state, &backend).await {
                        app.push_notice(error);
                    }
                }
                TuiAction::SelectWallet(wallet_id) => {
                    // Cancel any pending query
                    if app.pending_query.is_some() {
                        app.pending_query = None;
                        backend.try_recv_outputs();
                    }
                    // Show loading state immediately
                    app.wallet_data = WalletViewData::empty();
                    app.wallet_data.wallet_id = Some(wallet_id.clone());
                    if let Some(entry) = all_wallets_flat(&app).iter().find(|w| w.id == wallet_id) {
                        app.wallet_data.network = entry.network.map(|n| n.to_string());
                        app.wallet_data.label = entry.label.clone();
                    }
                    app.view = TuiView::WalletDetail;

                    let input = Input::Balance {
                        id: state.next_id(),
                        wallet: Some(wallet_id.clone()),
                        network: None,
                        check: false,
                    };
                    app.pending_query = Some(spawn_pending(
                        &backend,
                        input,
                        PendingQueryKind::WalletDetail(wallet_id),
                    ));
                }
                TuiAction::SelectGroup(network) => {
                    if app.pending_query.is_some() {
                        app.pending_query = None;
                        backend.try_recv_outputs();
                    }
                    app.group_summary = None;
                    app.view = TuiView::GroupSummary;

                    let input = Input::Balance {
                        id: state.next_id(),
                        wallet: None,
                        network: Some(network),
                        check: false,
                    };
                    app.pending_query = Some(spawn_pending(
                        &backend,
                        input,
                        PendingQueryKind::GroupSummary(network),
                    ));
                }
                TuiAction::ShowLimitDetail(li) => {
                    app.selected_limit = li;
                    app.view = TuiView::LimitDetail;
                }
                TuiAction::ShowGlobalConfig => {
                    app.global_config_form = make_global_config_form();
                    app.view = TuiView::GlobalConfig;
                    app.form_on_submit = false;
                }
                TuiAction::ShowDataView => {
                    app.view = TuiView::DataView;
                    app.data_on_submit = false;
                    app.data_field_cursor = 0; // start on mode-choice row
                    app.data_cursor_chars = 0;
                }
                TuiAction::RunDataOp => {
                    if app.data_pending.is_some() {
                        app.push_notice("operation already running".to_string());
                    } else if app.data_cursor == 0 {
                        // Global backup
                        app.data_backup_status = DataOpStatus::Running;
                        let data_dir = app.data_dir.clone();
                        let output_path = if app.data_backup_output.is_empty() {
                            None
                        } else {
                            Some(app.data_backup_output.clone())
                        };
                        let handle = tokio::task::spawn_blocking(move || {
                            let stamp = crate::mode::data::utc_stamp();
                            let archive_path = output_path
                                .unwrap_or_else(|| format!("./afpay-global-{stamp}.tar.zst"));
                            crate::mode::data::do_global_backup(
                                &data_dir,
                                &archive_path,
                                &stamp,
                                &[],
                            )
                            .map(|_| archive_path)
                        });
                        app.data_pending = Some(DataPending {
                            is_backup: true,
                            handle,
                        });
                    } else {
                        // Global restore
                        if app.data_restore_archive.is_empty() {
                            app.push_notice("archive path is required".to_string());
                        } else {
                            app.data_restore_status = DataOpStatus::Running;
                            let data_dir = app.data_dir.clone();
                            let archive_path = app.data_restore_archive.clone();
                            let overwrite = app.data_restore_overwrite;
                            let pg_url = if app.data_restore_pg_url.is_empty() {
                                None
                            } else {
                                Some(app.data_restore_pg_url.clone())
                            };
                            let handle = tokio::task::spawn_blocking(move || {
                                crate::mode::data::do_global_restore(
                                    &data_dir,
                                    &archive_path,
                                    overwrite,
                                    pg_url.as_deref(),
                                    &[],
                                )
                                .map(|_| archive_path)
                            });
                            app.data_pending = Some(DataPending {
                                is_backup: false,
                                handle,
                            });
                        }
                    }
                }
                TuiAction::FetchHistory { wallet, network } => {
                    if app.pending_query.is_some() {
                        app.pending_query = None;
                        backend.try_recv_outputs();
                    }
                    app.history_records.clear();
                    app.selected_history = 0;

                    let input = Input::HistoryList {
                        id: state.next_id(),
                        wallet,
                        network,
                        limit: Some(50),
                        offset: None,
                        onchain_memo: None,
                        since_epoch_s: None,
                        until_epoch_s: None,
                    };
                    app.pending_query =
                        Some(spawn_pending(&backend, input, PendingQueryKind::History));
                }
                TuiAction::FetchHistoryDetail(transaction_id) => {
                    if app.pending_query.is_some() {
                        app.pending_query = None;
                        backend.try_recv_outputs();
                    }
                    app.history_detail_text = None;
                    app.view = TuiView::HistoryDetail;

                    let input = Input::HistoryStatus {
                        id: state.next_id(),
                        transaction_id,
                    };
                    app.pending_query = Some(spawn_pending(
                        &backend,
                        input,
                        PendingQueryKind::HistoryDetail,
                    ));
                }
                TuiAction::FetchLimits => {
                    if app.pending_query.is_some() {
                        app.pending_query = None;
                        backend.try_recv_outputs();
                    }
                    app.view = TuiView::Limits;

                    let input = Input::LimitList {
                        id: state.next_id(),
                    };
                    app.pending_query =
                        Some(spawn_pending(&backend, input, PendingQueryKind::Limits));
                }
                TuiAction::Submit(line) => {
                    // For form submissions, show output in current view
                    if app.current_form().is_some() {
                        app.clear_messages();
                    } else if !matches!(app.view, TuiView::CommandResult) {
                        app.clear_messages();
                        app.view = TuiView::CommandResult;
                    }
                    should_quit = run_submitted_command(
                        &mut app,
                        &mut terminal,
                        &mut backend,
                        &mut state,
                        line.clone(),
                    )
                    .await;

                    // After limit commands, refresh limits
                    if line.starts_with("limit ")
                        || line.contains("limit add")
                        || line.contains("limit remove")
                    {
                        let input = Input::LimitList {
                            id: state.next_id(),
                        };
                        let outputs = backend.query(&mut state, input).await;
                        app.populate_limits(&outputs);
                    }
                }
            },
            Ok(Event::Resize(_, _)) => {}
            Ok(_) => {}
            Err(error) => {
                app.push_notice(format!("Read error: {error}"));
                should_quit = true;
            }
        }
    }

    let _ = std::fs::create_dir_all(&state.data_dir);
    save_history(&history_path, &app.history);
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    let _ = writeln!(std::io::stdout(), "Goodbye.");
}

// ---------------------------------------------------------------------------
// Tests (adapted - command building is now in separate functions)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Set a text field by label.
    fn set_field(form: &mut TuiFormConfig, label: &str, value: &str) {
        let field = form
            .fields
            .iter_mut()
            .find(|f| f.label == label)
            .unwrap_or_else(|| panic!("no field with label '{label}'"));
        match &mut field.value {
            TuiFieldValue::Text(t) => *t = value.to_string(),
            TuiFieldValue::Toggle(v) => *v = value == "true",
            TuiFieldValue::Choice { options, selected } => {
                *selected = options
                    .iter()
                    .position(|o| *o == value)
                    .unwrap_or_else(|| panic!("no choice option '{value}' in field '{label}'"));
            }
        }
    }

    /// Verify a built command contains expected fragments.
    fn assert_command_contains(command: &str, expected_fragments: &[&str]) {
        for frag in expected_fragments {
            assert!(
                command.contains(frag),
                "command should contain '{frag}': {command}"
            );
        }
    }

    #[test]
    fn send_form_ln_builds_command() {
        let mut form = make_send_form("ln", None);
        set_field(&mut form, "to", "lnbc123");
        set_field(&mut form, "onchain memo", "coffee beans");

        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("ln send"));
        assert_command_contains(&command, &["--to", "lnbc123"]);
    }

    #[test]
    fn send_form_cashu_p2p_builds_command() {
        let mut form = make_send_form("cashu", None);
        set_field(&mut form, "amount sats", "500");

        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("cashu send"));
        assert_command_contains(&command, &["--amount-sats", "500"]);
    }

    #[test]
    fn send_form_sol_builds_command() {
        let mut form = make_send_form("sol", None);
        set_field(
            &mut form,
            "to",
            "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
        );
        set_field(&mut form, "amount", "1000");
        set_field(&mut form, "token", "native");

        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("sol send"));
        assert_command_contains(&command, &["--to", "--amount", "1000", "--token", "native"]);
    }

    #[test]
    fn receive_form_cashu_invoice_builds_command() {
        // Variant 1 = "LN invoice" → cashu receive-from-ln
        let mut form = make_receive_form("cashu", None);
        form.variant_index = 1;
        form.fields = build_form_fields(&form.variants, 1, None);
        set_field(&mut form, "amount sats", "500");
        set_field(&mut form, "wait", "true");
        set_field(&mut form, "wait timeout s", "30");
        set_field(&mut form, "qr svg file", "true");

        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("cashu receive-from-ln"));
        assert_command_contains(
            &command,
            &[
                "--amount-sats",
                "500",
                "--wait",
                "--wait-timeout-s",
                "30",
                "--qr-svg-file",
            ],
        );
    }

    #[test]
    fn receive_form_cashu_token_path() {
        // Variant 0 = "Claim token" → cashu receive <token>
        let form = make_receive_form("cashu", None);
        // Default variant 0 has positional token arg
        assert_eq!(form.variant_index, 0);
        assert_command_contains(&form.variants[0].path.join(" "), &["cashu", "receive"]);
    }

    #[test]
    fn receive_form_ln_builds_command() {
        let mut form = make_receive_form("ln", None);
        set_field(&mut form, "amount sats", "1000");

        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("ln receive"));
        assert_command_contains(&command, &["--amount-sats", "1000"]);
    }

    #[test]
    fn send_form_fields_match_clap_args() {
        for net in NETWORK_OPTIONS {
            let form = make_send_form(net, None);
            // Check each variant's fields match clap args
            for (vi, variant) in form.variants.iter().enumerate() {
                let fields = build_form_fields(&form.variants, vi, None);
                let args = crate::args::subcommand_args(&variant.path);
                // Skip the variant choice field (index 0 when variants > 1)
                let skip = if form.variants.len() > 1 { 1 } else { 0 };
                let form_labels: Vec<&str> = fields.iter().skip(skip).map(|f| f.label).collect();
                let arg_labels: Vec<String> = args
                    .iter()
                    .filter(|a| a.long != "wallet")
                    .map(|a| a.long.replace('-', " "))
                    .collect();
                assert_eq!(
                    form_labels, arg_labels,
                    "send form for {net} variant '{}' should match clap args",
                    variant.label
                );
            }
        }
    }

    #[test]
    fn receive_form_fields_match_clap_args() {
        for net in NETWORK_OPTIONS {
            let form = make_receive_form(net, None);
            for (vi, variant) in form.variants.iter().enumerate() {
                let fields = build_form_fields(&form.variants, vi, None);
                let args = crate::args::subcommand_args(&variant.path);
                let skip = if form.variants.len() > 1 { 1 } else { 0 };
                let form_labels: Vec<&str> = fields.iter().skip(skip).map(|f| f.label).collect();
                let arg_labels: Vec<String> = args
                    .iter()
                    .filter(|a| a.long != "wallet")
                    .map(|a| a.long.replace('-', " "))
                    .collect();
                assert_eq!(
                    form_labels, arg_labels,
                    "receive form for {net} variant '{}' should match clap args",
                    variant.label
                );
            }
        }
    }

    #[test]
    fn wallet_groups_sort_and_filter() {
        let wallets = vec![
            TuiWalletEntry {
                id: "w2".to_string(),
                label: Some("beta".to_string()),
                network: Some(Network::Cashu),
            },
            TuiWalletEntry {
                id: "w1".to_string(),
                label: Some("alpha".to_string()),
                network: Some(Network::Cashu),
            },
            TuiWalletEntry {
                id: "w3".to_string(),
                label: None,
                network: Some(Network::Ln),
            },
        ];
        let groups = build_wallet_groups(wallets);
        // All 5 network groups are always present
        assert_eq!(groups.len(), 5);
        assert_eq!(groups[0].network, Network::Cashu);
        assert_eq!(groups[0].wallets.len(), 2);
        assert_eq!(groups[0].wallets[0].label.as_deref(), Some("alpha"));
        assert_eq!(groups[0].wallets[1].label.as_deref(), Some("beta"));
        assert_eq!(groups[1].network, Network::Ln);
        assert_eq!(groups[1].wallets.len(), 1);
        // Empty groups still present
        assert_eq!(groups[2].network, Network::Sol);
        assert_eq!(groups[2].wallets.len(), 0);
    }

    #[test]
    fn wallet_create_form_fields_match_clap_args() {
        for net in NETWORK_OPTIONS {
            let form = make_wallet_create_form(net);
            for (vi, variant) in form.variants.iter().enumerate() {
                let fields = build_form_fields(&form.variants, vi, None);
                let args = crate::args::subcommand_args(&variant.path);
                let skip = if form.variants.len() > 1 { 1 } else { 0 };
                let form_labels: Vec<&str> = fields.iter().skip(skip).map(|f| f.label).collect();
                // When keep_fields is set, only those fields (+ locked) appear
                let arg_labels: Vec<String> = if variant.keep_fields.is_some()
                    || !variant.locked_values.is_empty()
                {
                    // Form shows filtered/locked subset — just verify each form field exists in clap args
                    let all_arg_longs: Vec<&str> = args.iter().map(|a| a.long.as_str()).collect();
                    for label in &form_labels {
                        let long = label.replace(' ', "-");
                        assert!(
                            all_arg_longs.contains(&long.as_str()),
                            "form field '{label}' not found in clap args for {net} variant '{}'",
                            variant.label
                        );
                    }
                    continue;
                } else {
                    args.iter().map(|a| a.long.replace('-', " ")).collect()
                };
                assert_eq!(
                    form_labels, arg_labels,
                    "wallet create form for {net} variant '{}' should match clap args",
                    variant.label
                );
            }
        }
    }

    #[test]
    fn wallet_close_form_fields_match_clap_args() {
        for net in NETWORK_OPTIONS {
            let form = make_wallet_close_form(net, "w_test");
            for (vi, variant) in form.variants.iter().enumerate() {
                let fields = build_form_fields(&form.variants, vi, Some("w_test"));
                let args = crate::args::subcommand_args(&variant.path);
                let skip = if form.variants.len() > 1 { 1 } else { 0 };
                let form_labels: Vec<&str> = fields.iter().skip(skip).map(|f| f.label).collect();
                let arg_labels: Vec<String> =
                    args.iter().map(|a| a.long.replace('-', " ")).collect();
                assert_eq!(
                    form_labels, arg_labels,
                    "wallet close form for {net} variant '{}' should match clap args",
                    variant.label
                );
            }
        }
    }

    #[test]
    fn wallet_show_seed_form_fields_match_clap_args() {
        for net in NETWORK_OPTIONS {
            let form = make_wallet_show_seed_form(net, "w_test");
            for (vi, variant) in form.variants.iter().enumerate() {
                let fields = build_form_fields(&form.variants, vi, Some("w_test"));
                let args = crate::args::subcommand_args(&variant.path);
                let skip = if form.variants.len() > 1 { 1 } else { 0 };
                let form_labels: Vec<&str> = fields.iter().skip(skip).map(|f| f.label).collect();
                let arg_labels: Vec<String> =
                    args.iter().map(|a| a.long.replace('-', " ")).collect();
                assert_eq!(
                    form_labels, arg_labels,
                    "wallet show-seed form for {net} variant '{}' should match clap args",
                    variant.label
                );
            }
        }
    }

    #[test]
    fn wallet_create_form_cashu_builds_command() {
        let mut form = make_wallet_create_form("cashu");
        set_field(&mut form, "cashu mint", "https://mint.example");

        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("cashu wallet create"));
        assert_command_contains(&command, &["--cashu-mint", "https://mint.example"]);
    }

    #[test]
    fn wallet_close_form_builds_command() {
        let form = make_wallet_close_form("cashu", "w_abc");
        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("cashu wallet close"));
        assert_command_contains(&command, &["--wallet", "w_abc"]);
    }

    #[test]
    fn wallet_show_seed_form_builds_command() {
        let form = make_wallet_show_seed_form("sol", "w_sol1");
        let command = build_form_command_from_variant(&form).expect("should build");
        assert!(command.starts_with("sol wallet dangerously-show-seed"));
        assert_command_contains(&command, &["--wallet", "w_sol1"]);
    }

    // ═══════════════════════════════════════════
    // State machine tests (hotkeys, navigation, Esc)
    // ═══════════════════════════════════════════

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn test_app() -> TuiApp {
        let mut app = TuiApp::new(InteractiveFrontend::Tui, vec![]);
        let wallets = vec![
            TuiWalletEntry {
                id: "w_cashu1".to_string(),
                label: Some("wallet-a".to_string()),
                network: Some(Network::Cashu),
            },
            TuiWalletEntry {
                id: "w_ln1".to_string(),
                label: Some("ln-node".to_string()),
                network: Some(Network::Ln),
            },
        ];
        app.set_wallets(wallets);
        app
    }

    #[test]
    fn hotkey_c_on_group_opens_wallet_create() {
        let mut app = test_app();
        // Move to ln group (index 1)
        app.sidebar_cursor = SidebarItem::Group(1);
        app.focus = TuiFocus::Sidebar;
        let action = handle_tui_key(&mut app, key(KeyCode::Char('c')));
        assert!(matches!(action, TuiAction::None));
        assert!(matches!(app.view, TuiView::WalletCreate));
        assert_eq!(app.focus, TuiFocus::Main);
    }

    #[test]
    fn hotkey_c_on_wallet_does_nothing() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        app.focus = TuiFocus::Sidebar;
        let _action = handle_tui_key(&mut app, key(KeyCode::Char('c')));
        assert!(
            !matches!(app.view, TuiView::WalletCreate),
            "c on wallet should not open create — create is network-level only"
        );
    }

    #[test]
    fn hotkey_s_works_on_wallet() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        app.focus = TuiFocus::Sidebar;
        let action = handle_tui_key(&mut app, key(KeyCode::Char('s')));
        assert!(matches!(action, TuiAction::None));
        assert!(matches!(app.view, TuiView::Send));
    }

    #[test]
    fn hotkey_x_on_wallet_opens_close_form() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        app.focus = TuiFocus::Sidebar;
        let action = handle_tui_key(&mut app, key(KeyCode::Char('x')));
        assert!(matches!(action, TuiAction::None));
        assert!(matches!(app.view, TuiView::WalletClose));
    }

    #[test]
    fn hotkey_x_on_group_does_nothing() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(0);
        app.focus = TuiFocus::Sidebar;
        let _action = handle_tui_key(&mut app, key(KeyCode::Char('x')));
        assert!(!matches!(app.view, TuiView::WalletClose));
    }

    #[test]
    fn esc_from_wallet_create_returns_to_group() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(1); // ln group
                                                    // Open wallet create
        handle_tui_key(&mut app, key(KeyCode::Char('c')));
        assert!(matches!(app.view, TuiView::WalletCreate));
        // Esc should return to group summary
        let action = handle_tui_key(&mut app, key(KeyCode::Esc));
        assert!(
            matches!(action, TuiAction::SelectGroup(Network::Ln)),
            "Esc from wallet create on ln group should return SelectGroup(Ln), got {action:?}"
        );
    }

    #[test]
    fn esc_from_send_returns_to_wallet() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0); // cashu wallet
                                                        // Open send form
        handle_tui_key(&mut app, key(KeyCode::Char('s')));
        assert!(matches!(app.view, TuiView::Send));
        // Esc should return to wallet
        let action = handle_tui_key(&mut app, key(KeyCode::Esc));
        assert!(
            matches!(action, TuiAction::SelectWallet(ref wid) if wid == "w_cashu1"),
            "Esc from send on wallet should return SelectWallet, got {action:?}"
        );
    }

    #[test]
    fn sidebar_wallet_id_tracks_cursor() {
        let mut app = test_app();
        assert!(app.sidebar_wallet_id().is_none()); // default Group(0)
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        assert_eq!(app.sidebar_wallet_id(), Some("w_cashu1"));
        app.sidebar_cursor = SidebarItem::Group(1);
        assert!(app.sidebar_wallet_id().is_none());
    }

    #[test]
    fn form_context_from_group() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(1); // ln
        let (net, wid) = app.form_context();
        assert_eq!(net, "ln");
        assert!(wid.is_none());
    }

    #[test]
    fn form_context_from_wallet() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        let (net, wid) = app.form_context();
        assert_eq!(net, "cashu");
        assert_eq!(wid.as_deref(), Some("w_cashu1"));
    }

    // --- Bug-hunting tests ---

    #[test]
    fn hotkey_c_on_limits_does_nothing() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::LimitHeader;
        let _action = handle_tui_key(&mut app, key(KeyCode::Char('c')));
        assert!(
            !matches!(app.view, TuiView::WalletCreate),
            "c on limits should not open wallet create"
        );
    }

    #[test]
    fn send_works_on_group_with_wallets() {
        let mut app = test_app();
        // cashu group has wallets — s should work (auto-selects wallet)
        app.sidebar_cursor = SidebarItem::Group(0);
        let _action = handle_tui_key(&mut app, key(KeyCode::Char('s')));
        assert!(
            matches!(app.view, TuiView::Send),
            "s on group with wallets should open send form"
        );
    }

    #[test]
    fn send_blocked_on_empty_group() {
        let mut app = test_app();
        // sol group has no wallets — s should not trigger
        app.sidebar_cursor = SidebarItem::Group(2);
        let _action = handle_tui_key(&mut app, key(KeyCode::Char('s')));
        assert!(
            !matches!(app.view, TuiView::Send),
            "s on empty group should not open send form"
        );
    }

    #[test]
    fn hotkey_a_on_limits_opens_limit_add_form() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::LimitHeader;
        handle_tui_key(&mut app, key(KeyCode::Char('a')));
        assert!(matches!(app.view, TuiView::LimitAdd));
        assert_eq!(app.focus, TuiFocus::Main);
        // Should have scope variant switcher
        let form = app.current_form().unwrap();
        assert!(
            form.variants.len() >= 2,
            "limit add should have scope variants"
        );
        assert_eq!(form.variant_label, "scope");
    }

    #[test]
    fn limit_add_form_builds_command() {
        let form = &mut make_limit_add_form();
        // Global scope (variant 0)
        set_field(form, "window", "24h");
        set_field(form, "max spend", "50000");
        let cmd = build_limit_add_command(form).expect("should build");
        assert!(cmd.starts_with("global limit add"), "cmd: {cmd}");
        assert_command_contains(&cmd, &["--window", "24h", "--max-spend", "50000"]);
    }

    #[test]
    fn limit_add_network_scope_builds_command() {
        let form = &mut make_limit_add_form();
        // Switch to network scope (variant 1)
        form.variant_index = 1;
        form.fields = build_form_fields_labeled(&form.variants, 1, None, "scope");
        inject_limit_add_extra_fields(&mut form.fields, 1, 0, &[]); // cashu = index 0
        set_field(form, "network", "cashu");
        set_field(form, "window", "1h");
        set_field(form, "max spend", "10000");
        let cmd = build_limit_add_command(form).expect("should build");
        assert_command_contains(
            &cmd,
            &["cashu limit add", "--window", "1h", "--max-spend", "10000"],
        );
    }

    #[test]
    fn limit_add_wallet_scope_builds_command() {
        let form = &mut make_limit_add_form();
        // Switch to wallet scope (variant 2)
        form.variant_index = 2;
        form.fields = build_form_fields_labeled(&form.variants, 2, None, "scope");
        inject_limit_add_extra_fields(&mut form.fields, 2, 2, &["w_abc".to_string()]); // sol = index 2
        set_field(form, "network", "sol");
        set_field(form, "wallet", "w_abc");
        set_field(form, "window", "30m");
        set_field(form, "max spend", "5000");
        let cmd = build_limit_add_command(form).expect("should build");
        assert_command_contains(
            &cmd,
            &[
                "sol limit --wallet w_abc add",
                "--window",
                "30m",
                "--max-spend",
                "5000",
            ],
        );
    }

    #[test]
    fn sidebar_move_through_empty_groups() {
        let mut app = test_app();
        // Start at Group(0) = cashu (has wallets)
        app.sidebar_cursor = SidebarItem::Group(0);
        app.focus = TuiFocus::Sidebar;
        // cashu is expanded, so Down goes to Wallet(0,0)
        let _action = handle_tui_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.sidebar_cursor, SidebarItem::Wallet(0, 0));
        // Down again → Wallet(0,1) (second cashu wallet? no — only 1 cashu wallet in test_app)
        // Actually test_app has w_cashu1 in cashu, w_ln1 in ln
        let _action = handle_tui_key(&mut app, key(KeyCode::Down));
        // Should go to ln group (Group(1))
        assert_eq!(
            app.sidebar_cursor,
            SidebarItem::Group(1),
            "should skip from cashu wallet to ln group"
        );
        // Down → ln wallet
        let _action = handle_tui_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.sidebar_cursor, SidebarItem::Wallet(1, 0));
        // Down → sol group (empty, index 2)
        let _action = handle_tui_key(&mut app, key(KeyCode::Down));
        assert_eq!(
            app.sidebar_cursor,
            SidebarItem::Group(2),
            "empty sol group should still be navigable"
        );
    }

    #[test]
    fn wallet_create_form_network_matches_sidebar_group() {
        let mut app = test_app();
        // Navigate to sol group (index 2, empty)
        app.sidebar_cursor = SidebarItem::Group(2);
        handle_tui_key(&mut app, key(KeyCode::Char('c')));
        // The form should be for sol, not cashu
        let _cmd = app.build_form_command();
        // Even without filling fields, path should start with sol
        let path = &app.wallet_create_form.variants[app.wallet_create_form.variant_index].path;
        assert_eq!(
            path,
            &vec!["sol", "wallet", "create"],
            "wallet create on sol group should use sol path"
        );
    }

    // ═══════════════════════════════════════════
    // Full flow simulations
    // ═══════════════════════════════════════════

    /// Helper: simulate a sequence of keys on `app`, collecting actions.
    fn simulate(app: &mut TuiApp, keys: &[KeyCode]) -> Vec<TuiAction> {
        keys.iter().map(|&k| handle_tui_key(app, key(k))).collect()
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// State snapshot for assertions.
    #[derive(Debug)]
    struct Snapshot {
        view: &'static str,
        focus: TuiFocus,
        cursor: SidebarItem,
        form_on_submit: bool,
        _messages: usize,
    }

    fn snap(app: &TuiApp) -> Snapshot {
        Snapshot {
            view: match app.view {
                TuiView::WalletDetail => "WalletDetail",
                TuiView::GroupSummary => "GroupSummary",
                TuiView::Send => "Send",
                TuiView::Receive => "Receive",
                TuiView::WalletCreate => "WalletCreate",
                TuiView::WalletClose => "WalletClose",
                TuiView::WalletShowSeed => "WalletShowSeed",
                TuiView::History => "History",
                TuiView::HistoryDetail => "HistoryDetail",
                TuiView::Limits => "Limits",
                TuiView::LimitDetail => "LimitDetail",
                TuiView::LimitAdd => "LimitAdd",
                TuiView::WalletConfig => "WalletConfig",
                TuiView::GlobalConfig => "GlobalConfig",
                TuiView::CommandResult => "CommandResult",
                TuiView::DataView => "DataView",
            },
            focus: app.focus,
            cursor: app.sidebar_cursor,
            form_on_submit: app.form_on_submit,
            _messages: app.messages.len(),
        }
    }

    #[test]
    fn flow_sidebar_navigation_full() {
        let mut app = test_app();
        let s = snap(&app);
        // Initial state: sidebar focused, first group, wallet detail view
        assert_eq!(s.focus, TuiFocus::Sidebar);
        assert_eq!(s.cursor, SidebarItem::Group(0));
        assert_eq!(s.view, "WalletDetail");

        // Down → cashu wallet (group 0 is expanded)
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Wallet(0, 0));

        // Down → ln group
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Group(1));

        // Down → ln wallet
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Wallet(1, 0));

        // Down → sol group (empty)
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Group(2));

        // Down → evm group (empty)
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Group(3));

        // Down → btc group (empty)
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Group(4));

        // Down → LimitHeader
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::LimitHeader);

        // Up goes back
        simulate(&mut app, &[KeyCode::Up]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Group(4));
    }

    #[test]
    fn flow_group_collapse_skips_wallets() {
        let mut app = test_app();
        // Collapse cashu group (Enter on group toggles)
        app.sidebar_cursor = SidebarItem::Group(0);
        simulate(&mut app, &[KeyCode::Enter]);
        assert!(!app.wallet_groups[0].expanded);

        // Down should skip to ln group, not cashu wallet
        simulate(&mut app, &[KeyCode::Down]);
        assert_eq!(app.sidebar_cursor, SidebarItem::Group(1));
    }

    #[test]
    fn flow_tab_switches_focus() {
        let mut app = test_app();
        assert_eq!(app.focus, TuiFocus::Sidebar);

        // Tab → main
        simulate(&mut app, &[KeyCode::Tab]);
        assert_eq!(app.focus, TuiFocus::Main);

        // Tab → back to sidebar
        simulate(&mut app, &[KeyCode::Tab]);
        assert_eq!(app.focus, TuiFocus::Sidebar);
    }

    #[test]
    fn flow_wallet_create_fill_and_submit() {
        let mut app = test_app();
        // Go to cashu group, press c
        app.sidebar_cursor = SidebarItem::Group(0);
        simulate(&mut app, &[KeyCode::Char('c')]);
        let s = snap(&app);
        assert_eq!(s.view, "WalletCreate");
        assert_eq!(s.focus, TuiFocus::Main);
        assert!(!s.form_on_submit);

        // Form should have cashu fields: cashu-mint (required), label, mnemonic-secret
        let form = app.current_form().unwrap();
        assert!(
            form.fields.iter().any(|f| f.label == "cashu mint"),
            "cashu wallet create should have cashu mint field, fields: {:?}",
            form.fields.iter().map(|f| f.label).collect::<Vec<_>>()
        );

        // Type into cashu-mint field (should be first or second field)
        let mint_idx = form
            .fields
            .iter()
            .position(|f| f.label == "cashu mint")
            .unwrap();
        app.current_form_mut().unwrap().selected_field = mint_idx;
        app.sync_field_cursor();
        // Type URL
        for ch in "https://mint.example".chars() {
            handle_tui_key(&mut app, key(KeyCode::Char(ch)));
        }

        // Verify typed text
        let form = app.current_form().unwrap();
        let mint_val = form.fields[mint_idx].value.as_text().unwrap();
        assert_eq!(mint_val, "https://mint.example");

        // Navigate down past all fields to submit
        for _ in 0..form.fields.len() {
            simulate(&mut app, &[KeyCode::Down]);
        }
        assert!(
            app.form_on_submit,
            "should be on submit after navigating past all fields"
        );

        // Enter → builds command
        let action = handle_tui_key(&mut app, key(KeyCode::Enter));
        match action {
            TuiAction::Submit(cmd) => {
                assert!(cmd.contains("cashu wallet create"), "cmd: {cmd}");
                assert!(cmd.contains("--cashu-mint"), "cmd: {cmd}");
                assert!(cmd.contains("https://mint.example"), "cmd: {cmd}");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn flow_wallet_create_missing_required_field_shows_error() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(0); // cashu
        simulate(&mut app, &[KeyCode::Char('c')]);

        // Don't fill any fields, go straight to submit
        let form = app.current_form().unwrap();
        for _ in 0..form.fields.len() {
            simulate(&mut app, &[KeyCode::Down]);
        }
        assert!(app.form_on_submit);

        // Enter → should show error about missing --cashu-mint
        let action = handle_tui_key(&mut app, key(KeyCode::Enter));
        match action {
            TuiAction::None => {
                // Error should be in messages
                assert!(
                    !app.messages.is_empty(),
                    "empty submit should produce error message"
                );
                let last = &app.messages.last().unwrap().text;
                assert!(
                    last.contains("cashu-mint") || last.contains("required"),
                    "error should mention required field: {last}"
                );
            }
            other => panic!("expected None (error), got {other:?}"),
        }
    }

    #[test]
    fn flow_ctrl_c_quits_from_anywhere() {
        // From sidebar
        let mut app = test_app();
        let action = handle_tui_key(&mut app, ctrl(KeyCode::Char('c')));
        assert!(matches!(action, TuiAction::Quit));

        // From form
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        simulate(&mut app, &[KeyCode::Char('s')]);
        assert_eq!(snap(&app).view, "Send");
        let action = handle_tui_key(&mut app, ctrl(KeyCode::Char('c')));
        assert!(matches!(action, TuiAction::Quit));
    }

    #[test]
    fn flow_close_form_has_wallet_locked() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0); // w_cashu1
        simulate(&mut app, &[KeyCode::Char('x')]);
        assert_eq!(snap(&app).view, "WalletClose");

        // The wallet field should be locked with the wallet ID
        let form = app.current_form().unwrap();
        let wallet_field = form.fields.iter().find(|f| f.label == "wallet");
        assert!(
            wallet_field.is_some(),
            "close form should have wallet field"
        );
        let wf = wallet_field.unwrap();
        assert!(wf.locked, "wallet field should be locked");
        assert_eq!(wf.value.as_text(), Some("w_cashu1"));

        // Typing should not change a locked field
        handle_tui_key(&mut app, key(KeyCode::Char('z')));
        let form = app.current_form().unwrap();
        let wf = form.fields.iter().find(|f| f.label == "wallet").unwrap();
        assert_eq!(
            wf.value.as_text(),
            Some("w_cashu1"),
            "locked field should not accept input"
        );
    }

    #[test]
    fn flow_show_seed_wallet_and_esc_back() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(1, 0); // ln wallet
        simulate(&mut app, &[KeyCode::Char('D')]);
        let s = snap(&app);
        assert_eq!(s.view, "WalletShowSeed");
        assert_eq!(s.focus, TuiFocus::Main);

        // Verify form path is for ln
        let form = app.current_form().unwrap();
        let path = &form.variants[form.variant_index].path;
        assert_eq!(path, &vec!["ln", "wallet", "dangerously-show-seed"]);

        // Esc → back to ln wallet
        let action = handle_tui_key(&mut app, key(KeyCode::Esc));
        assert!(
            matches!(action, TuiAction::SelectWallet(ref wid) if wid == "w_ln1"),
            "Esc should return to ln wallet, got {action:?}"
        );
    }

    #[test]
    fn flow_form_tab_returns_to_sidebar_preserving_view() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        simulate(&mut app, &[KeyCode::Char('s')]);
        assert_eq!(app.focus, TuiFocus::Main);
        assert_eq!(snap(&app).view, "Send");

        // Tab returns to sidebar but keeps Send view
        simulate(&mut app, &[KeyCode::Tab]);
        assert_eq!(app.focus, TuiFocus::Sidebar);
        assert_eq!(
            snap(&app).view,
            "Send",
            "Tab from form should keep the form view visible"
        );
    }

    #[test]
    fn flow_hotkey_c_during_form_types_c_not_opens_create() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        // Open send form
        simulate(&mut app, &[KeyCode::Char('s')]);
        assert_eq!(snap(&app).view, "Send");

        // Now typing 'c' should insert into text field, NOT open wallet create
        let form = app.current_form().unwrap();
        // Find a text field
        let text_idx = form.fields.iter().position(|f| f.value.is_text()).unwrap();
        app.current_form_mut().unwrap().selected_field = text_idx;
        app.sync_field_cursor();

        handle_tui_key(&mut app, key(KeyCode::Char('c')));
        // Should still be in Send view, not WalletCreate
        assert_eq!(
            snap(&app).view,
            "Send",
            "'c' in form text field should type, not open wallet create"
        );
        // And the field should have 'c' in it
        let form = app.current_form().unwrap();
        let text = form.fields[text_idx].value.as_text().unwrap();
        assert_eq!(text, "c");
    }

    #[test]
    fn flow_q_during_form_types_q_not_quits() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        simulate(&mut app, &[KeyCode::Char('s')]);

        let form = app.current_form().unwrap();
        let text_idx = form.fields.iter().position(|f| f.value.is_text()).unwrap();
        app.current_form_mut().unwrap().selected_field = text_idx;
        app.sync_field_cursor();

        let action = handle_tui_key(&mut app, key(KeyCode::Char('q')));
        assert!(
            !matches!(action, TuiAction::Quit),
            "'q' in form text field should type, not quit"
        );
    }

    #[test]
    fn flow_receive_on_cashu_has_variant_switcher() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0); // cashu
        simulate(&mut app, &[KeyCode::Char('r')]);
        assert_eq!(snap(&app).view, "Receive");

        let form = app.current_form().unwrap();
        // Cashu receive has 3 variants: Claim token, LN invoice, Claim LN quote
        assert!(
            form.variants.len() >= 2,
            "cashu receive should have multiple variants, got {}",
            form.variants.len()
        );

        // First field should be the variant choice
        let first = &form.fields[0];
        assert_eq!(first.label, "action");
        assert!(matches!(first.value, TuiFieldValue::Choice { .. }));

        // Cycle right should change variant
        app.current_form_mut().unwrap().selected_field = 0;
        simulate(&mut app, &[KeyCode::Right]);
        let form = app.current_form().unwrap();
        assert_eq!(
            form.variant_index, 1,
            "Right on choice field should cycle variant"
        );
    }

    #[test]
    fn flow_create_ln_backend_switcher_filters_fields() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(1); // ln
        simulate(&mut app, &[KeyCode::Char('c')]);

        let form = app.current_form().unwrap();
        // Should have backend choice field
        let first = &form.fields[0];
        assert_eq!(
            first.label, "backend",
            "ln wallet create should show 'backend' not 'action'"
        );

        // Check initial variant fields — should only have backend-specific fields
        let field_labels: Vec<&str> = form.fields.iter().map(|f| f.label).collect();

        // Backend field should be present and locked (showing current backend)
        let backend_field = form.fields.iter().find(|f| f.label == "backend").unwrap();
        match &backend_field.value {
            TuiFieldValue::Choice { options, .. } => {
                // All options should be valid LN backends
                for opt in options.iter() {
                    assert!(
                        ["nwc", "phoenixd", "lnbits"].contains(opt),
                        "unexpected ln backend option: {opt}"
                    );
                }
            }
            _ => panic!("backend field should be Choice, got text/toggle"),
        }

        // Should NOT have fields from other backends mixed in
        // (e.g., nwc variant should not show "endpoint")
        let initial_backend = form.variants[form.variant_index].label;
        if initial_backend == "nwc" {
            assert!(
                !field_labels.contains(&"endpoint"),
                "nwc variant should not show endpoint field"
            );
            assert!(
                !field_labels.contains(&"password secret"),
                "nwc variant should not show password secret field"
            );
        } else if initial_backend == "phoenixd" {
            assert!(
                !field_labels.contains(&"nwc uri secret"),
                "phoenixd variant should not show nwc uri secret field"
            );
            assert!(
                !field_labels.contains(&"admin key secret"),
                "phoenixd variant should not show admin key secret field"
            );
        }
    }

    #[test]
    fn flow_backspace_in_form_field() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(0);
        simulate(&mut app, &[KeyCode::Char('c')]);

        let form = app.current_form().unwrap();
        let mint_idx = form
            .fields
            .iter()
            .position(|f| f.label == "cashu mint")
            .unwrap();
        app.current_form_mut().unwrap().selected_field = mint_idx;
        app.sync_field_cursor();

        // Type "abc"
        for ch in "abc".chars() {
            handle_tui_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(
            app.current_form().unwrap().fields[mint_idx].value.as_text(),
            Some("abc")
        );

        // Backspace removes last char
        handle_tui_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(
            app.current_form().unwrap().fields[mint_idx].value.as_text(),
            Some("ab")
        );

        // Two more backspaces → empty
        handle_tui_key(&mut app, key(KeyCode::Backspace));
        handle_tui_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(
            app.current_form().unwrap().fields[mint_idx].value.as_text(),
            Some("")
        );

        // Backspace on empty is safe
        handle_tui_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(
            app.current_form().unwrap().fields[mint_idx].value.as_text(),
            Some("")
        );
    }

    #[test]
    fn group_summary_empty_shows_no_total() {
        let mut app = test_app();

        // Empty group: wallet_count should be 0
        app.populate_group_summary(Network::Evm, &[]);
        let summary = app.group_summary.as_ref().unwrap();
        assert_eq!(summary.wallet_count, 0);
        // render_group_summary will show "No wallets" hint instead of Total/Wallets lines
        // (verified by the render logic branching on wallet_count == 0)
    }

    #[test]
    fn group_summary_default_unit_matches_network() {
        let mut app = test_app();

        app.populate_group_summary(Network::Evm, &[]);
        assert_eq!(app.group_summary.as_ref().unwrap().unit, "wei");

        app.populate_group_summary(Network::Sol, &[]);
        assert_eq!(app.group_summary.as_ref().unwrap().unit, "lamports");

        app.populate_group_summary(Network::Cashu, &[]);
        assert_eq!(app.group_summary.as_ref().unwrap().unit, "sat");

        app.populate_group_summary(Network::Ln, &[]);
        assert_eq!(app.group_summary.as_ref().unwrap().unit, "sat");

        app.populate_group_summary(Network::Btc, &[]);
        assert_eq!(app.group_summary.as_ref().unwrap().unit, "sat");
    }

    #[test]
    fn flow_history_on_wallet() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0); // cashu wallet
        let action = handle_tui_key(&mut app, key(KeyCode::Char('h')));
        match action {
            TuiAction::FetchHistory { wallet, network } => {
                assert_eq!(wallet.as_deref(), Some("w_cashu1"));
                assert!(
                    network.is_none(),
                    "wallet-level history should not set network"
                );
            }
            other => panic!("expected FetchHistory, got {other:?}"),
        }
        assert!(matches!(app.view, TuiView::History));
    }

    #[test]
    fn flow_history_on_group() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(0); // cashu group (has wallets)
        let action = handle_tui_key(&mut app, key(KeyCode::Char('h')));
        match action {
            TuiAction::FetchHistory { wallet, network } => {
                assert!(
                    wallet.is_none(),
                    "group-level history should not set wallet"
                );
                assert_eq!(network, Some(Network::Cashu));
            }
            other => panic!("expected FetchHistory, got {other:?}"),
        }
        assert!(matches!(app.view, TuiView::History));
    }

    #[test]
    fn flow_history_blocked_on_empty_group() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(2); // sol (empty)
        let _action = handle_tui_key(&mut app, key(KeyCode::Char('h')));
        assert!(
            !matches!(app.view, TuiView::History),
            "h on empty group should not open history"
        );
    }

    #[test]
    fn flow_multiple_esc_does_not_panic() {
        let mut app = test_app();
        // Esc from initial state
        let action = handle_tui_key(&mut app, key(KeyCode::Esc));
        assert!(matches!(action, TuiAction::None) || matches!(action, TuiAction::SelectGroup(_)));

        // Esc again
        let action = handle_tui_key(&mut app, key(KeyCode::Esc));
        assert!(!matches!(action, TuiAction::Quit), "Esc should not quit");
    }

    // ═══════════════════════════════════════════
    // Screen state audit
    // ═══════════════════════════════════════════

    /// Check the status bar cursor label for a given sidebar position.
    fn status_cursor_label(app: &TuiApp) -> String {
        match app.sidebar_cursor {
            SidebarItem::Wallet(gi, wi) => {
                let group = &app.wallet_groups[gi];
                let wallet = &group.wallets[wi];
                let net = group.network.to_string().to_lowercase();
                format!("{net}/{}", wallet.display_short())
            }
            SidebarItem::Group(gi) => app
                .wallet_groups
                .get(gi)
                .map(|g| g.network.to_string().to_lowercase())
                .unwrap_or_default(),
            SidebarItem::LimitHeader => "limits".to_string(),
            SidebarItem::Limit(li) => app
                .limit_records
                .get(li)
                .map(|r| r.rule_id.clone())
                .unwrap_or_else(|| "limit".to_string()),
            SidebarItem::ConfigHeader => "config".to_string(),
            SidebarItem::DataHeader => "data".to_string(),
        }
    }

    #[test]
    fn screen_initial_state() {
        let app = test_app();
        // Sidebar: focused, cursor on first group
        assert_eq!(app.focus, TuiFocus::Sidebar);
        assert_eq!(app.sidebar_cursor, SidebarItem::Group(0));
        // Right pane: WalletDetail with no data (event loop would process SelectGroup)
        assert!(matches!(app.view, TuiView::WalletDetail));
        assert!(
            app.wallet_data.wallet_id.is_none(),
            "initial state should have no wallet selected"
        );
        // Status bar: shows first network name
        assert_eq!(status_cursor_label(&app), "cashu");
        // No messages, no command input, no modal
        assert!(app.messages.is_empty());
        assert!(app.modal.is_none());
    }

    #[test]
    fn screen_sidebar_actions_for_every_position() {
        let mut app = test_app();
        // Collect all sidebar items and their expected actions
        let items = app.sidebar_items();
        for item in &items {
            app.sidebar_cursor = *item;
            let action = app.sidebar_auto_action();
            match item {
                SidebarItem::Group(gi) => {
                    let net = app.wallet_groups[*gi].network;
                    assert!(
                        matches!(action, TuiAction::SelectGroup(n) if n == net),
                        "Group({gi}) should return SelectGroup({net:?}), got {action:?}"
                    );
                }
                SidebarItem::Wallet(gi, wi) => {
                    let wid = &app.wallet_groups[*gi].wallets[*wi].id;
                    assert!(
                        matches!(&action, TuiAction::SelectWallet(id) if id == wid),
                        "Wallet({gi},{wi}) should return SelectWallet({wid}), got {action:?}"
                    );
                }
                SidebarItem::LimitHeader => {
                    assert!(
                        matches!(action, TuiAction::FetchLimits),
                        "LimitHeader should return FetchLimits, got {action:?}"
                    );
                }
                SidebarItem::Limit(li) => {
                    assert!(
                        matches!(action, TuiAction::ShowLimitDetail(i) if i == *li),
                        "Limit({li}) should return ShowLimitDetail, got {action:?}"
                    );
                }
                SidebarItem::ConfigHeader => {
                    assert!(
                        matches!(action, TuiAction::ShowGlobalConfig),
                        "ConfigHeader should return ShowGlobalConfig, got {action:?}"
                    );
                }
                SidebarItem::DataHeader => {
                    assert!(
                        matches!(action, TuiAction::ShowDataView),
                        "DataHeader should return ShowDataView, got {action:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn screen_status_bar_label_every_position() {
        let mut app = test_app();
        let items = app.sidebar_items();
        for item in &items {
            app.sidebar_cursor = *item;
            let label = status_cursor_label(&app);
            assert!(
                !label.is_empty(),
                "status bar label should not be empty for {item:?}"
            );
            // Specific checks
            match item {
                SidebarItem::Group(0) => assert_eq!(label, "cashu"),
                SidebarItem::Group(1) => assert_eq!(label, "ln"),
                SidebarItem::Wallet(0, 0) => assert_eq!(label, "cashu/wallet-a"),
                SidebarItem::Wallet(1, 0) => assert_eq!(label, "ln/ln-node"),
                SidebarItem::LimitHeader => assert_eq!(label, "limits"),
                _ => {} // other groups: sol, evm, btc
            }
        }
    }

    #[test]
    fn screen_wallet_detail_title_uses_label() {
        let mut app = test_app();
        // Simulate what event loop does for SelectWallet
        app.wallet_data = WalletViewData::empty();
        app.wallet_data.wallet_id = Some("w_cashu1".to_string());
        app.wallet_data.label = Some("wallet-a".to_string());
        app.wallet_data.network = Some("cashu".to_string());
        // Title should use label, not wallet ID
        let title = app
            .wallet_data
            .label
            .clone()
            .or_else(|| app.wallet_data.wallet_id.clone())
            .unwrap_or_else(|| "Wallet".to_string());
        assert_eq!(title, "wallet-a");
    }

    #[test]
    fn screen_form_title_reflects_sidebar_context() {
        let mut app = test_app();

        // On cashu wallet → form title shows wallet label
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);
        simulate(&mut app, &[KeyCode::Char('s')]);
        let (_, ctx_wallet) = app.form_context();
        assert_eq!(
            ctx_wallet.as_deref(),
            Some("w_cashu1"),
            "form title context should show wallet id"
        );

        // Esc, then on ln group → form title shows network
        app.sidebar_cursor = SidebarItem::Group(1);
        app.view = TuiView::WalletDetail; // reset
        simulate(&mut app, &[KeyCode::Char('c')]);
        let (ctx_net, ctx_wallet) = app.form_context();
        assert_eq!(ctx_net, "ln");
        assert!(
            ctx_wallet.is_none(),
            "form title on group should show network, not wallet"
        );
    }

    #[test]
    fn screen_stale_wallet_data_after_switching_to_group() {
        let mut app = test_app();

        // Populate wallet detail for cashu wallet
        app.wallet_data.wallet_id = Some("w_cashu1".to_string());
        app.wallet_data.label = Some("wallet-a".to_string());
        app.wallet_data.balance_text = Some("100 sats".to_string());

        // Now move sidebar to ln group
        app.sidebar_cursor = SidebarItem::Group(1);
        let action = app.sidebar_auto_action();
        // Event loop would set view = GroupSummary, but wallet_data is NOT cleared
        assert!(matches!(action, TuiAction::SelectGroup(Network::Ln)));

        // This is fine because GroupSummary view doesn't render wallet_data.
        // But if we then press Esc from a form (which sets view based on sidebar),
        // we get GroupSummary, not stale WalletDetail. Verify:
        simulate(&mut app, &[KeyCode::Char('c')]); // open create form
        let esc_action = handle_tui_key(&mut app, key(KeyCode::Esc));
        assert!(
            matches!(esc_action, TuiAction::SelectGroup(Network::Ln)),
            "Esc should return to ln group summary, not stale wallet detail"
        );
    }

    #[test]
    fn screen_history_cleared_on_switch() {
        // This test documents the expected fix: clear history when opening
        let mut app = test_app();
        app.history_records.push(HistoryDisplayRecord {
            transaction_id: "tx_test1".to_string(),
            wallet: Some("w_cashu1".to_string()),
            direction: "\u{2191}".to_string(),
            amount: "100 sats".to_string(),
            status: "confirmed".to_string(),
            date: "2026-01-01".to_string(),
            memo: None,
            local_memo: None,
        });

        app.sidebar_cursor = SidebarItem::Wallet(1, 0);
        handle_tui_key(&mut app, key(KeyCode::Char('h')));

        // After fix, history should be cleared immediately
        // (event loop will populate with fresh data)
        assert!(
            app.history_records.is_empty(),
            "history should be cleared when switching to a new wallet's history"
        );
    }

    #[test]
    fn screen_form_fields_reset_on_reopen() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0);

        // Open send, type into a field
        simulate(&mut app, &[KeyCode::Char('s')]);
        let form = app.current_form().unwrap();
        let text_idx = form
            .fields
            .iter()
            .position(|f| f.value.is_text() && !f.locked)
            .unwrap();
        app.current_form_mut().unwrap().selected_field = text_idx;
        app.sync_field_cursor();
        for ch in "test123".chars() {
            handle_tui_key(&mut app, key(KeyCode::Char(ch)));
        }
        // Verify typed
        let val = app.current_form().unwrap().fields[text_idx]
            .value
            .as_text()
            .unwrap();
        assert_eq!(val, "test123");

        // Esc back
        handle_tui_key(&mut app, key(KeyCode::Esc));

        // Open send again — should be fresh form, not stale
        simulate(&mut app, &[KeyCode::Char('s')]);
        let form = app.current_form().unwrap();
        let val = form.fields[text_idx].value.as_text().unwrap();
        assert!(
            val.is_empty(),
            "reopening send form should have clean fields, got '{val}'"
        );
    }

    /// Compute which single-char hotkeys are active for the current app state.
    fn active_hotkeys(app: &mut TuiApp) -> Vec<char> {
        let mut keys = Vec::new();
        for ch in ['c', 's', 'r', 'h', 'x', 'D', 'e', 'a', 'd', 'q', 'R'] {
            let mut clone = test_app();
            clone.sidebar_cursor = app.sidebar_cursor;
            clone.wallet_groups = app.wallet_groups.clone();
            clone.view = app.view;
            clone.focus = app.focus;
            clone.limit_records = app.limit_records.clone();
            clone.selected_limit = app.selected_limit;
            let before_view = clone.view;
            let action = handle_tui_key(&mut clone, key(KeyCode::Char(ch)));
            let did_something = !matches!(action, TuiAction::None) || clone.view != before_view;
            if did_something {
                keys.push(ch);
            }
        }
        keys
    }

    /// Compute which single-char hints the status bar would show.
    fn status_bar_hints(app: &TuiApp) -> Vec<char> {
        let on_wallet = matches!(app.sidebar_cursor, SidebarItem::Wallet(_, _));
        let on_group_with_wallets = match app.sidebar_cursor {
            SidebarItem::Group(gi) => app
                .wallet_groups
                .get(gi)
                .is_some_and(|g| !g.wallets.is_empty()),
            _ => false,
        };

        let mut hints = Vec::new();
        if app.current_form().is_some() {
            // form hints don't use single chars
        } else if on_wallet {
            hints.extend(['s', 'r', 'h', 'x', 'D', 'e']);
        } else if on_group_with_wallets {
            hints.extend(['c', 's', 'r', 'h']);
        } else if matches!(app.sidebar_cursor, SidebarItem::LimitHeader) {
            hints.push('a');
        } else if matches!(app.sidebar_cursor, SidebarItem::Limit(_)) {
            hints.push('d');
        } else if matches!(app.sidebar_cursor, SidebarItem::ConfigHeader) {
            // Config header: no special hotkeys
        } else if matches!(app.sidebar_cursor, SidebarItem::DataHeader) {
            // Data header: Tab to edit (not a single-char hotkey)
        } else {
            hints.push('c');
        }
        hints.extend(['R', 'q']);
        hints
    }

    #[test]
    fn screen_hints_match_hotkeys_at_every_position() {
        let mut app = test_app();
        let items = app.sidebar_items();
        for item in items {
            app.sidebar_cursor = item;
            app.view = TuiView::WalletDetail;
            app.focus = TuiFocus::Sidebar;
            let hints = status_bar_hints(&app);
            let hotkeys = active_hotkeys(&mut app);
            // Every hint should be an active hotkey
            for h in &hints {
                assert!(
                    hotkeys.contains(h),
                    "hint '{h}' shown for {item:?} but hotkey doesn't fire. \
                     hints={hints:?}, hotkeys={hotkeys:?}"
                );
            }
            // Every active hotkey (except global ones) should have a hint
            let global = ['q', 'R', ':'];
            for k in &hotkeys {
                if global.contains(k) {
                    continue;
                }
                assert!(
                    hints.contains(k),
                    "hotkey '{k}' fires for {item:?} but no hint shown. \
                     hints={hints:?}, hotkeys={hotkeys:?}"
                );
            }
        }
    }

    #[test]
    fn screen_send_on_group_omits_wallet_field() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Group(0); // cashu group
        simulate(&mut app, &[KeyCode::Char('s')]);

        let form = app.current_form().unwrap();
        let has_wallet_field = form.fields.iter().any(|f| f.label == "wallet");
        assert!(
            !has_wallet_field,
            "send form from group should omit wallet field (auto-selected by backend)"
        );
    }

    #[test]
    fn screen_send_on_wallet_has_locked_wallet_field() {
        let mut app = test_app();
        app.sidebar_cursor = SidebarItem::Wallet(0, 0); // cashu wallet
        simulate(&mut app, &[KeyCode::Char('s')]);

        let form = app.current_form().unwrap();
        let wallet_field = form.fields.iter().find(|f| f.label == "wallet");
        assert!(
            wallet_field.is_some(),
            "send form from wallet should have wallet field"
        );
        let wf = wallet_field.unwrap();
        assert!(wf.locked, "wallet field should be locked");
        assert_eq!(wf.value.as_text(), Some("w_cashu1"));
    }
}
