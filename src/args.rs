//! afpay's closed-world `cli-spec-v1` registry.
//!
//! One registry is the single source for argv parsing, typed invocation values,
//! which argument combinations are legal, output contracts, `--help`, and the
//! generated `docs/cli.md`. Every runtime "flag X requires flag Y" rule that
//! used to live in this file is now a registered combination instead, so the
//! parser rejects the illegal mix before any payment code runs.

#[cfg(feature = "rest")]
use crate::mode::rest::RestInit;
#[cfg(feature = "rpc")]
use crate::mode::rpc::RpcInit;
use crate::types::*;
use agent_first_data::{
    ArgSpec, ArgSyntax, ArgValueType, BoundOutcome, BuiltCliSpec, CliSpec, CliSpecError, CliValue,
    Combination, CommandSpec, OutputFormat, OutputSpec, ResolvedInvocation, build_afdata_cli,
    cli_help_event, cli_parse_output, cli_version_event, render_cli_reference,
};
// The unbound registry still resolves to `CliOutcome`; only the parts of this
// file that reuse it without a handler binding need the name.
#[cfg(any(feature = "interactive", test))]
use agent_first_data::CliOutcome;
use std::collections::BTreeMap;
use std::sync::OnceLock;

// ═══════════════════════════════════════════
// Mode Dispatch Types
// ═══════════════════════════════════════════

/// A rejected invocation, already classified under the closed CLI error
/// taxonomy (`cli_unknown_argument`, `cli_invalid_argument_value`, …).
pub struct CliError {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    /// A value the registry cannot type — a duration window, a base58 address,
    /// a `label=/path` pair. It reports the same classification the parser
    /// would rather than inventing a second spelling for the same failure.
    fn invalid_value(message: impl Into<String>) -> Self {
        Self::new("cli_invalid_argument_value", message)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self::invalid_value(message)
    }
}

pub enum Mode {
    Cli(Box<CliRequest>),
    Pipe(PipeInit),
    Interactive(InteractiveInit),
    #[cfg(feature = "rpc")]
    Rpc(RpcInit),
    #[cfg(not(feature = "rpc"))]
    Rpc(RpcStub),
    #[cfg(feature = "rest")]
    Rest(RestInit),
    Data(DataOp),
    SkillAdmin(SkillAdminRequest),
    Container(ContainerRequest),
}

/// Payload for `afpay container …`, handled by `crate::container`.
pub struct ContainerRequest {
    pub action: ContainerCliAction,
    pub output: OutputFormat,
}

#[cfg(not(feature = "rpc"))]
pub struct RpcStub;

// ── Agent Skill administration ──────────────
// The CLI-facing enums below convert to `agent_first_data::skill` enums in
// `crate::skill_admin`, so every spore installs its skill through the same admin.

pub struct SkillAdminRequest {
    pub action: SkillAdminAction,
    pub output: OutputFormat,
}

pub enum SkillAdminAction {
    Status(SkillAdminOptions),
    Install(SkillAdminOptions),
    Uninstall(SkillAdminOptions),
}

pub struct SkillAdminOptions {
    pub agent: SkillAgentSelection,
    pub scope: SkillScope,
    pub skills_dir: Option<String>,
    pub force: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillAgentSelection {
    All,
    Codex,
    ClaudeCode,
    Opencode,
    Hermes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillScope {
    Personal,
    Workspace,
}

#[cfg_attr(not(feature = "backup"), allow(dead_code))]
pub struct DataOp {
    pub kind: DataOpKind,
    pub data_dir: Option<String>,
    pub output: OutputFormat,
}

#[cfg_attr(not(feature = "backup"), allow(dead_code))]
pub enum DataOpKind {
    GlobalBackup {
        output_path: Option<String>,
        extra_dirs: Vec<(String, String)>,
    },
    GlobalRestore {
        archive_path: String,
        overwrite: bool,
        pg_url_secret: Option<String>,
        extra_dirs: Vec<(String, String)>,
    },
    NetworkBackup {
        network: Network,
        output_path: Option<String>,
        wallet: Option<String>,
    },
    NetworkRestore {
        network: Network,
        archive_path: String,
        overwrite: bool,
        pg_url_secret: Option<String>,
    },
}

pub struct CliRequest {
    pub input: Input,
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub rpc_endpoint: Option<String>,
    #[cfg_attr(not(feature = "rpc"), allow(dead_code))]
    pub rpc_secret: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
    pub dry_run: bool,
}

pub struct PipeInit {
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
    /// True when the operator passed `--public-listen`. Pipe scrubs the raw
    /// serde error detail from the wire response in this mode so an attacker
    /// poking at the schema does not get field names and byte offsets back.
    /// The full detail still goes to the daemon log when enabled.
    pub scrub_parse_errors: bool,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractiveFrontend {
    Interactive,
    Tui,
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct InteractiveInit {
    pub frontend: InteractiveFrontend,
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub rpc_endpoint: Option<String>,
    pub rpc_secret: Option<String>,
}

// ── Container orchestration (afpay container …) ──────────────
// Builds and runs the afpay daemon image locally; handled by `crate::container`.

pub enum ContainerCliAction {
    Install(ContainerInstallArgs),
    Uninstall(ContainerUninstallArgs),
    Status(ContainerStatusArgs),
    Logs(ContainerLogsArgs),
}

pub struct ContainerCommonArgs {
    pub runtime: Option<ContainerRuntimeArg>,
    pub name: String,
}

pub struct ContainerInstallArgs {
    pub common: ContainerCommonArgs,
    pub port: u16,
    pub mode: ContainerModeArg,
    pub with: Vec<String>,
    pub allow: Vec<String>,
    pub btc_network: String,
    pub btc_rpc_port: u16,
    pub btc_prune_mb: u32,
    pub features: Option<String>,
    pub rebuild: bool,
    pub from_source: bool,
    pub context: Option<String>,
    pub reveal_daemon_secret: bool,
}

pub struct ContainerUninstallArgs {
    pub common: ContainerCommonArgs,
    pub purge: bool,
}

pub struct ContainerStatusArgs {
    pub common: ContainerCommonArgs,
    pub port: u16,
    pub mode: ContainerModeArg,
    pub reveal_daemon_secret: bool,
}

pub struct ContainerLogsArgs {
    pub common: ContainerCommonArgs,
    pub follow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerRuntimeArg {
    Docker,
    Podman,
    Apple,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerModeArg {
    Rest,
    Rpc,
}

// ═══════════════════════════════════════════
// Registry: shared vocabulary
// ═══════════════════════════════════════════

const NETWORKS: [&str; 5] = ["cashu", "ln", "sol", "evm", "btc"];
const AGENTS: [&str; 4] = ["codex", "claude-code", "opencode", "hermes"];
const EVERY_AGENT: &str = "all";

/// Arguments every CLI-mode command accepts. They used to be root-level
/// flags that had to precede the subcommand; the registry is flat, so each
/// command declares them and they are written after the command path.
const RUNTIME_IDS: [&str; 5] = ["data_dir", "log", "rpc_endpoint", "rpc_secret", "dry_run"];

fn runtime_args() -> [ArgSpec; 5] {
    [
        data_dir_arg(),
        ArgSpec::option("--log", "FILTER")
            .repeatable()
            .about("Log filter to enable; repeat, or pass a comma-separated list"),
        ArgSpec::option("--rpc-endpoint", "HOST:PORT")
            .about("Forward the command to a remote afpay RPC daemon instead of running locally"),
        ArgSpec::option("--rpc-secret", "SECRET").about("Shared secret for --rpc-endpoint"),
        ArgSpec::flag("--dry-run").about("Preview the command without executing it"),
    ]
}

fn data_dir_arg() -> ArgSpec {
    ArgSpec::option("--data-dir", "DIR").about("Wallet and data directory")
}

fn protocol() -> OutputSpec {
    OutputSpec::protocol_finite(
        ["json", "yaml", "plain"],
        ["split", "stdout", "stderr"],
        "json",
        "split",
    )
    .file_sinks(["stdout", "stderr"])
}

/// A long-lived session that emits many terminal events over time — the pipe
/// protocol, the REPL, and the two daemons — rather than one result and exit.
fn session() -> OutputSpec {
    OutputSpec::protocol_stream(
        ["json", "yaml", "plain"],
        ["split", "stdout", "stderr"],
        "json",
        "split",
    )
    .file_sinks(["stdout", "stderr"])
}

/// Bytes the container runtime writes straight through this process.
fn passthrough() -> OutputSpec {
    OutputSpec::raw().file_sinks(["stdout", "stderr"])
}

fn group<I, S>(path: I, about: &str) -> CommandSpec
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    CommandSpec::new(path).about(about)
}

/// A command that resolves to a payment request, and therefore accepts the
/// shared runtime arguments.
fn leaf<I, S>(path: I, about: &str) -> CommandSpec
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut command = CommandSpec::new(path).about(about);
    for argument in runtime_args() {
        command = command.arg(argument);
    }
    command
}

/// One shape of a payment request: the runtime arguments are always legal, so
/// they are folded in here rather than repeated on every combination.
fn shape(id: &str, action: &str) -> Combination {
    Combination::new(id)
        .action(action)
        .optional(RUNTIME_IDS)
        .output(protocol())
}

fn wallet_arg(about: &str) -> ArgSpec {
    ArgSpec::option("--wallet", "WALLET_ID").about(about)
}

fn network_arg() -> ArgSpec {
    ArgSpec::option_enum("--network", NETWORKS)
        .value_name("NETWORK")
        .about("Restrict to one network")
}

/// Arguments shared by every `send`. `onchain_memo` is opt-in because Lightning
/// has no on-chain memo to carry: it is absent from `ln send` entirely rather
/// than accepted and then rejected.
fn send_args(onchain_memo: bool) -> Vec<ArgSpec> {
    let mut args = vec![wallet_arg("Source wallet ID (auto-selected if omitted)")];
    if onchain_memo {
        args.push(
            ArgSpec::option("--onchain-memo", "TEXT")
                .about("On-chain memo, sent with the transaction"),
        );
    }
    args.push(
        ArgSpec::option("--local-memo", "KEY=VALUE")
            .repeatable()
            .about("Local bookkeeping annotation; bare text is stored as note=<text>"),
    );
    args.push(ArgSpec::option("--idempotency-key", "KEY").about(
        "Opaque key (≤128 chars); a repeat with the same key and body replays the first \
         response instead of re-broadcasting",
    ));
    args
}

fn send_ids(onchain_memo: bool) -> Vec<&'static str> {
    if onchain_memo {
        vec!["wallet", "onchain_memo", "local_memo", "idempotency_key"]
    } else {
        vec!["wallet", "local_memo", "idempotency_key"]
    }
}

/// Arguments every `receive` accepts, in both its shapes.
fn receive_args() -> Vec<ArgSpec> {
    vec![
        wallet_arg("Wallet ID (auto-selected if omitted)"),
        ArgSpec::flag("--wait").about("Block until a matching payment settles"),
        ArgSpec::option_i64("--wait-timeout-s", "SECONDS").about("Give up waiting after N seconds"),
        ArgSpec::option_i64("--wait-poll-interval-ms", "MS").about("Poll interval while waiting"),
        ArgSpec::flag("--qr-svg-file").about("Write the receive QR payload to an SVG file"),
    ]
}

const RECEIVE_IDLE_IDS: [&str; 2] = ["wallet", "qr_svg_file"];
const RECEIVE_WAIT_IDS: [&str; 4] = [
    "wallet",
    "qr_svg_file",
    "wait_timeout_s",
    "wait_poll_interval_ms",
];

/// Both shapes of one `receive`: an address/invoice now, or the same call
/// blocking until it settles. `--wait-timeout-s` and its siblings describe the
/// wait, so only the waiting shape accepts them.
fn receive_command(
    path: [&str; 2],
    action: &str,
    about: &str,
    idle_extra: &[&'static str],
    wait_extra: &[&'static str],
    extra_args: Vec<ArgSpec>,
) -> CommandSpec {
    let mut command = leaf(path, about);
    for argument in receive_args().into_iter().chain(extra_args) {
        command = command.arg(argument);
    }
    let network = path[0];
    command
        .combination(
            shape(&format!("{network}-receive"), action)
                .about("Return the receive address or invoice and exit")
                .optional(RECEIVE_IDLE_IDS)
                .optional(idle_extra.to_vec()),
        )
        .combination(
            shape(&format!("{network}-receive-wait"), action)
                .about("Block until a matching payment settles; only this shape describes the wait")
                .required(["wait"])
                .optional(RECEIVE_WAIT_IDS)
                .optional(wait_extra.to_vec()),
        )
}

// ═══════════════════════════════════════════
// Registry: per-network command families
// ═══════════════════════════════════════════

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigKind {
    /// Label only.
    Label,
    /// Label, RPC endpoints, and custom token registration.
    SolTokens,
    /// Label, RPC endpoints, chain id, and custom token registration.
    EvmTokens,
}

struct NetworkFamily {
    name: &'static str,
    label: &'static str,
    /// Multi-token networks scope a spend limit by token.
    token_limits: bool,
    config: ConfigKind,
}

const FAMILIES: [NetworkFamily; 5] = [
    NetworkFamily {
        name: "cashu",
        label: "Cashu",
        token_limits: false,
        config: ConfigKind::Label,
    },
    NetworkFamily {
        name: "ln",
        label: "Lightning",
        token_limits: false,
        config: ConfigKind::Label,
    },
    NetworkFamily {
        name: "sol",
        label: "Solana",
        token_limits: true,
        config: ConfigKind::SolTokens,
    },
    NetworkFamily {
        name: "evm",
        label: "EVM",
        token_limits: true,
        config: ConfigKind::EvmTokens,
    },
    NetworkFamily {
        name: "btc",
        label: "Bitcoin",
        token_limits: false,
        config: ConfigKind::Label,
    },
];

/// Every command each of the five networks shares, generated once per network:
/// wallet close/list/seed, spend limits, per-wallet config, balance, and the
/// archive pair.
fn family_commands(family: &NetworkFamily) -> Vec<CommandSpec> {
    let net = family.name;
    let label = family.label;
    let mut commands = vec![
        group([net, "wallet"], &format!("{label} wallet management")),
        leaf([net, "wallet", "close"], &format!("Close a {label} wallet"))
            .arg(wallet_arg("Wallet ID"))
            .arg(
                ArgSpec::flag("--dangerously-skip-balance-check-and-may-lose-money")
                    .about("Close even if the wallet still holds funds"),
            )
            .combination(
                shape(&format!("{net}-wallet-close"), "wallet_close")
                    .required(["wallet"])
                    .optional(["dangerously_skip_balance_check_and_may_lose_money"]),
            ),
        leaf([net, "wallet", "list"], &format!("List {label} wallets"))
            .combination(shape(&format!("{net}-wallet-list"), "wallet_list")),
        leaf(
            [net, "wallet", "dangerously-show-seed"],
            &format!("Reveal the {label} wallet seed; output is deliberately unredacted"),
        )
        .arg(wallet_arg("Wallet ID"))
        .combination(
            shape(&format!("{net}-wallet-show-seed"), "wallet_show_seed").required(["wallet"]),
        ),
        group(
            [net, "limit"],
            &format!("Spend limits for the {label} network or one {label} wallet"),
        ),
        limit_add_command(net, label, family.token_limits),
        group(
            [net, "config"],
            &format!("Per-wallet {label} configuration"),
        ),
        leaf(
            [net, "config", "show"],
            &format!("Show one {label} wallet's configuration"),
        )
        .arg(wallet_arg("Wallet ID"))
        .combination(shape(&format!("{net}-config-show"), "config_show").required(["wallet"])),
        config_set_command(net, label, family.config),
        CommandSpec::new([net, "backup"])
            .about(format!("Back up {label} wallet data to a .tar.zst archive"))
            .arg(data_dir_arg())
            .arg(ArgSpec::option("--archive-out", "PATH").about(format!(
                "Archive path (default: ./afpay-{net}-<epoch>.tar.zst)"
            )))
            .arg(wallet_arg(&format!(
                "Wallet ID (omit to back up every {label} wallet)"
            )))
            .combination(
                Combination::new(format!("{net}-backup"))
                    .action("network_backup")
                    .optional(["data_dir", "archive_out", "wallet"])
                    .output(protocol()),
            ),
        restore_command(net, label),
    ];
    if family.config != ConfigKind::Label {
        commands.push(token_add_command(net, label));
        commands.push(token_remove_command(net, label));
    }
    commands
}

fn limit_add_command(net: &'static str, label: &str, token: bool) -> CommandSpec {
    let mut command = leaf(
        [net, "limit", "add"],
        &format!("Add a {label} network or wallet spend limit"),
    )
    .arg(wallet_arg(
        "Wallet ID; omit for a limit covering the whole network",
    ))
    .arg(ArgSpec::option("--window", "DURATION").about("Rolling window, e.g. 30m, 1h, 24h, 7d"))
    .arg(ArgSpec::option_i64("--max-spend", "AMOUNT").about("Maximum spend in base units"));
    let mut optional = vec!["wallet"];
    if token {
        command =
            command.arg(ArgSpec::option("--token", "TOKEN").about("Token the limit applies to"));
        optional.push("token");
    }
    command.combination(
        shape(&format!("{net}-limit-add"), "limit_add")
            .required(["window", "max_spend"])
            .optional(optional),
    )
}

fn config_set_command(net: &'static str, label: &str, kind: ConfigKind) -> CommandSpec {
    let mut command = leaf(
        [net, "config", "set"],
        &format!("Update one {label} wallet's settings"),
    )
    .arg(wallet_arg("Wallet ID"))
    .arg(ArgSpec::option("--label", "LABEL").about("New wallet label"));
    let mut optional = vec!["label"];
    match kind {
        ConfigKind::Label => {}
        ConfigKind::SolTokens => {
            command = command.arg(
                ArgSpec::option("--sol-rpc-endpoint", "URL")
                    .repeatable()
                    .about("Replace the Solana JSON-RPC endpoints, in failover order"),
            );
            optional.push("sol_rpc_endpoint");
        }
        ConfigKind::EvmTokens => {
            command = command
                .arg(
                    ArgSpec::option("--evm-rpc-endpoint", "URL")
                        .repeatable()
                        .about("Replace the EVM JSON-RPC endpoints, in failover order"),
                )
                .arg(ArgSpec::option_i64("--chain-id", "ID").about("EVM chain ID"));
            optional.push("evm_rpc_endpoint");
            optional.push("chain_id");
        }
    }
    command.combination(
        shape(&format!("{net}-config-set"), "config_set")
            .required(["wallet"])
            .optional(optional),
    )
}

fn token_add_command(net: &'static str, label: &str) -> CommandSpec {
    leaf(
        [net, "config", "token-add"],
        &format!("Register a custom {label} token for balance tracking"),
    )
    .arg(wallet_arg("Wallet ID"))
    .arg(ArgSpec::option("--symbol", "SYMBOL").about("Token symbol, e.g. dai"))
    .arg(ArgSpec::option("--address", "ADDRESS").about("Token contract or mint address"))
    .arg(
        ArgSpec::option_i64("--decimals", "N")
            .default_i64(6)
            .about("Token decimals"),
    )
    .combination(
        shape(&format!("{net}-token-add"), "config_token_add")
            .required(["wallet", "symbol", "address"])
            .optional(["decimals"]),
    )
}

fn token_remove_command(net: &'static str, label: &str) -> CommandSpec {
    leaf(
        [net, "config", "token-remove"],
        &format!("Unregister a custom {label} token"),
    )
    .arg(wallet_arg("Wallet ID"))
    .arg(ArgSpec::option("--symbol", "SYMBOL").about("Token symbol to remove"))
    .combination(
        shape(&format!("{net}-token-remove"), "config_token_remove").required(["wallet", "symbol"]),
    )
}

fn restore_command(net: &'static str, label: &str) -> CommandSpec {
    CommandSpec::new([net, "restore"])
        .about(format!(
            "Restore {label} wallet data from a .tar.zst archive"
        ))
        .arg(ArgSpec::positional("archive", 0, "ARCHIVE").about("Path to the backup archive"))
        .arg(data_dir_arg())
        .arg(
            ArgSpec::flag("--dangerously-overwrite")
                .about("Clear existing data before restoring instead of merging"),
        )
        .arg(
            ArgSpec::option("--pg-url-secret", "URL")
                .about("Override the PostgreSQL connection URL for the pg restore step"),
        )
        .combination(
            Combination::new(format!("{net}-restore"))
                .action("network_restore")
                .required(["archive"])
                .optional(["data_dir", "dangerously_overwrite", "pg_url_secret"])
                .output(protocol()),
        )
}

fn balance_command(family: &NetworkFamily) -> CommandSpec {
    let net = family.name;
    let mut command = leaf([net, "balance"], &format!("{} balance", family.label)).arg(wallet_arg(
        &format!("Wallet ID (omit to show every {} wallet)", family.label),
    ));
    let mut optional = vec!["wallet"];
    if net == "cashu" {
        command = command.arg(
            ArgSpec::flag("--check")
                .about("Verify proofs against the mint; slower but authoritative"),
        );
        optional.push("check");
    }
    command.combination(shape(&format!("{net}-balance"), "network_balance").optional(optional))
}

// ═══════════════════════════════════════════
// Registry: the whole CLI
// ═══════════════════════════════════════════

/// Build the registry. Every legal `afpay` invocation is one combination here.
fn build_cli_spec() -> Result<BuiltCliSpec, CliSpecError> {
    let mut spec = CliSpec::new("afpay", env!("CARGO_PKG_VERSION"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .display_name(env!("DISPLAY_NAME"))
        .lifecycle_output(protocol())
        .command(root_command());
    if let Some(build) = Some(env!("GIT_SHA")).filter(|sha| *sha != "unknown") {
        spec = spec.build_id(build);
    }

    for command in global_commands()
        .into_iter()
        .chain(network_commands())
        .chain(cross_network_commands())
        .chain(skill_commands())
        .chain(container_commands())
    {
        spec = spec.command(command);
    }

    build_afdata_cli(spec)
}

/// `afpay` with no subcommand runs a long-lived session instead of one request.
/// Each session is its own shape, so the arguments that only configure the gRPC
/// daemon cannot be passed to the REPL, and vice versa.
fn root_command() -> CommandSpec {
    let mut modes = vec!["pipe", "interactive", "tui", "rpc"];
    #[cfg(feature = "rest")]
    modes.push("rest");

    let mut command = CommandSpec::new(Vec::<String>::new())
        .about("Run a long-lived afpay session instead of a single command")
        .arg(
            ArgSpec::option_enum("--mode", modes)
                .value_name("MODE")
                .about("Long-lived session to run instead of a single command"),
        )
        .arg(data_dir_arg())
        .arg(
            ArgSpec::option("--log", "FILTER")
                .repeatable()
                .about("Log filter to enable; repeat, or pass a comma-separated list"),
        )
        .arg(
            ArgSpec::option("--rpc-endpoint", "HOST:PORT")
                .about("Drive a remote afpay RPC daemon from this session"),
        )
        .arg(ArgSpec::option("--rpc-secret", "SECRET").about("Shared secret for the RPC channel"))
        .arg(
            ArgSpec::option("--rpc-listen", "HOST:PORT")
                .default("127.0.0.1:9400")
                .about("Listen address for the RPC daemon"),
        )
        .arg(
            ArgSpec::flag("--public-listen").about(
                "Allow binding to a non-loopback address; use only behind TLS or a firewall",
            ),
        );

    #[cfg(feature = "rest")]
    {
        command = command
            .arg(
                ArgSpec::option("--rest-listen", "HOST:PORT")
                    .default("127.0.0.1:9401")
                    .about("Listen address for the REST server"),
            )
            .arg(
                ArgSpec::option("--rest-api-key-secret", "KEY")
                    .about("Bearer API key the REST server requires"),
            );
    }

    command = command
        .combination(
            Combination::new("session-pipe")
                .action("mode_pipe")
                .about("Read JSONL requests on stdin and answer on stdout")
                .fixed("mode", "pipe")
                .optional(["data_dir", "log", "public_listen"])
                .output(session()),
        )
        .combination(
            Combination::new("session-interactive")
                .action("mode_interactive")
                .about("Human REPL with completion and QR helpers")
                .fixed("mode", "interactive")
                .optional(["data_dir", "log", "rpc_endpoint", "rpc_secret"])
                .output(session()),
        )
        .combination(
            Combination::new("session-tui")
                .action("mode_tui")
                .about("Full-screen terminal workflow over the same command interface")
                .fixed("mode", "tui")
                .optional(["data_dir", "log", "rpc_endpoint", "rpc_secret"])
                .output(session()),
        )
        .combination(
            Combination::new("session-rpc")
                .action("mode_rpc")
                .about("Encrypted gRPC daemon; only this shape accepts --rpc-listen")
                .fixed("mode", "rpc")
                .optional([
                    "data_dir",
                    "log",
                    "rpc_listen",
                    "rpc_secret",
                    "public_listen",
                ])
                .output(session()),
        );

    #[cfg(feature = "rest")]
    {
        command = command.combination(
            Combination::new("session-rest")
                .action("mode_rest")
                .about("HTTP API server; only this shape accepts --rest-listen")
                .fixed("mode", "rest")
                .optional([
                    "data_dir",
                    "log",
                    "rest_listen",
                    "rest_api_key_secret",
                    "public_listen",
                ])
                .output(session()),
        );
    }

    command
}

fn global_commands() -> Vec<CommandSpec> {
    vec![
        group(
            ["global"],
            "Cross-network operations and global configuration",
        ),
        group(["global", "limit"], "Global spend limit, in USD cents"),
        leaf(
            ["global", "limit", "add"],
            "Add a global spend limit, in USD cents",
        )
        .arg(ArgSpec::option("--window", "DURATION").about("Rolling window, e.g. 30m, 1h, 24h, 7d"))
        .arg(ArgSpec::option_i64("--max-spend", "CENTS").about("Maximum spend in USD cents"))
        .combination(
            shape("global-limit-add", "global_limit_add").required(["window", "max_spend"]),
        ),
        group(["global", "config"], "Global runtime configuration"),
        leaf(
            ["global", "config", "get"],
            "Read the runtime configuration, or one value by dot-path",
        )
        .arg(
            ArgSpec::positional("key", 0, "KEY")
                .about("Dot-path key, e.g. log or exchange_rate.ttl_s; omit to show everything"),
        )
        .combination(shape("global-config-get", "global_config_get").optional(["key"])),
        leaf(
            ["global", "config", "set"],
            "Set one runtime configuration value",
        )
        .arg(ArgSpec::positional("key", 0, "KEY").about("Dot-path key"))
        .arg(
            ArgSpec::positional("values", 1, "VALUE")
                .repeatable()
                .about("Value, or repeated values for a list-valued key"),
        )
        .combination(
            shape("global-config-set", "global_config_set")
                .required(["key"])
                .optional(["values"]),
        ),
        CommandSpec::new(["global", "backup"])
            .about("Back up every network's data to a .tar.zst archive")
            .arg(data_dir_arg())
            .arg(
                ArgSpec::option("--archive-out", "PATH")
                    .about("Archive path (default: ./afpay-global-<epoch>.tar.zst)"),
            )
            .arg(
                ArgSpec::option("--extra-dir", "LABEL=/path")
                    .repeatable()
                    .about("Also archive this directory under LABEL"),
            )
            .combination(
                Combination::new("global-backup")
                    .action("global_backup")
                    .optional(["data_dir", "archive_out", "extra_dir"])
                    .output(protocol()),
            ),
        CommandSpec::new(["global", "restore"])
            .about("Restore every network's data from a .tar.zst archive")
            .arg(ArgSpec::positional("archive", 0, "ARCHIVE").about("Path to the backup archive"))
            .arg(data_dir_arg())
            .arg(
                ArgSpec::flag("--dangerously-overwrite")
                    .about("Clear existing data before restoring instead of merging"),
            )
            .arg(
                ArgSpec::option("--pg-url-secret", "URL")
                    .about("Override the PostgreSQL connection URL for the pg restore step"),
            )
            .arg(
                ArgSpec::option("--extra-dir", "LABEL=/path")
                    .repeatable()
                    .about("Also restore this directory from LABEL"),
            )
            .combination(
                Combination::new("global-restore")
                    .action("global_restore")
                    .required(["archive"])
                    .optional([
                        "data_dir",
                        "dangerously_overwrite",
                        "pg_url_secret",
                        "extra_dir",
                    ])
                    .output(protocol()),
            ),
    ]
}

fn network_commands() -> Vec<CommandSpec> {
    let mut commands = Vec::new();
    for family in &FAMILIES {
        commands.push(group(
            [family.name],
            match family.name {
                "cashu" => "Cashu ecash operations",
                "ln" => "Lightning Network operations (NWC, phoenixd, LNbits)",
                "sol" => "Solana operations",
                "evm" => "EVM chain operations (Base, Arbitrum, …)",
                _ => "Bitcoin on-chain operations",
            },
        ));
        commands.extend(family_commands(family));
        commands.push(balance_command(family));
    }
    commands.extend(cashu_commands());
    commands.extend(ln_commands());
    commands.extend(sol_commands());
    commands.extend(evm_commands());
    commands.extend(btc_commands());
    commands
}

fn cashu_commands() -> Vec<CommandSpec> {
    vec![
        leaf(
            ["cashu", "wallet", "create"],
            "Create a Cashu wallet on one mint",
        )
        .arg(ArgSpec::option("--cashu-mint", "URL").about("Cashu mint URL"))
        .arg(ArgSpec::option("--label", "LABEL").about("Optional wallet label"))
        .arg(
            ArgSpec::option("--mnemonic-secret", "WORDS")
                .about("Existing BIP39 mnemonic to restore this wallet from"),
        )
        .combination(
            shape("cashu-wallet-create", "cashu_wallet_create")
                .required(["cashu_mint"])
                .optional(["label", "mnemonic_secret"]),
        ),
        leaf(
            ["cashu", "wallet", "restore"],
            "Restore lost proofs from the mint, repairing counter or proof drift",
        )
        .arg(wallet_arg("Wallet ID"))
        .combination(shape("cashu-wallet-restore", "cashu_wallet_restore").required(["wallet"])),
        {
            let mut command = leaf(
                ["cashu", "send"],
                "Mint a P2P Cashu token; to pay a Lightning invoice use `cashu send-to-ln`",
            )
            .arg(ArgSpec::option_i64("--amount-sats", "SATS").about("Amount in sats"))
            .arg(
                ArgSpec::option("--cashu-mint", "URL")
                    .repeatable()
                    .about("Restrict to wallets on these mints, tried in order"),
            );
            for argument in send_args(true) {
                command = command.arg(argument);
            }
            command.combination(
                shape("cashu-send", "cashu_send")
                    .required(["amount_sats"])
                    .optional(["cashu_mint"])
                    .optional(send_ids(true)),
            )
        },
        leaf(["cashu", "receive"], "Redeem a Cashu token into a wallet")
            .arg(ArgSpec::positional("token", 0, "TOKEN").about("Cashu token string"))
            .arg(wallet_arg(
                "Wallet ID (auto-matched from the token if omitted)",
            ))
            .combination(
                shape("cashu-receive-token", "cashu_receive")
                    .required(["token"])
                    .optional(["wallet"]),
            ),
        {
            let mut command = leaf(
                ["cashu", "send-to-ln"],
                "Melt Cashu proofs to pay a Lightning invoice",
            )
            .arg(ArgSpec::option("--to", "BOLT11").about("Lightning invoice to pay"));
            for argument in send_args(true) {
                command = command.arg(argument);
            }
            command.combination(
                shape("cashu-send-to-ln", "cashu_send_to_ln")
                    .required(["to"])
                    .optional(send_ids(true)),
            )
        },
        receive_command(
            ["cashu", "receive-from-ln"],
            "cashu_receive_from_ln",
            "Create a Lightning invoice that mints Cashu proofs when paid",
            &["amount_sats", "onchain_memo"],
            &["amount_sats", "onchain_memo"],
            vec![
                ArgSpec::option_i64("--amount-sats", "SATS").about("Amount in sats"),
                ArgSpec::option("--onchain-memo", "TEXT").about("Invoice description"),
            ],
        ),
        leaf(
            ["cashu", "receive-from-ln-claim"],
            "Claim the proofs minted by a settled receive-from-ln quote",
        )
        .arg(wallet_arg("Wallet ID"))
        .arg(
            ArgSpec::option("--ln-quote-id", "QUOTE_ID")
                .about("Quote ID or payment hash from the deposit"),
        )
        .combination(
            shape("cashu-receive-from-ln-claim", "cashu_receive_from_ln_claim")
                .required(["wallet", "ln_quote_id"]),
        ),
    ]
}

/// `ln wallet create` as three shapes, one per backend. Each backend needs a
/// different credential, and the registry now says which — the old code took
/// every credential and then failed at runtime on the wrong combination.
fn ln_wallet_create_command() -> CommandSpec {
    leaf(["ln", "wallet", "create"], "Create a Lightning wallet")
        .arg(
            ArgSpec::option_enum("--backend", ["nwc", "phoenixd", "lnbits"])
                .value_name("BACKEND")
                .about("Lightning backend this wallet talks to"),
        )
        .arg(ArgSpec::option("--nwc-uri-secret", "URI").about("NWC connection URI"))
        .arg(ArgSpec::option("--endpoint-url", "URL").about("Backend HTTP endpoint"))
        .arg(ArgSpec::option("--password-secret", "PASSWORD").about("phoenixd HTTP password"))
        .arg(ArgSpec::option("--admin-key-secret", "KEY").about("LNbits admin API key"))
        .arg(ArgSpec::option("--label", "LABEL").about("Optional wallet label"))
        .combination(
            shape("ln-wallet-create-nwc", "ln_wallet_create")
                .about("Nostr Wallet Connect; authenticates with a connection URI")
                .fixed("backend", "nwc")
                .required(["nwc_uri_secret"])
                .optional(["label"]),
        )
        .combination(
            shape("ln-wallet-create-phoenixd", "ln_wallet_create")
                .about("phoenixd; authenticates with an endpoint and HTTP password")
                .fixed("backend", "phoenixd")
                .required(["endpoint_url", "password_secret"])
                .optional(["label"]),
        )
        .combination(
            shape("ln-wallet-create-lnbits", "ln_wallet_create")
                .about("LNbits; authenticates with an endpoint and admin API key")
                .fixed("backend", "lnbits")
                .required(["endpoint_url", "admin_key_secret"])
                .optional(["label"]),
        )
}

fn ln_commands() -> Vec<CommandSpec> {
    vec![
        ln_wallet_create_command(),
        {
            let mut command = leaf(
                ["ln", "send"],
                "Pay a BOLT11 invoice or a BOLT12 offer. Lightning carries no on-chain memo, \
                 so annotate with --local-memo",
            )
            .arg(ArgSpec::option("--to", "INVOICE").about("BOLT11 invoice or BOLT12 offer (lno1…)"))
            .arg(
                ArgSpec::option_i64("--amount-sats", "SATS")
                    .about("Amount in sats; required for a BOLT12 offer, rejected for BOLT11"),
            );
            for argument in send_args(false) {
                command = command.arg(argument);
            }
            command.combination(
                shape("ln-send", "ln_send")
                    .required(["to"])
                    .optional(["amount_sats"])
                    .optional(send_ids(false)),
            )
        },
        receive_command(
            ["ln", "receive"],
            "ln_receive",
            "Create a BOLT11 invoice, or return the reusable BOLT12 offer",
            &["amount_sats"],
            &["amount_sats"],
            vec![
                ArgSpec::option_i64("--amount-sats", "SATS")
                    .about("Amount in sats; omit for a BOLT12 offer"),
            ],
        ),
    ]
}

fn sol_commands() -> Vec<CommandSpec> {
    vec![
        leaf(["sol", "wallet", "create"], "Create a Solana wallet")
            .arg(
                ArgSpec::option("--sol-rpc-endpoint", "URL")
                    .repeatable()
                    .about("Solana JSON-RPC endpoint; repeat to set the failover order"),
            )
            .arg(ArgSpec::option("--label", "LABEL").about("Optional wallet label"))
            .arg(
                ArgSpec::option_enum("--sol-cluster", ["mainnet-beta", "devnet", "testnet"])
                    .value_name("CLUSTER")
                    .about("Cluster pinned to this wallet; sends elsewhere are rejected"),
            )
            .combination(
                shape("sol-wallet-create", "sol_wallet_create")
                    .required(["sol_rpc_endpoint"])
                    .optional(["label", "sol_cluster"]),
            ),
        {
            let mut command = leaf(["sol", "send"], "Send SOL or an SPL token")
                .arg(ArgSpec::option("--to", "ADDRESS").about("Recipient Solana address (base58)"))
                .arg(
                    ArgSpec::option_i64("--amount", "BASE_UNITS")
                        .about("Amount in base units (lamports for SOL)"),
                )
                .arg(
                    ArgSpec::option("--token", "TOKEN")
                        .about("`native` for SOL, or a registered token symbol"),
                )
                .arg(
                    ArgSpec::option("--reference", "KEY")
                        .about("Reference key for order binding (base58-encoded 32 bytes)"),
                );
            for argument in send_args(true) {
                command = command.arg(argument);
            }
            command.combination(
                shape("sol-send", "sol_send")
                    .required(["to", "amount", "token"])
                    .optional(["reference"])
                    .optional(send_ids(true)),
            )
        },
        receive_command(
            ["sol", "receive"],
            "sol_receive",
            "Show the wallet's receive address",
            &[],
            &["onchain_memo", "min_confirmations", "reference"],
            vec![
                ArgSpec::option("--onchain-memo", "TEXT").about("On-chain memo to watch for"),
                ArgSpec::option_i64("--min-confirmations", "N")
                    .about("Confirmation depth before the payment counts as settled"),
                ArgSpec::option("--reference", "KEY").about("Reference key to watch for (base58)"),
            ],
        ),
    ]
}

fn evm_commands() -> Vec<CommandSpec> {
    vec![
        leaf(["evm", "wallet", "create"], "Create an EVM chain wallet")
            .arg(
                ArgSpec::option("--evm-rpc-endpoint", "URL")
                    .repeatable()
                    .about("EVM JSON-RPC endpoint; repeat to set the failover order"),
            )
            .arg(
                ArgSpec::option_i64("--chain-id", "ID")
                    .default_i64(8453)
                    .about("EVM chain ID"),
            )
            .arg(ArgSpec::option("--label", "LABEL").about("Optional wallet label"))
            .combination(
                shape("evm-wallet-create", "evm_wallet_create")
                    .required(["evm_rpc_endpoint"])
                    .optional(["chain_id", "label"]),
            ),
        {
            let mut command = leaf(
                ["evm", "send"],
                "Send the chain's native token or an ERC-20",
            )
            .arg(ArgSpec::option("--to", "ADDRESS").about("Recipient address (0x…)"))
            .arg(
                ArgSpec::option_i64("--amount", "BASE_UNITS")
                    .about("Amount in base units (wei for ETH)"),
            )
            .arg(
                ArgSpec::option("--token", "TOKEN").about("`native`, or a registered token symbol"),
            )
            .arg(
                ArgSpec::option_i64("--chain-id", "ID")
                    .about("Pin the chain; a mismatched wallet returns wrong_chain"),
            );
            for argument in send_args(true) {
                command = command.arg(argument);
            }
            command.combination(
                shape("evm-send", "evm_send")
                    .required(["to", "amount", "token"])
                    .optional(["chain_id"])
                    .optional(send_ids(true)),
            )
        },
        // EVM has no watcher, so this command has one shape and no --wait: the
        // old code accepted the flag and then rejected every use of it.
        leaf(["evm", "receive"], "Show the wallet's receive address")
            .arg(wallet_arg("Wallet ID (auto-selected if omitted)"))
            .arg(ArgSpec::option("--onchain-memo", "TEXT").about("Memo recorded with the request"))
            .combination(shape("evm-receive", "evm_receive").optional(["wallet", "onchain_memo"])),
    ]
}

/// `btc wallet create` as three shapes, one per chain source. Which URL the
/// wallet needs follows from the backend, so the registry pairs them instead of
/// letting the provider discover the mismatch later.
fn btc_wallet_create_command() -> CommandSpec {
    let base = |command: CommandSpec| {
        command
            .arg(
                ArgSpec::option_enum("--btc-network", ["mainnet", "signet"])
                    .value_name("NETWORK")
                    .default("mainnet")
                    .about("Bitcoin sub-network"),
            )
            .arg(
                ArgSpec::option_enum("--btc-address-type", ["taproot", "segwit"])
                    .value_name("TYPE")
                    .default("taproot")
                    .about("Address type"),
            )
            .arg(
                ArgSpec::option_enum("--btc-backend", ["esplora", "core-rpc", "electrum"])
                    .value_name("BACKEND")
                    .default("esplora")
                    .about("Chain-source backend"),
            )
            .arg(ArgSpec::option("--btc-esplora-url", "URL").about("Custom Esplora API URL"))
            .arg(ArgSpec::option("--btc-core-url", "URL").about("Bitcoin Core RPC URL"))
            .arg(
                ArgSpec::option("--btc-core-auth-secret", "USER:PASS")
                    .about("Bitcoin Core RPC credentials"),
            )
            .arg(ArgSpec::option("--btc-electrum-url", "URL").about("Electrum server URL"))
            .arg(
                ArgSpec::option("--mnemonic-secret", "WORDS")
                    .about("Existing BIP39 mnemonic to restore this wallet from"),
            )
            .arg(ArgSpec::option("--label", "LABEL").about("Optional wallet label"))
    };
    let common = [
        "btc_network",
        "btc_address_type",
        "mnemonic_secret",
        "label",
    ];
    base(leaf(["btc", "wallet", "create"], "Create a Bitcoin wallet"))
        .combination(
            shape("btc-wallet-create-esplora", "btc_wallet_create")
                .about("Esplora chain source; only this shape accepts --btc-esplora-url")
                .fixed("btc_backend", "esplora")
                .optional(["btc_esplora_url"])
                .optional(common),
        )
        .combination(
            shape("btc-wallet-create-core-rpc", "btc_wallet_create")
                .about("Bitcoin Core RPC chain source; requires --btc-core-url")
                .fixed("btc_backend", "core-rpc")
                .required(["btc_core_url"])
                .optional(["btc_core_auth_secret"])
                .optional(common),
        )
        .combination(
            shape("btc-wallet-create-electrum", "btc_wallet_create")
                .about("Electrum chain source; requires --btc-electrum-url")
                .fixed("btc_backend", "electrum")
                .required(["btc_electrum_url"])
                .optional(common),
        )
}

fn btc_commands() -> Vec<CommandSpec> {
    vec![
        btc_wallet_create_command(),
        {
            let mut command = leaf(["btc", "send"], "Send BTC on-chain")
                .arg(
                    ArgSpec::option("--to", "ADDRESS")
                        .about("Recipient Bitcoin address (bc1… / tb1…)"),
                )
                .arg(ArgSpec::option_i64("--amount-sats", "SATS").about("Amount in satoshis"));
            for argument in send_args(true) {
                command = command.arg(argument);
            }
            command.combination(
                shape("btc-send", "btc_send")
                    .required(["to", "amount_sats"])
                    .optional(send_ids(true)),
            )
        },
        receive_command(
            ["btc", "receive"],
            "btc_receive",
            "Show the wallet's receive address",
            &[],
            &["wait_sync_limit"],
            vec![
                ArgSpec::option_i64("--wait-sync-limit", "N")
                    .about("Max history records scanned per poll while resolving the tx id"),
            ],
        ),
    ]
}

fn cross_network_commands() -> Vec<CommandSpec> {
    vec![
        group(["wallet"], "Cross-network wallet views"),
        leaf(["wallet", "list"], "List wallets across every network")
            .arg(network_arg())
            .combination(shape("wallet-list-all", "wallet_list").optional(["network"])),
        leaf(["balance"], "Balance across every network")
            .arg(wallet_arg("Wallet ID (omit to show every wallet)"))
            .arg(network_arg())
            .arg(
                ArgSpec::flag("--cashu-check")
                    .about("Verify Cashu proofs against the mint; slower but authoritative"),
            )
            .combination(shape("balance-all", "balance").optional([
                "wallet",
                "network",
                "cashu_check",
            ])),
        group(["history"], "Transaction history"),
        leaf(
            ["history", "list"],
            "List history records from the local store",
        )
        .arg(wallet_arg("Filter by wallet ID"))
        .arg(network_arg())
        .arg(ArgSpec::option("--onchain-memo", "TEXT").about("Filter by exact on-chain memo"))
        .arg(
            ArgSpec::option_i64("--limit", "N")
                .default_i64(20)
                .about("Maximum records returned"),
        )
        .arg(
            ArgSpec::option_i64("--offset", "N")
                .default_i64(0)
                .about("Records to skip"),
        )
        .arg(
            ArgSpec::option_i64("--since-epoch-s", "EPOCH")
                .about("Only records created at or after this epoch second"),
        )
        .arg(
            ArgSpec::option_i64("--until-epoch-s", "EPOCH")
                .about("Only records created before this epoch second"),
        )
        .combination(shape("history-list", "history_list").optional([
            "wallet",
            "network",
            "onchain_memo",
            "limit",
            "offset",
            "since_epoch_s",
            "until_epoch_s",
        ])),
        leaf(
            ["history", "status"],
            "Report one transaction's current status",
        )
        .arg(ArgSpec::option("--transaction-id", "TX_ID").about("Transaction ID"))
        .combination(shape("history-status", "history_status").required(["transaction_id"])),
        leaf(
            ["history", "update"],
            "Incrementally sync backend history into the local store",
        )
        .arg(wallet_arg(
            "Sync one wallet (default: every wallet in scope)",
        ))
        .arg(network_arg())
        .arg(
            ArgSpec::option_i64("--limit", "N")
                .default_i64(200)
                .about("Maximum records scanned per wallet"),
        )
        .combination(
            shape("history-update", "history_update").optional(["wallet", "network", "limit"]),
        ),
        group(["limit"], "Cross-network spend-limit administration"),
        leaf(
            ["limit", "list"],
            "Show every spend-limit rule and its usage",
        )
        .combination(shape("limit-list", "limit_list")),
        leaf(["limit", "remove"], "Remove a spend-limit rule by ID")
            .arg(ArgSpec::option("--rule-id", "RULE_ID").about("Rule ID, e.g. r_1a2b3c4d"))
            .combination(shape("limit-remove", "limit_remove").required(["rule_id"])),
        leaf(
            ["limit", "reconcile"],
            "Force a stuck spend-ledger reservation to a terminal state (operator-only)",
        )
        .arg(ArgSpec::option_i64("--reservation-id", "ID").about("Reservation ID"))
        .arg(ArgSpec::flag("--confirm").about("The payment did settle"))
        .arg(ArgSpec::flag("--cancel").about("The payment did not settle"))
        .arg(
            ArgSpec::option("--reason", "TEXT")
                .about("Audit note (1..=512 chars) explaining the forced outcome"),
        )
        .combination(
            shape("limit-reconcile-confirm", "limit_reconcile_confirm")
                .about("Record the spend: the payment actually succeeded")
                .required(["reservation_id", "confirm", "reason"]),
        )
        .combination(
            shape("limit-reconcile-cancel", "limit_reconcile_cancel")
                .about("Release the reservation: the payment did not happen")
                .required(["reservation_id", "cancel", "reason"]),
        ),
    ]
}

/// One skill verb, as two shapes. `--skills-dir` names a single directory, so
/// it is meaningless when the verb fans out across every agent.
fn skill_command(verb: &str, about: &str, force: bool) -> CommandSpec {
    let mut command = CommandSpec::new(["skill", verb])
        .about(about)
        .arg(
            ArgSpec::option_enum("--agent", std::iter::once(EVERY_AGENT).chain(AGENTS))
                .value_name("AGENT")
                .default(EVERY_AGENT)
                .about("Agent to manage"),
        )
        .arg(
            ArgSpec::option_enum("--scope", ["personal", "workspace"])
                .value_name("SCOPE")
                .default("personal")
                .about("Skill scope"),
        )
        .arg(ArgSpec::option("--skills-dir", "DIR").about("Directory that contains skill folders"));

    let mut every: Vec<&str> = vec!["scope"];
    let mut named: Vec<&str> = vec!["scope", "skills_dir"];
    if force {
        command =
            command
                .arg(ArgSpec::flag("--force").about(
                    "Overwrite or remove an Agent-First Pay skill this tool did not manage",
                ));
        every.push("force");
        named.push("force");
    }

    command
        .combination(
            Combination::new(format!("skill-{verb}-every-agent"))
                .action(format!("skill_{verb}"))
                .about("Target every agent that supports the scope")
                .fixed("agent", EVERY_AGENT)
                .optional(every)
                .output(protocol()),
        )
        .combination(
            Combination::new(format!("skill-{verb}-one-agent"))
                .action(format!("skill_{verb}"))
                .about("Target one named agent; only this shape accepts --skills-dir")
                .fixed_one_of("agent", AGENTS)
                .optional(named)
                .output(protocol()),
        )
}

fn skill_commands() -> Vec<CommandSpec> {
    vec![
        group(
            ["skill"],
            "Manage the embedded Agent Skill for Codex, Claude Code, opencode, and Hermes",
        ),
        skill_command(
            "status",
            "Show whether the Agent-First Pay skill is installed, valid, and up to date",
            false,
        ),
        skill_command(
            "install",
            "Install or refresh the Agent-First Pay skill",
            true,
        ),
        skill_command(
            "uninstall",
            "Remove an afpay-managed Agent-First Pay skill",
            true,
        ),
    ]
}

fn container_base(command: CommandSpec) -> CommandSpec {
    command
        .arg(
            ArgSpec::option_enum("--runtime", ["docker", "podman", "apple"])
                .value_name("RUNTIME")
                .about("Container runtime; auto-detected when omitted"),
        )
        .arg(
            ArgSpec::option("--name", "NAME")
                .default("afpay")
                .about("Container name"),
        )
}

fn container_mode_arg() -> ArgSpec {
    ArgSpec::option_enum("--mode", ["rest", "rpc"])
        .value_name("MODE")
        .default("rest")
        .about("Server mode: rest (HTTP + bearer key) or rpc (gRPC + PSK)")
}

fn container_port_arg() -> ArgSpec {
    ArgSpec::option_i64("--port", "PORT")
        .default_i64(9401)
        .about("Daemon port, published on 127.0.0.1")
}

fn reveal_arg() -> ArgSpec {
    ArgSpec::flag("--reveal-daemon-secret")
        .about("Print the generated daemon credential and the credential-bearing client command")
}

fn container_commands() -> Vec<CommandSpec> {
    let install_common = [
        "runtime",
        "name",
        "port",
        "mode",
        "with",
        "allow",
        "btc_network",
        "btc_rpc_port",
        "btc_prune_mb",
        "reveal_daemon_secret",
    ];
    vec![
        group(
            ["container"],
            "Build and run the afpay daemon container (Docker, Podman, or Apple)",
        ),
        container_base(
            CommandSpec::new(["container", "install"])
                .about("Build the image if missing, run the daemon, and print the client command"),
        )
        .arg(container_port_arg())
        .arg(container_mode_arg())
        .arg(
            ArgSpec::option_enum("--with", ["phoenixd", "bitcoind"])
                .value_name("DAEMON")
                .repeatable()
                .about("Bundled daemon to install and enable"),
        )
        .arg(
            ArgSpec::option("--allow", "CATEGORY=URL")
                .repeatable()
                .about(
                    "Operator allowlist entry; a public listener refuses to start without one. \
                     Categories: mint, esplora, sol-rpc, evm-rpc, btc-core, btc-electrum, ln",
                ),
        )
        .arg(
            ArgSpec::option_enum("--btc-network", ["mainnet", "signet"])
                .value_name("NETWORK")
                .default("mainnet")
                .about("Bitcoin network when --with bitcoind"),
        )
        .arg(
            ArgSpec::option_i64("--btc-rpc-port", "PORT")
                .default_i64(8332)
                .about("bitcoind RPC port when --with bitcoind"),
        )
        .arg(
            ArgSpec::option_i64("--btc-prune-mb", "MB")
                .default_i64(550)
                .about("bitcoind prune target when --with bitcoind"),
        )
        .arg(ArgSpec::flag("--rebuild").about("Rebuild the image even when it already exists"))
        .arg(
            ArgSpec::flag("--from-source")
                .about("Compile the image from a source checkout instead of the prebuilt release"),
        )
        .arg(
            ArgSpec::option("--features", "FEATURES")
                .about("Cargo feature set for the source build"),
        )
        .arg(ArgSpec::option("--context", "DIR").about("Source checkout to build from"))
        .arg(reveal_arg())
        .combination(
            Combination::new("container-install-release")
                .action("container_install")
                .about("Download the prebuilt release image pinned to this binary")
                .optional(install_common)
                .optional(["rebuild"])
                .output(protocol()),
        )
        .combination(
            Combination::new("container-install-from-source")
                .action("container_install")
                .about("Compile from a checkout; only this shape accepts --features and --context")
                .required(["from_source"])
                .optional(install_common)
                .optional(["features", "context"])
                .output(protocol()),
        ),
        container_base(
            CommandSpec::new(["container", "uninstall"])
                .about("Stop and remove the container; --purge also drops the image and cache"),
        )
        .arg(ArgSpec::flag("--purge").about("Also remove the built image and the cached context"))
        .combination(
            Combination::new("container-uninstall")
                .action("container_uninstall")
                .optional(["runtime", "name", "purge"])
                .output(protocol()),
        ),
        container_base(
            CommandSpec::new(["container", "status"]).about(
                "Report whether the daemon is running, with its endpoint and client command",
            ),
        )
        .arg(container_port_arg())
        .arg(container_mode_arg())
        .arg(reveal_arg())
        .combination(
            Combination::new("container-status")
                .action("container_status")
                .optional(["runtime", "name", "port", "mode", "reveal_daemon_secret"])
                .output(protocol()),
        ),
        // The runtime writes the log bytes straight through this process, so
        // they are raw output rather than protocol events.
        container_base(
            CommandSpec::new(["container", "logs"]).about("Stream the container logs, unchanged"),
        )
        .arg(ArgSpec::flag("--follow").about("Keep streaming as new lines arrive"))
        .combination(
            Combination::new("container-logs")
                .action("container_logs")
                .optional(["runtime", "name", "follow"])
                .output(passthrough()),
        ),
    ]
}

// ═══════════════════════════════════════════
// Registry access
// ═══════════════════════════════════════════

static CLI: OnceLock<Result<BuiltCliSpec, String>> = OnceLock::new();

fn cli() -> Result<&'static BuiltCliSpec, CliError> {
    CLI.get_or_init(|| build_cli_spec().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| CliError::new("cli_spec_invalid", message.clone()))
}

/// Every action the registry can resolve to. `bind_actions` checks this list
/// against the registry at startup, so a shape without a handler — or a handler
/// without a shape — fails before any argv is read.
fn action_ids() -> Vec<&'static str> {
    let mut ids = vec![
        "mode_pipe",
        "mode_interactive",
        "mode_tui",
        "mode_rpc",
        "global_limit_add",
        "global_config_get",
        "global_config_set",
        "global_backup",
        "global_restore",
        "wallet_close",
        "wallet_list",
        "wallet_show_seed",
        "limit_add",
        "config_show",
        "config_set",
        "config_token_add",
        "config_token_remove",
        "network_backup",
        "network_restore",
        "network_balance",
        "cashu_wallet_create",
        "cashu_wallet_restore",
        "cashu_send",
        "cashu_receive",
        "cashu_send_to_ln",
        "cashu_receive_from_ln",
        "cashu_receive_from_ln_claim",
        "ln_wallet_create",
        "ln_send",
        "ln_receive",
        "sol_wallet_create",
        "sol_send",
        "sol_receive",
        "evm_wallet_create",
        "evm_send",
        "evm_receive",
        "btc_wallet_create",
        "btc_send",
        "btc_receive",
        "balance",
        "history_list",
        "history_status",
        "history_update",
        "limit_list",
        "limit_remove",
        "limit_reconcile_confirm",
        "limit_reconcile_cancel",
        "skill_status",
        "skill_install",
        "skill_uninstall",
        "container_install",
        "container_uninstall",
        "container_status",
        "container_logs",
    ];
    #[cfg(feature = "rest")]
    ids.push("mode_rest");
    ids
}

type ModeHandler = fn(&ResolvedInvocation) -> Result<Mode, CliError>;

// ═══════════════════════════════════════════
// Invocation accessors
// ═══════════════════════════════════════════

fn opt_str(invocation: &ResolvedInvocation, id: &str) -> Option<String> {
    invocation
        .optional(id)
        .and_then(CliValue::as_str)
        .map(str::to_string)
}

/// A value the matched shape declares as required or fixed.
///
/// Reading it cannot fail: the shape that matched supplies every id it
/// declares. Asking for an id it does not declare is a defect in this file, not
/// a value the caller omitted.
fn req_str(invocation: &ResolvedInvocation, id: &str) -> String {
    invocation
        .required(id)
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A wallet id the caller may leave blank; the handler auto-selects for `None`.
fn wallet_of(invocation: &ResolvedInvocation) -> Option<String> {
    opt_str(invocation, "wallet").filter(|value| !value.is_empty())
}

fn strs(invocation: &ResolvedInvocation, id: &str) -> Vec<String> {
    invocation
        .repeated(id)
        .iter()
        .filter_map(CliValue::as_str)
        .map(str::to_string)
        .collect()
}

fn flag(invocation: &ResolvedInvocation, id: &str) -> bool {
    invocation
        .optional(id)
        .and_then(CliValue::as_bool)
        .unwrap_or(false)
}

/// The registry's integer type is `i64`; every count afpay carries is unsigned.
/// The bound check reports the same classification the parser would.
fn unsigned(invocation: &ResolvedInvocation, id: &str, max: u64) -> Result<Option<u64>, CliError> {
    match invocation.optional(id).and_then(CliValue::as_i64) {
        None => Ok(None),
        Some(value) if value >= 0 && (value as u64) <= max => Ok(Some(value as u64)),
        Some(_) => Err(CliError::invalid_value(format!(
            "--{} must be between 0 and {max}",
            id.replace('_', "-")
        ))),
    }
}

fn required_unsigned(invocation: &ResolvedInvocation, id: &str, max: u64) -> Result<u64, CliError> {
    unsigned(invocation, id, max)?
        .ok_or_else(|| CliError::invalid_value(format!("--{} is required", id.replace('_', "-"))))
}

fn opt_usize(invocation: &ResolvedInvocation, id: &str) -> Result<Option<usize>, CliError> {
    Ok(unsigned(invocation, id, usize::MAX as u64)?.map(|value| value as usize))
}

fn format_of(invocation: &ResolvedInvocation) -> OutputFormat {
    invocation
        .output_plan()
        .format()
        .and_then(|format| cli_parse_output(format).ok())
        .unwrap_or(OutputFormat::Json)
}

/// `--log a,b` and `--log a --log b` mean the same thing.
fn log_of(invocation: &ResolvedInvocation) -> Vec<String> {
    let entries: Vec<String> = strs(invocation, "log")
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    agent_first_data::cli_parse_log_filters(&entries)
        .as_slice()
        .to_vec()
}

fn network_of_path(invocation: &ResolvedInvocation) -> Option<Network> {
    invocation
        .command_path()
        .first()
        .map(String::as_str)
        .and_then(network_from_str)
}

fn network_from_str(name: &str) -> Option<Network> {
    match name {
        "cashu" => Some(Network::Cashu),
        "ln" => Some(Network::Ln),
        "sol" => Some(Network::Sol),
        "evm" => Some(Network::Evm),
        "btc" => Some(Network::Btc),
        _ => None,
    }
}

fn network_filter(invocation: &ResolvedInvocation) -> Option<Network> {
    opt_str(invocation, "network").and_then(|name| network_from_str(&name))
}

fn startup_requested() -> bool {
    std::env::args().any(|argument| argument == "--log")
}

fn startup_args(invocation: &ResolvedInvocation, mode: &str) -> serde_json::Value {
    serde_json::json!({
        "mode": mode,
        "output_format": invocation.output_plan().format().unwrap_or("json"),
        "output_to": invocation.output_plan().destination().unwrap_or("split"),
        "data_dir": opt_str(invocation, "data_dir"),
        "rpc_endpoint": opt_str(invocation, "rpc_endpoint"),
        "rpc_listen_address": opt_str(invocation, "rpc_listen"),
        "rest_listen_address": opt_str(invocation, "rest_listen"),
        "public_listen_enabled": flag(invocation, "public_listen"),
    })
}

// ═══════════════════════════════════════════
// Memo, window, and address helpers
// ═══════════════════════════════════════════

fn parse_memo_kv(entry: &str) -> Result<(String, String), String> {
    match entry.split_once('=') {
        Some((key, value)) => {
            if key.is_empty() {
                return Err("memo key must not be empty".into());
            }
            Ok((key.to_string(), value.to_string()))
        }
        None => Ok(("note".to_string(), entry.to_string())),
    }
}

fn local_memo_of(
    invocation: &ResolvedInvocation,
) -> Result<Option<BTreeMap<String, String>>, CliError> {
    let mut map = BTreeMap::new();
    for entry in strs(invocation, "local_memo") {
        let (key, value) = parse_memo_kv(&entry).map_err(CliError::invalid_value)?;
        map.insert(key, value);
    }
    Ok((!map.is_empty()).then_some(map))
}

fn extra_dirs_of(invocation: &ResolvedInvocation) -> Result<Vec<(String, String)>, CliError> {
    strs(invocation, "extra_dir")
        .iter()
        .map(|entry| match entry.split_once('=') {
            Some((label, path)) if !label.is_empty() && !path.is_empty() => {
                Ok((label.to_string(), path.to_string()))
            }
            _ => Err(CliError::invalid_value(format!(
                "--extra-dir expects label=/path, got: {entry}"
            ))),
        })
        .collect()
}

fn parse_window(value: &str) -> Result<u64, String> {
    let (digits, multiplier) = if let Some(rest) = value.strip_suffix('d') {
        (rest, 86400u64)
    } else if let Some(rest) = value.strip_suffix('h') {
        (rest, 3600u64)
    } else if let Some(rest) = value.strip_suffix('m') {
        (rest, 60u64)
    } else {
        return Err(format!(
            "invalid window '{value}': expected suffix m (minutes), h (hours), or d (days)"
        ));
    };
    let count: u64 = digits
        .parse()
        .map_err(|_| format!("invalid window number '{digits}'"))?;
    if count == 0 {
        return Err("window cannot be zero".to_string());
    }
    Ok(count.saturating_mul(multiplier))
}

fn validate_sol_address(to: &str) -> Result<(), String> {
    if to.starts_with("0x") {
        return Err(format!(
            "invalid Solana address '{to}': looks like an EVM address (0x prefix). \
             Solana addresses are base58-encoded"
        ));
    }
    if !(32..=44).contains(&to.len()) {
        return Err(format!(
            "invalid Solana address '{to}': expected 32-44 base58 characters, got {}",
            to.len()
        ));
    }
    if let Some(bad) = to
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() || *c == '0' || *c == 'O' || *c == 'I' || *c == 'l')
    {
        return Err(format!(
            "invalid Solana address '{to}': illegal base58 character '{bad}'"
        ));
    }
    Ok(())
}

fn validate_evm_address(to: &str) -> Result<(), String> {
    if !to.starts_with("0x") {
        return Err(format!("invalid EVM address '{to}': must start with 0x"));
    }
    let hex_part = &to[2..];
    if hex_part.len() != 40 {
        return Err(format!(
            "invalid EVM address '{to}': expected 0x + 40 hex characters, got 0x + {}",
            hex_part.len()
        ));
    }
    if !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid EVM address '{to}': contains non-hex characters"
        ));
    }
    Ok(())
}

fn validate_bolt11(to: &str) -> Result<(), String> {
    let lower = to.to_lowercase();
    if !lower.starts_with("lnbc")
        && !lower.starts_with("lntb")
        && !lower.starts_with("lnbcrt")
        && !lower.starts_with("lno1")
    {
        return Err(format!(
            "invalid Lightning invoice/offer '{to}': must start with lnbc, lntb, lnbcrt, or lno1"
        ));
    }
    Ok(())
}

fn validate_token_not_contract(token: &str) -> Result<(), String> {
    if token.starts_with("0x")
        || (token.len() > 40 && token.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        return Err(format!(
            "raw contract address not accepted for --token; register it first: \
             afpay <network> config token-add --wallet <id> --symbol <name> --address {token}"
        ));
    }
    Ok(())
}

/// Resolve rpc_endpoint/rpc_secret: CLI args take priority, then config.toml.
fn resolve_rpc_args(
    cli_endpoint: Option<String>,
    cli_secret: Option<String>,
    data_dir: Option<&str>,
) -> (Option<String>, Option<String>) {
    if cli_endpoint.is_some() {
        return (cli_endpoint, cli_secret);
    }
    let dir = data_dir
        .map(|value| value.to_string())
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let config = RuntimeConfig::load_from_dir(&dir).unwrap_or_default();
    if config.rpc_endpoint.is_some() {
        return (config.rpc_endpoint, cli_secret.or(config.rpc_secret));
    }
    (None, cli_secret)
}

// ═══════════════════════════════════════════
// Entry point
// ═══════════════════════════════════════════

pub fn parse_args() -> Result<Mode, CliError> {
    let cli = cli()?;
    let handlers = action_ids()
        .into_iter()
        .map(|id| (id, dispatch as ModeHandler));
    let app = cli
        .bind_actions(handlers)
        .map_err(|error| CliError::new("cli_actions_invalid", error.to_string()))?;

    let outcome = match app.resolve_from(std::env::args_os()) {
        Ok(outcome) => outcome,
        // Rejected before anything ran: the error names its own rule in
        // `error.code`, so there is no generic `cli_error` to branch on.
        Err(error) => {
            let event = agent_first_data::cli_error_event(&error);
            if crate::output_fmt::emit_process_event(event.into(), OutputFormat::Json).is_err() {
                std::process::exit(4);
            }
            std::process::exit(error.exit_code().into());
        }
    };

    match outcome {
        BoundOutcome::Run(invocation) => invocation.run(),
        // `--docs` renders the whole registry as raw Markdown, so it carries no
        // format of its own and never becomes a protocol event.
        BoundOutcome::Docs(_) => {
            write_or_exit(&render_cli_reference(cli));
            std::process::exit(0);
        }
        BoundOutcome::Help(help) => {
            if format_of_plan(help.output_plan()) == OutputFormat::Plain {
                write_or_exit(&help.plain());
            } else {
                emit_or_exit(
                    cli_help_event(&help).into(),
                    format_of_plan(help.output_plan()),
                );
            }
            std::process::exit(0);
        }
        BoundOutcome::Version(version) => {
            emit_or_exit(
                cli_version_event(&version).into(),
                format_of_plan(version.output_plan()),
            );
            std::process::exit(0);
        }
    }
}

fn format_of_plan(plan: &agent_first_data::OutputPlan) -> OutputFormat {
    plan.format()
        .and_then(|format| cli_parse_output(format).ok())
        .unwrap_or(OutputFormat::Json)
}

fn write_or_exit(text: &str) {
    if crate::output_fmt::write_process_result(text).is_err() {
        std::process::exit(4);
    }
}

fn emit_or_exit(event: serde_json::Value, format: OutputFormat) {
    if crate::output_fmt::emit_process_event(event, format).is_err() {
        std::process::exit(4);
    }
}

// ═══════════════════════════════════════════
// Action dispatch
// ═══════════════════════════════════════════

fn dispatch(invocation: &ResolvedInvocation) -> Result<Mode, CliError> {
    let action = invocation.action_id();
    match action {
        "mode_pipe" => Ok(Mode::Pipe(PipeInit {
            output: format_of(invocation),
            log: log_of(invocation),
            data_dir: opt_str(invocation, "data_dir"),
            startup_argv: std::env::args().collect(),
            startup_args: startup_args(invocation, "pipe"),
            startup_requested: startup_requested(),
            scrub_parse_errors: flag(invocation, "public_listen"),
        })),
        "mode_interactive" => Ok(interactive_mode(
            invocation,
            InteractiveFrontend::Interactive,
        )),
        "mode_tui" => Ok(interactive_mode(invocation, InteractiveFrontend::Tui)),
        "mode_rpc" => rpc_mode(invocation),
        #[cfg(feature = "rest")]
        "mode_rest" => Ok(Mode::Rest(RestInit {
            listen: opt_str(invocation, "rest_listen").unwrap_or_default(),
            api_key_secret: opt_str(invocation, "rest_api_key_secret"),
            allow_public_listen: flag(invocation, "public_listen"),
            log: log_of(invocation),
            data_dir: opt_str(invocation, "data_dir"),
            startup_argv: std::env::args().collect(),
            startup_args: startup_args(invocation, "rest"),
            startup_requested: startup_requested(),
        })),
        "skill_status" | "skill_install" | "skill_uninstall" => {
            Ok(Mode::SkillAdmin(SkillAdminRequest {
                action: skill_action(invocation, action),
                output: format_of(invocation),
            }))
        }
        "container_install" | "container_uninstall" | "container_status" | "container_logs" => {
            Ok(Mode::Container(ContainerRequest {
                action: container_action(invocation, action)?,
                output: format_of(invocation),
            }))
        }
        "global_backup" | "global_restore" | "network_backup" | "network_restore" => {
            Ok(Mode::Data(DataOp {
                kind: data_op_kind(invocation, action)?,
                data_dir: opt_str(invocation, "data_dir"),
                output: format_of(invocation),
            }))
        }
        _ => {
            let id = crate::store::wallet::generate_request_identifier()
                .map_err(|error| CliError::new("request_id_unavailable", error.to_string()))?;
            let input = invocation_to_input(invocation, &id)?;
            let (rpc_endpoint, rpc_secret) = resolve_rpc_args(
                opt_str(invocation, "rpc_endpoint"),
                opt_str(invocation, "rpc_secret"),
                opt_str(invocation, "data_dir").as_deref(),
            );
            Ok(Mode::Cli(Box::new(CliRequest {
                input,
                output: format_of(invocation),
                log: log_of(invocation),
                data_dir: opt_str(invocation, "data_dir"),
                rpc_endpoint,
                rpc_secret,
                startup_argv: std::env::args().collect(),
                startup_args: startup_args(invocation, "cli"),
                startup_requested: startup_requested(),
                dry_run: flag(invocation, "dry_run"),
            })))
        }
    }
}

fn interactive_mode(invocation: &ResolvedInvocation, frontend: InteractiveFrontend) -> Mode {
    let data_dir = opt_str(invocation, "data_dir");
    let (rpc_endpoint, rpc_secret) = resolve_rpc_args(
        opt_str(invocation, "rpc_endpoint"),
        opt_str(invocation, "rpc_secret"),
        data_dir.as_deref(),
    );
    Mode::Interactive(InteractiveInit {
        frontend,
        output: format_of(invocation),
        log: log_of(invocation),
        data_dir,
        rpc_endpoint,
        rpc_secret,
    })
}

fn rpc_mode(invocation: &ResolvedInvocation) -> Result<Mode, CliError> {
    #[cfg(feature = "rpc")]
    {
        Ok(Mode::Rpc(RpcInit {
            listen: opt_str(invocation, "rpc_listen").unwrap_or_default(),
            rpc_secret: opt_str(invocation, "rpc_secret"),
            allow_public_listen: flag(invocation, "public_listen"),
            log: agent_first_data::LogFilters::new(log_of(invocation)),
            data_dir: opt_str(invocation, "data_dir"),
            startup_argv: std::env::args().collect(),
            startup_args: startup_args(invocation, "rpc"),
            startup_requested: startup_requested(),
        }))
    }
    #[cfg(not(feature = "rpc"))]
    {
        let _ = invocation;
        Ok(Mode::Rpc(RpcStub))
    }
}

fn skill_action(invocation: &ResolvedInvocation, action: &str) -> SkillAdminAction {
    let options = SkillAdminOptions {
        agent: match opt_str(invocation, "agent").as_deref() {
            Some("codex") => SkillAgentSelection::Codex,
            Some("claude-code") => SkillAgentSelection::ClaudeCode,
            Some("opencode") => SkillAgentSelection::Opencode,
            Some("hermes") => SkillAgentSelection::Hermes,
            _ => SkillAgentSelection::All,
        },
        scope: match opt_str(invocation, "scope").as_deref() {
            Some("workspace") => SkillScope::Workspace,
            _ => SkillScope::Personal,
        },
        skills_dir: opt_str(invocation, "skills_dir"),
        force: flag(invocation, "force"),
    };
    match action {
        "skill_install" => SkillAdminAction::Install(options),
        "skill_uninstall" => SkillAdminAction::Uninstall(options),
        _ => SkillAdminAction::Status(options),
    }
}

fn container_common(invocation: &ResolvedInvocation) -> ContainerCommonArgs {
    ContainerCommonArgs {
        runtime: match opt_str(invocation, "runtime").as_deref() {
            Some("docker") => Some(ContainerRuntimeArg::Docker),
            Some("podman") => Some(ContainerRuntimeArg::Podman),
            Some("apple") => Some(ContainerRuntimeArg::Apple),
            _ => None,
        },
        name: opt_str(invocation, "name").unwrap_or_default(),
    }
}

fn container_mode(invocation: &ResolvedInvocation) -> ContainerModeArg {
    match opt_str(invocation, "mode").as_deref() {
        Some("rpc") => ContainerModeArg::Rpc,
        _ => ContainerModeArg::Rest,
    }
}

fn container_action(
    invocation: &ResolvedInvocation,
    action: &str,
) -> Result<ContainerCliAction, CliError> {
    Ok(match action {
        "container_install" => ContainerCliAction::Install(ContainerInstallArgs {
            common: container_common(invocation),
            port: required_unsigned(invocation, "port", u64::from(u16::MAX))? as u16,
            mode: container_mode(invocation),
            with: strs(invocation, "with"),
            allow: strs(invocation, "allow"),
            btc_network: opt_str(invocation, "btc_network").unwrap_or_default(),
            btc_rpc_port: required_unsigned(invocation, "btc_rpc_port", u64::from(u16::MAX))?
                as u16,
            btc_prune_mb: required_unsigned(invocation, "btc_prune_mb", u64::from(u32::MAX))?
                as u32,
            features: opt_str(invocation, "features"),
            rebuild: flag(invocation, "rebuild"),
            from_source: flag(invocation, "from_source"),
            context: opt_str(invocation, "context"),
            reveal_daemon_secret: flag(invocation, "reveal_daemon_secret"),
        }),
        "container_uninstall" => ContainerCliAction::Uninstall(ContainerUninstallArgs {
            common: container_common(invocation),
            purge: flag(invocation, "purge"),
        }),
        "container_status" => ContainerCliAction::Status(ContainerStatusArgs {
            common: container_common(invocation),
            port: required_unsigned(invocation, "port", u64::from(u16::MAX))? as u16,
            mode: container_mode(invocation),
            reveal_daemon_secret: flag(invocation, "reveal_daemon_secret"),
        }),
        _ => ContainerCliAction::Logs(ContainerLogsArgs {
            common: container_common(invocation),
            follow: flag(invocation, "follow"),
        }),
    })
}

fn data_op_kind(invocation: &ResolvedInvocation, action: &str) -> Result<DataOpKind, CliError> {
    Ok(match action {
        "global_backup" => DataOpKind::GlobalBackup {
            output_path: opt_str(invocation, "archive_out"),
            extra_dirs: extra_dirs_of(invocation)?,
        },
        "global_restore" => DataOpKind::GlobalRestore {
            archive_path: req_str(invocation, "archive"),
            overwrite: flag(invocation, "dangerously_overwrite"),
            pg_url_secret: opt_str(invocation, "pg_url_secret"),
            extra_dirs: extra_dirs_of(invocation)?,
        },
        "network_backup" => DataOpKind::NetworkBackup {
            network: network_of_path(invocation).unwrap_or(Network::Cashu),
            output_path: opt_str(invocation, "archive_out"),
            wallet: wallet_of(invocation),
        },
        _ => DataOpKind::NetworkRestore {
            network: network_of_path(invocation).unwrap_or(Network::Cashu),
            archive_path: req_str(invocation, "archive"),
            overwrite: flag(invocation, "dangerously_overwrite"),
            pg_url_secret: opt_str(invocation, "pg_url_secret"),
        },
    })
}

// ═══════════════════════════════════════════
// Payment requests
// ═══════════════════════════════════════════

#[derive(Default)]
struct WalletCreateParams {
    label: Option<String>,
    mint_url: Option<String>,
    rpc_endpoints: Vec<String>,
    chain_id: Option<u64>,
    mnemonic_secret: Option<String>,
    btc_esplora_url: Option<String>,
    btc_network: Option<String>,
    btc_address_type: Option<String>,
    btc_backend: Option<BtcBackend>,
    btc_core_url: Option<String>,
    btc_core_auth_secret: Option<String>,
    btc_electrum_url: Option<String>,
    sol_cluster: Option<String>,
}

fn wallet_create(id: &str, network: Network, params: WalletCreateParams) -> Input {
    Input::WalletCreate {
        id: id.to_string(),
        network,
        label: params.label,
        mint_url: params.mint_url,
        rpc_endpoints: params.rpc_endpoints,
        chain_id: params.chain_id,
        mnemonic_secret: params.mnemonic_secret,
        btc_esplora_url: params.btc_esplora_url,
        btc_network: params.btc_network,
        btc_address_type: params.btc_address_type,
        btc_backend: params.btc_backend,
        btc_core_url: params.btc_core_url,
        btc_core_auth_secret: params.btc_core_auth_secret,
        btc_electrum_url: params.btc_electrum_url,
        sol_cluster: params.sol_cluster,
    }
}

fn sats(value: u64) -> Amount {
    Amount {
        value,
        token: "sats".to_string(),
    }
}

fn limit_add_input(
    invocation: &ResolvedInvocation,
    id: &str,
    scope: SpendScope,
    network: Option<Network>,
) -> Result<Input, CliError> {
    let window_s = parse_window(&req_str(invocation, "window")).map_err(CliError::invalid_value)?;
    let max_spend = required_unsigned(invocation, "max_spend", u64::MAX)?;
    let (scope, wallet) = match (scope, wallet_of(invocation)) {
        (SpendScope::GlobalUsdCents, _) => (SpendScope::GlobalUsdCents, None),
        (_, Some(wallet)) => (SpendScope::Wallet, Some(wallet)),
        (_, None) => (SpendScope::Network, None),
    };
    Ok(Input::LimitAdd {
        id: id.to_string(),
        limit: SpendLimit {
            rule_id: None,
            scope,
            network: network.map(|network| network.to_string()),
            wallet,
            window_s,
            max_spend,
            token: opt_str(invocation, "token"),
        },
    })
}

fn receive_input(
    invocation: &ResolvedInvocation,
    id: &str,
    network: Network,
    amount: Option<Amount>,
    onchain_memo: Option<String>,
) -> Result<Input, CliError> {
    Ok(Input::Receive {
        id: id.to_string(),
        wallet: wallet_of(invocation).unwrap_or_default(),
        network: Some(network),
        amount,
        onchain_memo,
        wait_until_paid: flag(invocation, "wait"),
        wait_timeout_s: unsigned(invocation, "wait_timeout_s", u64::MAX)?,
        wait_poll_interval_ms: unsigned(invocation, "wait_poll_interval_ms", u64::MAX)?,
        wait_sync_limit: opt_usize(invocation, "wait_sync_limit")?,
        write_qr_svg_file: flag(invocation, "qr_svg_file"),
        min_confirmations: unsigned(invocation, "min_confirmations", u64::from(u32::MAX))?
            .map(|value| value as u32),
        reference: opt_str(invocation, "reference"),
    })
}

fn send_input(
    invocation: &ResolvedInvocation,
    id: &str,
    network: Network,
    to: String,
    chain_id: Option<u64>,
) -> Result<Input, CliError> {
    Ok(Input::Send {
        id: id.to_string(),
        wallet: wallet_of(invocation),
        network: Some(network),
        to,
        amount: None,
        onchain_memo: opt_str(invocation, "onchain_memo"),
        local_memo: local_memo_of(invocation)?,
        mints: None,
        chain_id,
        idempotency_key: opt_str(invocation, "idempotency_key"),
    })
}

fn invocation_to_input(invocation: &ResolvedInvocation, id: &str) -> Result<Input, CliError> {
    let owned = id.to_string();
    match invocation.action_id() {
        "wallet_close" => Ok(Input::WalletClose {
            id: owned,
            wallet: req_str(invocation, "wallet"),
            dangerously_skip_balance_check_and_may_lose_money: flag(
                invocation,
                "dangerously_skip_balance_check_and_may_lose_money",
            ),
        }),
        "wallet_list" => Ok(Input::WalletList {
            id: owned,
            network: network_of_path(invocation).or_else(|| network_filter(invocation)),
        }),
        "wallet_show_seed" => Ok(Input::WalletShowSeed {
            id: owned,
            wallet: req_str(invocation, "wallet"),
        }),
        "global_limit_add" => limit_add_input(invocation, id, SpendScope::GlobalUsdCents, None),
        "limit_add" => limit_add_input(
            invocation,
            id,
            SpendScope::Network,
            network_of_path(invocation),
        ),
        "config_show" => Ok(Input::WalletConfigShow {
            id: owned,
            wallet: req_str(invocation, "wallet"),
        }),
        "config_set" => {
            let mut endpoints = strs(invocation, "sol_rpc_endpoint");
            endpoints.extend(strs(invocation, "evm_rpc_endpoint"));
            Ok(Input::WalletConfigSet {
                id: owned,
                wallet: req_str(invocation, "wallet"),
                label: opt_str(invocation, "label"),
                rpc_endpoints: endpoints,
                chain_id: unsigned(invocation, "chain_id", u64::MAX)?,
            })
        }
        "config_token_add" => Ok(Input::WalletConfigTokenAdd {
            id: owned,
            wallet: req_str(invocation, "wallet"),
            symbol: req_str(invocation, "symbol"),
            address: req_str(invocation, "address"),
            decimals: required_unsigned(invocation, "decimals", u64::from(u8::MAX))? as u8,
        }),
        "config_token_remove" => Ok(Input::WalletConfigTokenRemove {
            id: owned,
            wallet: req_str(invocation, "wallet"),
            symbol: req_str(invocation, "symbol"),
        }),
        "network_balance" => Ok(Input::Balance {
            id: owned,
            wallet: wallet_of(invocation),
            network: network_of_path(invocation),
            check: flag(invocation, "check"),
        }),
        "balance" => Ok(Input::Balance {
            id: owned,
            wallet: wallet_of(invocation),
            network: network_filter(invocation),
            check: flag(invocation, "cashu_check"),
        }),
        "global_config_get" => Ok(Input::ConfigGet {
            id: owned,
            key: opt_str(invocation, "key"),
        }),
        "global_config_set" => Ok(Input::ConfigSet {
            id: owned,
            key: req_str(invocation, "key"),
            values: strs(invocation, "values"),
        }),
        "history_list" => Ok(Input::HistoryList {
            id: owned,
            wallet: wallet_of(invocation),
            network: network_filter(invocation),
            onchain_memo: opt_str(invocation, "onchain_memo"),
            limit: opt_usize(invocation, "limit")?,
            offset: opt_usize(invocation, "offset")?,
            since_epoch_s: unsigned(invocation, "since_epoch_s", u64::MAX)?,
            until_epoch_s: unsigned(invocation, "until_epoch_s", u64::MAX)?,
        }),
        "history_status" => Ok(Input::HistoryStatus {
            id: owned,
            transaction_id: req_str(invocation, "transaction_id"),
        }),
        "history_update" => Ok(Input::HistoryUpdate {
            id: owned,
            wallet: wallet_of(invocation),
            network: network_filter(invocation),
            limit: opt_usize(invocation, "limit")?,
        }),
        "limit_list" => Ok(Input::LimitList { id: owned }),
        "limit_remove" => Ok(Input::LimitRemove {
            id: owned,
            rule_id: req_str(invocation, "rule_id"),
        }),
        "limit_reconcile_confirm" | "limit_reconcile_cancel" => Ok(Input::ReconcileReservation {
            id: owned,
            reservation_id: required_unsigned(invocation, "reservation_id", u64::MAX)?,
            action: if invocation.action_id() == "limit_reconcile_confirm" {
                ReconcileAction::Confirm
            } else {
                ReconcileAction::Cancel
            },
            reason: req_str(invocation, "reason"),
        }),
        "cashu_wallet_create" => Ok(wallet_create(
            id,
            Network::Cashu,
            WalletCreateParams {
                label: opt_str(invocation, "label"),
                mint_url: Some(req_str(invocation, "cashu_mint")),
                mnemonic_secret: opt_str(invocation, "mnemonic_secret"),
                ..WalletCreateParams::default()
            },
        )),
        "cashu_wallet_restore" => Ok(Input::Restore {
            id: owned,
            wallet: req_str(invocation, "wallet"),
        }),
        "cashu_send" => {
            let mints = strs(invocation, "cashu_mint");
            Ok(Input::CashuSend {
                id: owned,
                wallet: wallet_of(invocation),
                amount: sats(required_unsigned(invocation, "amount_sats", u64::MAX)?),
                onchain_memo: opt_str(invocation, "onchain_memo"),
                local_memo: local_memo_of(invocation)?,
                mints: (!mints.is_empty()).then_some(mints),
                idempotency_key: opt_str(invocation, "idempotency_key"),
            })
        }
        "cashu_receive" => Ok(Input::CashuReceive {
            id: owned,
            wallet: wallet_of(invocation),
            token: req_str(invocation, "token"),
        }),
        "cashu_send_to_ln" => send_input(
            invocation,
            id,
            Network::Cashu,
            req_str(invocation, "to"),
            None,
        ),
        "cashu_receive_from_ln" => {
            let amount = unsigned(invocation, "amount_sats", u64::MAX)?.map(sats);
            receive_input(
                invocation,
                id,
                Network::Cashu,
                amount,
                opt_str(invocation, "onchain_memo"),
            )
        }
        "cashu_receive_from_ln_claim" => Ok(Input::ReceiveClaim {
            id: owned,
            wallet: req_str(invocation, "wallet"),
            quote_id: req_str(invocation, "ln_quote_id"),
        }),
        "ln_wallet_create" => {
            let backend = match opt_str(invocation, "backend").as_deref() {
                Some("phoenixd") => LnWalletBackend::Phoenixd,
                Some("lnbits") => LnWalletBackend::Lnbits,
                _ => LnWalletBackend::Nwc,
            };
            Ok(Input::LnWalletCreate {
                id: owned,
                request: LnWalletCreateRequest {
                    backend,
                    label: opt_str(invocation, "label"),
                    nwc_uri_secret: opt_str(invocation, "nwc_uri_secret"),
                    endpoint_url: opt_str(invocation, "endpoint_url"),
                    password_secret: opt_str(invocation, "password_secret"),
                    admin_key_secret: opt_str(invocation, "admin_key_secret"),
                },
            })
        }
        "ln_send" => {
            let to = req_str(invocation, "to");
            validate_bolt11(&to).map_err(CliError::invalid_value)?;
            let amount_sats = unsigned(invocation, "amount_sats", u64::MAX)?;
            // Whether the amount belongs on argv depends on the invoice's own
            // contents, which no shape can see; a BOLT11 already encodes it.
            let to = if is_bolt12_offer(&to) {
                let value = amount_sats.ok_or_else(|| {
                    CliError::invalid_value(
                        "--amount-sats is required when sending to a bolt12 offer",
                    )
                })?;
                format!("{to}?amount={value}")
            } else {
                if amount_sats.is_some() {
                    return Err(CliError::invalid_value(
                        "--amount-sats is not accepted for bolt11 invoices; the invoice encodes \
                         the amount",
                    ));
                }
                to
            };
            send_input(invocation, id, Network::Ln, to, None)
        }
        "ln_receive" => {
            let amount = unsigned(invocation, "amount_sats", u64::MAX)?.map(sats);
            receive_input(invocation, id, Network::Ln, amount, None)
        }
        "sol_wallet_create" => Ok(wallet_create(
            id,
            Network::Sol,
            WalletCreateParams {
                label: opt_str(invocation, "label"),
                rpc_endpoints: strs(invocation, "sol_rpc_endpoint"),
                sol_cluster: opt_str(invocation, "sol_cluster"),
                ..WalletCreateParams::default()
            },
        )),
        "sol_send" => {
            let to = req_str(invocation, "to");
            let token = req_str(invocation, "token");
            validate_sol_address(&to).map_err(CliError::invalid_value)?;
            validate_token_not_contract(&token).map_err(CliError::invalid_value)?;
            let amount = required_unsigned(invocation, "amount", u64::MAX)?;
            let mut target = format!("solana:{to}?amount={amount}&token={token}");
            if let Some(reference) = opt_str(invocation, "reference") {
                target.push_str(&format!("&reference={reference}"));
            }
            send_input(invocation, id, Network::Sol, target, None)
        }
        "sol_receive" => receive_input(
            invocation,
            id,
            Network::Sol,
            None,
            opt_str(invocation, "onchain_memo").filter(|memo| !memo.trim().is_empty()),
        ),
        "evm_wallet_create" => Ok(wallet_create(
            id,
            Network::Evm,
            WalletCreateParams {
                label: opt_str(invocation, "label"),
                rpc_endpoints: strs(invocation, "evm_rpc_endpoint"),
                chain_id: unsigned(invocation, "chain_id", u64::MAX)?,
                ..WalletCreateParams::default()
            },
        )),
        "evm_send" => {
            let to = req_str(invocation, "to");
            let token = req_str(invocation, "token");
            validate_evm_address(&to).map_err(CliError::invalid_value)?;
            validate_token_not_contract(&token).map_err(CliError::invalid_value)?;
            let amount = required_unsigned(invocation, "amount", u64::MAX)?;
            let target = format!("ethereum:{to}?amount={amount}&token={token}");
            let chain_id = unsigned(invocation, "chain_id", u64::MAX)?;
            send_input(invocation, id, Network::Evm, target, chain_id)
        }
        "evm_receive" => receive_input(
            invocation,
            id,
            Network::Evm,
            None,
            opt_str(invocation, "onchain_memo"),
        ),
        "btc_wallet_create" => Ok(wallet_create(
            id,
            Network::Btc,
            WalletCreateParams {
                label: opt_str(invocation, "label"),
                mnemonic_secret: opt_str(invocation, "mnemonic_secret"),
                btc_esplora_url: opt_str(invocation, "btc_esplora_url"),
                btc_network: opt_str(invocation, "btc_network"),
                btc_address_type: opt_str(invocation, "btc_address_type"),
                btc_backend: match opt_str(invocation, "btc_backend").as_deref() {
                    Some("core-rpc") => Some(BtcBackend::CoreRpc),
                    Some("electrum") => Some(BtcBackend::Electrum),
                    _ => Some(BtcBackend::Esplora),
                },
                btc_core_url: opt_str(invocation, "btc_core_url"),
                btc_core_auth_secret: opt_str(invocation, "btc_core_auth_secret"),
                btc_electrum_url: opt_str(invocation, "btc_electrum_url"),
                ..WalletCreateParams::default()
            },
        )),
        "btc_send" => {
            let to = req_str(invocation, "to");
            let amount = required_unsigned(invocation, "amount_sats", u64::MAX)?;
            send_input(
                invocation,
                id,
                Network::Btc,
                format!("bitcoin:{to}?amount={amount}"),
                None,
            )
        }
        "btc_receive" => receive_input(invocation, id, Network::Btc, None, None),
        other => Err(CliError::new(
            "cli_action_unreachable",
            format!("resolved action `{other}` has no implementation"),
        )),
    }
}

// ═══════════════════════════════════════════
// Interactive-mode reuse
// ═══════════════════════════════════════════

/// Parse an interactive-session command line, e.g.
/// `["cashu", "send", "--amount-sats", "100"]`, through the same registry the
/// process entry point uses.
#[cfg(any(feature = "interactive", test))]
pub fn parse_subcommand(args: &[&str], id: &str) -> Result<Input, String> {
    let cli = cli().map_err(|error| error.message)?;
    let mut argv = vec!["afpay".to_string()];
    argv.extend(args.iter().map(|value| (*value).to_string()));
    match cli.resolve_from(argv) {
        Ok(CliOutcome::Run(invocation)) => {
            invocation_to_input(&invocation, id).map_err(|error| error.message)
        }
        // `<cmd> --help` inside the session prints help instead of dispatching.
        Ok(CliOutcome::Help(help)) => Err(help.plain()),
        Ok(_) => Err("--version and --docs are not available inside a session".to_string()),
        Err(error) => Err(error.message),
    }
}

/// Render help for the given args, e.g. `&["--help"]` or `&["cashu", "--help"]`.
#[cfg(feature = "interactive")]
pub fn subcommand_help(args: &[&str]) -> String {
    let Ok(cli) = cli() else {
        return String::new();
    };
    let mut argv = vec!["afpay".to_string()];
    argv.extend(args.iter().map(|value| (*value).to_string()));
    match cli.resolve_from(argv) {
        Ok(CliOutcome::Help(help)) => help.plain(),
        _ => String::new(),
    }
}

/// Describes a single CLI argument, for the TUI's generated forms.
#[cfg(feature = "interactive")]
#[derive(Debug, Clone)]
pub struct ArgInfo {
    /// Long flag name without the `--` prefix, or the positional's id.
    pub long: String,
    /// The registry's description of this argument.
    pub help: String,
    /// Required by every shape of the command.
    pub required: bool,
    /// A boolean flag: presence is the value.
    pub is_flag: bool,
    /// Positional index, `None` for named arguments.
    pub positional_index: Option<usize>,
}

/// The user-facing arguments of one command path, read straight from the
/// registry. The shared runtime arguments are left out: the session already
/// owns the data dir, log filters, and transport.
#[cfg(feature = "interactive")]
pub fn subcommand_args(path: &[&str]) -> Vec<ArgInfo> {
    let Ok(cli) = cli() else {
        return Vec::new();
    };
    let wanted: Vec<String> = path.iter().map(|value| (*value).to_string()).collect();
    let Some(command) = cli
        .spec()
        .commands
        .iter()
        .find(|candidate| candidate.command_path == wanted)
    else {
        return Vec::new();
    };
    command
        .arguments
        .iter()
        .filter(|argument| !RUNTIME_IDS.contains(&argument.argument_id.as_str()))
        .map(|argument| ArgInfo {
            long: match &argument.syntax {
                ArgSyntax::Long { name } => name.trim_start_matches("--").to_string(),
                ArgSyntax::Positional { .. } => argument.argument_id.clone(),
            },
            help: argument.about.clone().unwrap_or_default(),
            required: !command.combinations.is_empty()
                && command.combinations.iter().all(|combination| {
                    combination
                        .required
                        .iter()
                        .any(|id| id == &argument.argument_id)
                }),
            is_flag: argument.value_type == ArgValueType::Flag,
            positional_index: match &argument.syntax {
                ArgSyntax::Positional { index } => Some(*index),
                ArgSyntax::Long { .. } => None,
            },
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use agent_first_data::CliErrorRule;

    fn built() -> &'static BuiltCliSpec {
        match cli() {
            Ok(cli) => cli,
            Err(error) => panic!("registry must build: {}", error.message),
        }
    }

    fn rejection(argv: &[&str]) -> agent_first_data::CliError {
        match built().resolve_from(argv.to_vec()) {
            Err(error) => error,
            Ok(_) => panic!("{argv:?} must be rejected"),
        }
    }

    #[test]
    fn registry_builds_and_every_shape_is_reachable() {
        let cli = built();
        // Each generated argv must resolve back to the shape it came from, so
        // an overlapping or unreachable combination fails here rather than at a
        // caller's first invocation.
        let synthetics = cli.synthetic_invocations();
        assert!(!synthetics.is_empty(), "the registry generated no fixtures");
        for synthetic in synthetics {
            let argv = synthetic.argv.clone();
            match cli.resolve_from(argv.clone()) {
                Ok(CliOutcome::Run(invocation)) => assert_eq!(
                    invocation.combination_id(),
                    synthetic.combination_id,
                    "{argv:?} resolved to the wrong shape"
                ),
                Ok(_) => panic!("{argv:?} did not resolve to a run"),
                Err(error) => panic!("{argv:?} failed to resolve: {}", error.message),
            }
        }
    }

    #[test]
    fn every_registered_action_has_exactly_one_handler() {
        let handlers = action_ids()
            .into_iter()
            .map(|id| (id, dispatch as ModeHandler));
        if let Err(error) = built().bind_actions(handlers) {
            panic!("action coverage must match the registry: {error}");
        }
    }

    /// Every declared shape, run through `dispatch` with strict argument reads:
    /// an arm that asks for an argument id its shape cannot supply names itself
    /// here rather than silently degrading to an empty string in production.
    ///
    /// Safe to run because `dispatch` only projects — it opens no wallet, binds
    /// no listener, and reaches no network.
    #[test]
    fn every_combination_reads_only_ids_its_shape_declares() {
        let handlers = action_ids()
            .into_iter()
            .map(|id| (id, dispatch as ModeHandler));
        let app = match built().bind_actions(handlers) {
            Ok(app) => app,
            Err(error) => panic!("action coverage must match the registry: {error}"),
        };
        app.call_every_combination();
    }

    // The old code accepted `--wait` on `evm receive` and then rejected every
    // use of it at runtime. The argument is simply not part of the command now.
    #[test]
    fn evm_receive_has_no_wait() {
        let error = rejection(&["afpay", "evm", "receive", "--wait"]);
        assert_eq!(error.rule, CliErrorRule::UnknownArgument);
    }

    #[test]
    fn ln_wallet_create_credentials_follow_the_backend() {
        let error = rejection(&[
            "afpay",
            "ln",
            "wallet",
            "create",
            "--backend",
            "nwc",
            "--endpoint-url",
            "https://phoenix.example",
        ]);
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    #[test]
    fn btc_core_rpc_backend_requires_its_url() {
        let error = rejection(&[
            "afpay",
            "btc",
            "wallet",
            "create",
            "--btc-backend",
            "core-rpc",
        ]);
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    #[test]
    fn help_is_v2_with_ready_to_run_subcommands() {
        let cli = built();
        let root = match cli.resolve_from(["afpay", "--help"]) {
            Ok(CliOutcome::Help(help)) => help,
            other => panic!("root --help must render help: {other:?}"),
        };
        assert_eq!(root.model().schema, "cli-help-v2");
        assert_eq!(root.model().command_path, "afpay");
        assert!(
            root.model()
                .subcommands
                .iter()
                .any(|entry| entry == "afpay cashu --help"),
            "subcommands must be ready-to-run strings: {:?}",
            root.model().subcommands
        );

        let scoped = match cli.resolve_from(["afpay", "cashu", "send", "--help"]) {
            Ok(CliOutcome::Help(help)) => help,
            other => panic!("scoped --help must render help: {other:?}"),
        };
        assert_eq!(scoped.model().command_path, "afpay cashu send");
        assert!(
            scoped
                .model()
                .shapes
                .iter()
                .any(|shape| shape.usage.contains("--amount-sats")),
            "every shape must be complete: {:?}",
            scoped.model().shapes
        );
        assert!(scoped.plain().contains("afpay cashu send"));

        // help-v1's second level is gone; there is nothing left to recurse into.
        assert_eq!(
            rejection(&["afpay", "--help", "--recursive"]).rule,
            CliErrorRule::UnknownArgument
        );
    }

    #[test]
    fn version_is_one_structured_result() {
        let version = match built().resolve_from(["afpay", "--version"]) {
            Ok(CliOutcome::Version(version)) => version,
            other => panic!("--version must render a version: {other:?}"),
        };
        let event = cli_version_event(&version);
        assert_eq!(event.as_value()["kind"], "result");
        assert_eq!(event.as_value()["result"]["code"], "version");
        assert_eq!(event.as_value()["result"]["name"], "afpay");
    }

    #[test]
    fn tui_session_is_its_own_shape() {
        match built().resolve_from(["afpay", "--mode", "tui"]) {
            Ok(CliOutcome::Run(invocation)) => {
                assert_eq!(invocation.combination_id(), "session-tui");
            }
            other => panic!("--mode tui must resolve: {other:?}"),
        }
    }

    #[test]
    fn parse_window_minutes() {
        assert_eq!(parse_window("30m").unwrap(), 1800);
    }

    #[test]
    fn parse_window_hours() {
        assert_eq!(parse_window("1h").unwrap(), 3600);
        assert_eq!(parse_window("24h").unwrap(), 86400);
    }

    #[test]
    fn parse_window_days() {
        assert_eq!(parse_window("7d").unwrap(), 604800);
    }

    #[test]
    fn parse_window_rejects_invalid() {
        assert!(parse_window("0h").is_err());
        assert!(parse_window("abc").is_err());
        assert!(parse_window("10s").is_err());
    }

    #[test]
    fn parse_limit_add_network_scope() {
        let input = parse_subcommand(
            &[
                "cashu",
                "limit",
                "add",
                "--window",
                "1h",
                "--max-spend",
                "10000",
            ],
            "t_limit_1",
        )
        .expect("cashu limit add should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::Network);
                assert_eq!(limit.network.as_deref(), Some("cashu"));
                assert_eq!(limit.window_s, 3600);
                assert_eq!(limit.max_spend, 10000);
                assert!(limit.token.is_none());
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_add_global_usd_cents_scope() {
        let input = parse_subcommand(
            &[
                "global",
                "limit",
                "add",
                "--window",
                "24h",
                "--max-spend",
                "50000",
            ],
            "t_limit_2",
        )
        .expect("global limit add should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::GlobalUsdCents);
                assert_eq!(limit.window_s, 86400);
                assert_eq!(limit.max_spend, 50000);
                assert!(limit.token.is_none());
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_add_network_scope_with_token() {
        let input = parse_subcommand(
            &[
                "evm",
                "limit",
                "add",
                "--token",
                "usdc",
                "--window",
                "24h",
                "--max-spend",
                "100000000",
            ],
            "t_limit_2b",
        )
        .expect("evm limit add with token should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::Network);
                assert_eq!(limit.network.as_deref(), Some("evm"));
                assert_eq!(limit.token.as_deref(), Some("usdc"));
                assert_eq!(limit.max_spend, 100000000);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // `--wallet` used to sit on the `limit` group and had to precede `add`;
    // arguments are command-local now, so it follows the whole command path.
    #[test]
    fn parse_limit_add_wallet_scope() {
        let input = parse_subcommand(
            &[
                "cashu",
                "limit",
                "add",
                "--wallet",
                "w_abc",
                "--window",
                "30m",
                "--max-spend",
                "5000",
            ],
            "t_limit_4",
        )
        .expect("cashu limit add --wallet should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::Wallet);
                assert_eq!(limit.network.as_deref(), Some("cashu"));
                assert_eq!(limit.wallet.as_deref(), Some("w_abc"));
                assert_eq!(limit.window_s, 1800);
                assert_eq!(limit.max_spend, 5000);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_remove() {
        let input = parse_subcommand(&["limit", "remove", "--rule-id", "r_1a2b3c4d"], "t_limit_3")
            .expect("limit remove should parse");
        match input {
            Input::LimitRemove { rule_id, .. } => assert_eq!(rule_id, "r_1a2b3c4d"),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_list() {
        let input =
            parse_subcommand(&["limit", "list"], "t_limit_4").expect("limit list should parse");
        assert!(matches!(input, Input::LimitList { .. }));
    }

    #[test]
    fn limit_reconcile_needs_exactly_one_outcome() {
        assert_eq!(
            rejection(&[
                "afpay",
                "limit",
                "reconcile",
                "--reservation-id",
                "7",
                "--reason",
                "manual",
            ])
            .rule,
            CliErrorRule::UnregisteredCombination
        );
        assert_eq!(
            rejection(&[
                "afpay",
                "limit",
                "reconcile",
                "--reservation-id",
                "7",
                "--confirm",
                "--cancel",
                "--reason",
                "manual",
            ])
            .rule,
            CliErrorRule::UnregisteredCombination
        );
        let input = parse_subcommand(
            &[
                "limit",
                "reconcile",
                "--reservation-id",
                "7",
                "--confirm",
                "--reason",
                "manual",
            ],
            "t_rec_1",
        )
        .expect("limit reconcile --confirm should parse");
        match input {
            Input::ReconcileReservation {
                reservation_id,
                action,
                ..
            } => {
                assert_eq!(reservation_id, 7);
                assert_eq!(action, ReconcileAction::Confirm);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_receive_wallet_optional() {
        let input = parse_subcommand(&["ln", "receive", "--amount-sats", "100"], "t_1")
            .expect("ln receive should parse without --wallet");
        match input {
            Input::Receive { wallet, amount, .. } => {
                assert_eq!(wallet, "");
                assert_eq!(amount.expect("amount").value, 100);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_receive_from_ln_wallet_optional() {
        let input = parse_subcommand(&["cashu", "receive-from-ln", "--amount-sats", "100"], "t_2")
            .expect("cashu receive-from-ln should parse without --wallet");
        match input {
            Input::Receive {
                wallet,
                network,
                amount,
                ..
            } => {
                assert_eq!(wallet, "");
                assert_eq!(network, Some(Network::Cashu));
                assert_eq!(amount.expect("amount").value, 100);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_send_mint_url() {
        let input = parse_subcommand(
            &[
                "cashu",
                "send",
                "--amount-sats",
                "100",
                "--cashu-mint",
                "https://mint-a.example",
                "--cashu-mint",
                "https://mint-b.example",
            ],
            "t_cashu_1",
        )
        .expect("cashu send --cashu-mint should parse");
        match input {
            Input::CashuSend { mints, amount, .. } => {
                assert_eq!(amount.value, 100);
                assert_eq!(
                    mints,
                    Some(vec![
                        "https://mint-a.example".to_string(),
                        "https://mint-b.example".to_string()
                    ])
                );
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_send_legacy_mint_flag_rejected() {
        let error = rejection(&[
            "afpay",
            "cashu",
            "send",
            "--amount-sats",
            "100",
            "--mint",
            "https://mint-a.example",
        ]);
        assert_eq!(error.rule, CliErrorRule::UnknownArgument);
        assert!(error.message.contains("--mint"));
    }

    // `cashu send` mints a token; it has no recipient. The hidden `--to` that
    // existed only to produce a nicer error is gone with the runtime check.
    #[test]
    fn parse_cashu_send_rejects_to() {
        let error = rejection(&[
            "afpay",
            "cashu",
            "send",
            "--amount-sats",
            "100",
            "--to",
            "lnbc1...",
        ]);
        assert_eq!(error.rule, CliErrorRule::UnknownArgument);
        assert!(error.message.contains("--to"));
    }

    #[test]
    fn parse_cashu_wallet_create_with_mnemonic_secret() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
                        abandon abandon about";
        let input = parse_subcommand(
            &[
                "cashu",
                "wallet",
                "create",
                "--cashu-mint",
                "https://mint.example",
                "--mnemonic-secret",
                mnemonic,
            ],
            "t_cashu_create_1",
        )
        .expect("cashu wallet create --mnemonic-secret should parse");
        match input {
            Input::WalletCreate {
                network,
                mint_url,
                mnemonic_secret,
                ..
            } => {
                assert_eq!(network, Network::Cashu);
                assert_eq!(mint_url.as_deref(), Some("https://mint.example"));
                assert_eq!(mnemonic_secret.as_deref(), Some(mnemonic));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_create_sol_rpc_endpoint() {
        let input = parse_subcommand(
            &[
                "sol",
                "wallet",
                "create",
                "--sol-rpc-endpoint",
                "https://api.mainnet-beta.solana.com",
            ],
            "t_sol_create_1",
        )
        .expect("sol wallet create --sol-rpc-endpoint should parse");
        match input {
            Input::WalletCreate {
                network,
                rpc_endpoints,
                mint_url,
                ..
            } => {
                assert_eq!(network, Network::Sol);
                assert!(mint_url.is_none());
                assert_eq!(rpc_endpoints, vec!["https://api.mainnet-beta.solana.com"]);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_create_multiple_sol_rpc_endpoints() {
        let input = parse_subcommand(
            &[
                "sol",
                "wallet",
                "create",
                "--sol-rpc-endpoint",
                "https://rpc-a.example",
                "--sol-rpc-endpoint",
                "https://rpc-b.example",
            ],
            "t_sol_create_3",
        )
        .expect("sol wallet create with repeated --sol-rpc-endpoint should parse");
        match input {
            Input::WalletCreate { rpc_endpoints, .. } => assert_eq!(
                rpc_endpoints,
                vec!["https://rpc-a.example", "https://rpc-b.example"]
            ),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // `--rpc-endpoint` now names the remote afpay daemon on every command, so
    // omitting `--sol-rpc-endpoint` leaves no registered shape to match.
    #[test]
    fn parse_sol_wallet_create_without_sol_rpc_endpoint_rejected() {
        let error = rejection(&[
            "afpay",
            "sol",
            "wallet",
            "create",
            "--rpc-endpoint",
            "127.0.0.1:9400",
        ]);
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    #[test]
    fn parse_sol_receive_qr_svg_file() {
        let input = parse_subcommand(
            &["sol", "receive", "--wallet", "w_12345678", "--qr-svg-file"],
            "t_sol_1",
        )
        .expect("sol receive --qr-svg-file should parse");
        match input {
            Input::Receive {
                wallet,
                network,
                write_qr_svg_file,
                ..
            } => {
                assert_eq!(wallet, "w_12345678");
                assert_eq!(network, Some(Network::Sol));
                assert!(write_qr_svg_file);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_receive_wait_with_onchain_memo() {
        let input = parse_subcommand(
            &[
                "sol",
                "receive",
                "--wallet",
                "w_12345678",
                "--onchain-memo",
                "order:ord_123",
                "--wait",
                "--wait-timeout-s",
                "15",
            ],
            "t_sol_1b",
        )
        .expect("sol receive --onchain-memo --wait should parse");
        match input {
            Input::Receive {
                onchain_memo,
                wait_until_paid,
                wait_timeout_s,
                ..
            } => {
                assert_eq!(onchain_memo.as_deref(), Some("order:ord_123"));
                assert!(wait_until_paid);
                assert_eq!(wait_timeout_s, Some(15));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // The wait knobs describe the wait, so the shape that does not wait does
    // not accept them.
    #[test]
    fn sol_receive_wait_knobs_require_wait() {
        assert_eq!(
            rejection(&["afpay", "sol", "receive", "--wait-timeout-s", "15"]).rule,
            CliErrorRule::UnregisteredCombination
        );
    }

    #[test]
    fn parse_history_list_with_onchain_memo_filter() {
        let input = parse_subcommand(
            &[
                "history",
                "list",
                "--wallet",
                "w_12345678",
                "--onchain-memo",
                "order:ord_123",
                "--limit",
                "50",
            ],
            "t_hist_1",
        )
        .expect("history list --onchain-memo should parse");
        match input {
            Input::HistoryList {
                wallet,
                onchain_memo,
                limit,
                ..
            } => {
                assert_eq!(wallet.as_deref(), Some("w_12345678"));
                assert_eq!(onchain_memo.as_deref(), Some("order:ord_123"));
                assert_eq!(limit, Some(50));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_history_update_with_scope() {
        let input = parse_subcommand(
            &[
                "history",
                "update",
                "--wallet",
                "w_12345678",
                "--network",
                "btc",
                "--limit",
                "120",
            ],
            "t_hist_up_1",
        )
        .expect("history update with scope should parse");
        match input {
            Input::HistoryUpdate {
                wallet,
                network,
                limit,
                ..
            } => {
                assert_eq!(wallet.as_deref(), Some("w_12345678"));
                assert_eq!(network, Some(Network::Btc));
                assert_eq!(limit, Some(120));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_history_update_defaults_limit() {
        let input = parse_subcommand(&["history", "update"], "t_hist_up_2")
            .expect("history update should parse");
        match input {
            Input::HistoryUpdate {
                wallet,
                network,
                limit,
                ..
            } => {
                assert_eq!(wallet, None);
                assert_eq!(network, None);
                assert_eq!(limit, Some(200));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_dangerously_show_seed() {
        let input = parse_subcommand(
            &[
                "sol",
                "wallet",
                "dangerously-show-seed",
                "--wallet",
                "w_sol",
            ],
            "t_sol_2",
        )
        .expect("sol wallet dangerously-show-seed should parse");
        match input {
            Input::WalletShowSeed { wallet, .. } => assert_eq!(wallet, "w_sol"),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_wallet_dangerously_show_seed() {
        let input = parse_subcommand(
            &["ln", "wallet", "dangerously-show-seed", "--wallet", "w_ln"],
            "t_ln_1",
        )
        .expect("ln wallet dangerously-show-seed should parse");
        match input {
            Input::WalletShowSeed { wallet, .. } => assert_eq!(wallet, "w_ln"),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_legacy_show_seed_rejected() {
        let error = rejection(&["afpay", "sol", "wallet", "show-seed", "--wallet", "w_sol"]);
        assert_eq!(error.rule, CliErrorRule::UnknownCommand);
        // The rejection does not quote the token back; what points the caller
        // at the mistake is the hint naming the command it was written under.
        assert_eq!(error.message, "unknown command");
        assert_eq!(
            error.hint,
            "run `afpay sol wallet --help` and choose one registered combination"
        );
    }

    #[test]
    fn parse_ln_send_sets_network_hint() {
        let input = parse_subcommand(
            &[
                "ln",
                "send",
                "--to",
                "lnbc1example",
                "--local-memo",
                "hello",
            ],
            "t_3",
        )
        .expect("ln send should parse");
        match input {
            Input::Send {
                network,
                local_memo,
                ..
            } => {
                assert_eq!(network, Some(Network::Ln));
                assert_eq!(
                    local_memo.and_then(|memo| memo.get("note").cloned()),
                    Some("hello".to_string())
                );
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // Lightning has no on-chain memo to carry, so the flag is absent instead of
    // accepted and then rejected.
    #[test]
    fn ln_send_has_no_onchain_memo() {
        let error = rejection(&[
            "afpay",
            "ln",
            "send",
            "--to",
            "lnbc1example",
            "--onchain-memo",
            "hi",
        ]);
        assert_eq!(error.rule, CliErrorRule::UnknownArgument);
    }

    #[test]
    fn parse_cashu_send_amount() {
        let input = parse_subcommand(&["cashu", "send", "--amount-sats", "500"], "t_unified_1")
            .expect("cashu send --amount-sats should parse");
        match input {
            Input::CashuSend { amount, .. } => {
                assert_eq!(amount.value, 500);
                assert_eq!(amount.token, "sats");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_send_token_required() {
        let input = parse_subcommand(
            &[
                "sol",
                "send",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "1000000",
                "--token",
                "native",
            ],
            "t_unified_2",
        )
        .expect("sol send --amount --token should parse");
        match input {
            Input::Send { to, .. } => {
                assert!(to.contains("amount=1000000"));
                assert!(to.contains("token=native"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_evm_send_token_required() {
        let input = parse_subcommand(
            &[
                "evm",
                "send",
                "--to",
                "0x1234567890abcdef1234567890abcdef12345678",
                "--amount",
                "1000000000",
                "--token",
                "native",
            ],
            "t_unified_3",
        )
        .expect("evm send --amount --token should parse");
        match input {
            Input::Send { to, .. } => {
                assert!(to.contains("amount=1000000000"));
                assert!(to.contains("token=native"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_receive_amount() {
        let input = parse_subcommand(&["ln", "receive", "--amount-sats", "1000"], "t_unified_4")
            .expect("ln receive --amount-sats should parse");
        match input {
            Input::Receive { amount, .. } => {
                let amount = amount.expect("amount should be set");
                assert_eq!(amount.value, 1000);
                assert_eq!(amount.token, "sats");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_receive_from_ln_claim() {
        let input = parse_subcommand(
            &[
                "cashu",
                "receive-from-ln-claim",
                "--wallet",
                "w_abc",
                "--ln-quote-id",
                "ph_456",
            ],
            "t_claim_5",
        )
        .expect("cashu receive-from-ln-claim should parse");
        match input {
            Input::ReceiveClaim {
                wallet, quote_id, ..
            } => {
                assert_eq!(wallet, "w_abc");
                assert_eq!(quote_id, "ph_456");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_wallet_restore() {
        let input = parse_subcommand(
            &["cashu", "wallet", "restore", "--wallet", "w_cashu1"],
            "t_wr_1",
        )
        .expect("cashu wallet restore should parse");
        match input {
            Input::Restore { wallet, .. } => assert_eq!(wallet, "w_cashu1"),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_balance_with_cashu_check() {
        let input = parse_subcommand(&["balance", "--cashu-check"], "t_bal_1")
            .expect("balance --cashu-check should parse");
        match input {
            Input::Balance { check, wallet, .. } => {
                assert!(check);
                assert!(wallet.is_none());
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_balance_with_wallet() {
        let input = parse_subcommand(&["balance", "--wallet", "w_abc"], "t_bal_2")
            .expect("balance --wallet should parse");
        match input {
            Input::Balance { wallet, check, .. } => {
                assert_eq!(wallet.as_deref(), Some("w_abc"));
                assert!(!check);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_receive_without_amount_for_bolt12() {
        let input = parse_subcommand(&["ln", "receive"], "t_bolt12_1")
            .expect("ln receive without --amount-sats should parse (bolt12 offer)");
        match input {
            Input::Receive {
                network, amount, ..
            } => {
                assert_eq!(network, Some(Network::Ln));
                assert!(amount.is_none(), "amount should be None for bolt12 offer");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_send_bolt12_requires_amount() {
        let error = parse_subcommand(&["ln", "send", "--to", "lno1abc123"], "t_bolt12_3")
            .expect_err("ln send to bolt12 without --amount-sats should error");
        assert!(
            error.contains("amount-sats"),
            "error should mention amount-sats: {error}"
        );
    }

    #[test]
    fn parse_ln_send_bolt12_with_amount() {
        let input = parse_subcommand(
            &["ln", "send", "--to", "lno1abc123", "--amount-sats", "500"],
            "t_bolt12_4",
        )
        .expect("ln send to bolt12 with --amount-sats should parse");
        match input {
            Input::Send { to, network, .. } => {
                assert_eq!(network, Some(Network::Ln));
                assert!(to.contains("lno1abc123"));
                assert!(to.contains("?amount=500"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_send_bolt12_case_insensitive() {
        let input = parse_subcommand(
            &[
                "ln",
                "send",
                "--to",
                "LNO1UPPERCASE",
                "--amount-sats",
                "100",
            ],
            "t_bolt12_5",
        )
        .expect("uppercase LNO1 should be accepted");
        match input {
            Input::Send { to, .. } => assert!(to.contains("?amount=100")),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_send_bolt11_rejects_amount_sats() {
        let error = parse_subcommand(
            &["ln", "send", "--to", "lnbc1abc", "--amount-sats", "100"],
            "t_bolt12_8",
        )
        .expect_err("ln send to bolt11 with --amount-sats should error");
        assert!(
            error.contains("not accepted"),
            "error should reject amount for bolt11: {error}"
        );
    }
}
