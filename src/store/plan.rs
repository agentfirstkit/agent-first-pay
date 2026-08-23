//! Reviewed payment plans: the first half of afpay's plan/confirm boundary.
//!
//! A plan is a fully resolved payment — the wallet the provider picked, the
//! amount it read out of the destination, the fee it quoted, and the spend
//! budgets it would debit — recorded under an opaque id so a caller can review
//! it and then submit *that id* to move the money. Nothing here contacts a
//! network and nothing here moves value; [`crate::handler`] resolves the plan,
//! writes it, and later claims it.
//!
//! Plans live in the workspace, next to the wallets they spend from
//! (`{data_dir}/pay-plans/`), for three reasons:
//!
//! - **Workspace binding is structural.** A plan written under one data dir
//!   cannot be found, let alone confirmed, by a daemon serving another. There
//!   is no cross-workspace id space to get wrong.
//! - **The CLI and the daemon share it.** `afpay ... send` in one process and
//!   `afpay pay confirm` in the next are the same two steps the HTTP face runs,
//!   against the same records. Neither can bypass the other.
//! - **A restart is not an approval.** The plan outlives the process that
//!   resolved it, and its [`PlanBinding`] outlives it too — so a daemon that
//!   comes back up with a different configuration refuses the plan rather than
//!   inheriting an approval for terms that have since moved.
//!
//! Claiming is a rename, which is atomic: exactly one confirm can take a plan,
//! and a second one finds it gone. That is what makes a plan single-use even
//! when two callers with different idempotency keys race for it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provider::PayError;
use crate::types::{
    Amount, Network, PayPlanOperation, RuntimeConfig, SendQuoteInfo, SpendLimitStatus,
};

const PLANS_DIR: &str = "pay-plans";
const PLAN_SUFFIX: &str = ".plan.json";
const CLAIMED_SUFFIX: &str = ".plan.claimed.json";

/// How long a resolved plan stays confirmable.
///
/// Long enough for a person to read a confirm window and answer it, short
/// enough that a plan left lying around is not a standing authorisation. A
/// fee quote is a perishable statement about the network; past this the caller
/// resolves a new one rather than paying on stale terms.
pub const PLAN_TTL_MS: u64 = 15 * 60 * 1_000;

/// The state a plan was resolved against.
///
/// §9 of the Provider OpenAPI baseline requires configuration, identity and
/// workspace changes to invalidate an outstanding plan. These four digests are
/// how afpay notices: each is recomputed at confirm time and compared, and any
/// difference refuses the plan by name rather than paying on terms nobody
/// reviewed.
///
/// Ledger *consumption* is deliberately not among them. A budget another
/// payment ate between plan and confirm is caught where it has always been
/// caught — the reservation the confirm takes before it broadcasts, which
/// refuses with `limit_exceeded`. Folding live spend into the binding would
/// make plans expire from unrelated traffic without protecting anything the
/// reserve does not already protect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBinding {
    /// The workspace this plan belongs to.
    pub workspace: String,
    /// Daemon configuration: peers, providers, allowlists, rate limits.
    pub config: String,
    /// The resolved wallet's stored metadata — endpoints, chain, mint, cluster.
    pub wallet: String,
    /// Every spend-limit rule this node enforces, by shape rather than by use.
    pub limits: String,
}

impl PlanBinding {
    /// Compute the binding for one resolved wallet in one workspace.
    pub fn resolve(
        data_dir: &str,
        config: &RuntimeConfig,
        wallet_metadata: Option<&serde_json::Value>,
        limits: &[SpendLimitStatus],
    ) -> Self {
        Self {
            workspace: digest(&serde_json::json!(canonical_workspace(data_dir))),
            config: digest(&config_fingerprint_source(config)),
            wallet: digest(&serde_json::json!(wallet_metadata)),
            limits: digest(&limits_fingerprint_source(limits)),
        }
    }

    /// Which parts moved since the plan was resolved, in the caller's terms.
    pub fn drifted_from(&self, current: &Self) -> Vec<&'static str> {
        let mut drifted = Vec::new();
        if self.workspace != current.workspace {
            drifted.push("workspace");
        }
        if self.config != current.config {
            drifted.push("configuration");
        }
        if self.wallet != current.wallet {
            drifted.push("wallet");
        }
        if self.limits != current.limits {
            drifted.push("spend_limits");
        }
        drifted
    }
}

/// A resolved payment, everything the confirm needs to execute it, and the
/// state it was resolved against.
///
/// The confirm reads the payment out of here rather than out of a request
/// body. That is the whole point of the boundary: what executes is what was
/// reviewed, byte for byte, with no second resolution that could land
/// somewhere else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayPlan {
    pub plan_id: String,
    pub operation: PayPlanOperation,
    pub network: Network,
    /// The wallet the provider picked, never the caller's unresolved hint.
    pub wallet: String,
    /// The destination, normalised exactly as the send will use it. Absent for
    /// a Cashu token mint, which has no destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The amount a Cashu token mint moves. Sends carry theirs in `quote`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onchain_memo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_memo: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mints: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<u64>,
    /// The spend-ledger scope key the debits below belong to. Lightning
    /// backends narrow it (`ln-phoenixd`), so it is resolved with the plan
    /// rather than re-derived from the network at confirm time.
    pub spend_provider_key: String,
    /// What the provider resolved: amount, fee, and the budgets this debits.
    pub quote: SendQuoteInfo,
    pub binding: PlanBinding,
    pub created_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
}

impl PayPlan {
    pub fn expired(&self, now_epoch_ms: u64) -> bool {
        now_epoch_ms >= self.expires_at_epoch_ms
    }
}

pub fn generate_plan_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| PayError::internal_error(format!("rng failed: {e}")))?;
    Ok(format!("plan_{}", hex::encode(buf)))
}

pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Write a freshly resolved plan, and drop any that have expired while we are
/// holding the directory open.
pub fn save(data_dir: &str, plan: &PayPlan) -> Result<(), PayError> {
    let root = plans_root(data_dir);
    std::fs::create_dir_all(&root)
        .map_err(|e| PayError::internal_error(format!("create {}: {e}", root.display())))?;
    set_private_directory_permissions(&root)?;
    sweep_expired(data_dir, plan.created_at_epoch_ms);

    let path = plan_path(data_dir, &plan.plan_id, PLAN_SUFFIX)?;
    let bytes = serde_json::to_vec_pretty(plan)
        .map_err(|e| PayError::internal_error(format!("serialize plan: {e}")))?;
    write_private_file(&path, &bytes)
}

/// Take the plan for execution. Atomic: the rename succeeds for exactly one
/// caller, and everyone else sees the plan as already gone.
pub fn claim(data_dir: &str, plan_id: &str) -> Result<PayPlan, PayError> {
    let available = plan_path(data_dir, plan_id, PLAN_SUFFIX)?;
    let claimed = plan_path(data_dir, plan_id, CLAIMED_SUFFIX)?;
    if !available.exists() {
        return Err(not_found(plan_id, claimed.exists()));
    }
    std::fs::rename(&available, &claimed).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            not_found(plan_id, true)
        } else {
            PayError::internal_error(format!("claim plan {plan_id}: {error}"))
        }
    })?;
    read_plan(&claimed, plan_id)
}

/// The plan was executed, or its outcome is unknown. Either way it is spent:
/// a second confirm must resolve a fresh plan rather than replay this one.
pub fn consume(data_dir: &str, plan_id: &str) {
    if let Ok(path) = plan_path(data_dir, plan_id, CLAIMED_SUFFIX) {
        let _ = std::fs::remove_file(path);
    }
}

/// The confirm refused before anything left the process. Put the plan back so
/// the caller can retry without re-reviewing terms that have not changed.
pub fn release(data_dir: &str, plan_id: &str) {
    let (Ok(claimed), Ok(available)) = (
        plan_path(data_dir, plan_id, CLAIMED_SUFFIX),
        plan_path(data_dir, plan_id, PLAN_SUFFIX),
    ) else {
        return;
    };
    let _ = std::fs::rename(claimed, available);
}

/// Read a plan without taking it. Used by the confirm to report `plan_expired`
/// and `plan_stale` before it claims anything, so a refusal never burns the
/// plan it refused.
pub fn peek(data_dir: &str, plan_id: &str) -> Result<PayPlan, PayError> {
    let available = plan_path(data_dir, plan_id, PLAN_SUFFIX)?;
    if !available.exists() {
        let claimed = plan_path(data_dir, plan_id, CLAIMED_SUFFIX)?;
        return Err(not_found(plan_id, claimed.exists()));
    }
    read_plan(&available, plan_id)
}

/// Drop plans whose window has closed. Best effort: a plan that survives a
/// sweep is still refused on read, this only keeps the directory honest.
pub fn sweep_expired(data_dir: &str, now_epoch_ms: u64) {
    let Ok(entries) = std::fs::read_dir(plans_root(data_dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(PLAN_SUFFIX) && !name.ends_with(CLAIMED_SUFFIX) {
            continue;
        }
        let expired = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PayPlan>(&bytes).ok())
            .is_some_and(|plan| plan.expired(now_epoch_ms));
        if expired {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ═══════════════════════════════════════════
// Paths and files
// ═══════════════════════════════════════════

fn plans_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join(PLANS_DIR)
}

/// Plan ids are afpay's own (`plan_` + 32 hex), but this is the boundary where
/// a caller-supplied string becomes a filename, so it is checked rather than
/// trusted. A traversal attempt is a bad id, not a path.
fn plan_path(data_dir: &str, plan_id: &str, suffix: &str) -> Result<PathBuf, PayError> {
    let valid = !plan_id.is_empty()
        && plan_id.len() <= 128
        && plan_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
    if !valid {
        return Err(PayError::invalid_request(format!(
            "plan_id '{plan_id}' is not a plan identifier afpay issued"
        )));
    }
    Ok(plans_root(data_dir).join(format!("{plan_id}{suffix}")))
}

fn read_plan(path: &Path, plan_id: &str) -> Result<PayPlan, PayError> {
    let bytes = std::fs::read(path)
        .map_err(|e| PayError::internal_error(format!("read plan {plan_id}: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| PayError::internal_error(format!("parse plan {plan_id}: {e}")))
}

fn not_found(plan_id: &str, claimed: bool) -> PayError {
    let detail = if claimed {
        "it is already being confirmed"
    } else {
        "it was confirmed, refused, or expired"
    };
    PayError::PlanNotFound {
        message: format!("no reviewable plan '{plan_id}': {detail}"),
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), PayError> {
    std::fs::write(path, bytes)
        .map_err(|e| PayError::internal_error(format!("write {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| PayError::internal_error(format!("chmod {}: {e}", path.display())))?;
    }
    Ok(())
}

fn set_private_directory_permissions(path: &Path) -> Result<(), PayError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| PayError::internal_error(format!("chmod {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

// ═══════════════════════════════════════════
// Fingerprints
// ═══════════════════════════════════════════

fn digest(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(blake3::hash(&bytes).as_bytes())
}

/// The same workspace reached by two spellings is one workspace. Resolving
/// keeps `~/.afpay` and `/Users/x/.afpay` from looking like different
/// workspaces to a plan that is perfectly valid in both.
fn canonical_workspace(data_dir: &str) -> String {
    std::fs::canonicalize(data_dir)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| data_dir.to_string())
}

/// Configuration, minus the parts that describe this invocation rather than
/// this node. `log` is a per-command flag; letting it into the fingerprint
/// would make `afpay --log pay ...` refuse a plan resolved without it.
fn config_fingerprint_source(config: &RuntimeConfig) -> serde_json::Value {
    let mut value = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("log");
    }
    value
}

/// Every rule, by its shape. `spent` / `remaining` / `window_reset_s` are the
/// ledger's live state and move on their own; a plan must not expire because
/// a window ticked over.
fn limits_fingerprint_source(limits: &[SpendLimitStatus]) -> serde_json::Value {
    let mut rules = limits
        .iter()
        .map(|limit| {
            serde_json::json!({
                "rule_id": limit.rule_id,
                "scope": limit.scope,
                "network": limit.network,
                "wallet": limit.wallet,
                "window_s": limit.window_s,
                "max_spend": limit.max_spend,
                "token": limit.token,
            })
        })
        .collect::<Vec<_>>();
    rules.sort_by_key(|rule| rule["rule_id"].as_str().unwrap_or_default().to_string());
    serde_json::Value::Array(rules)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::types::{SpendDebit, SpendScope};

    fn quote() -> SendQuoteInfo {
        SendQuoteInfo {
            wallet: "w_1".to_string(),
            amount_native: 1_000,
            fee_estimate_native: 10,
            fee_unit: "sats".to_string(),
            spend_debits: vec![SpendDebit {
                amount_native: 1_010,
                token: None,
            }],
            warnings: Vec::new(),
            upstream_plan_id: None,
        }
    }

    fn plan(plan_id: &str) -> PayPlan {
        let now = now_epoch_ms();
        PayPlan {
            plan_id: plan_id.to_string(),
            operation: PayPlanOperation::Send,
            network: Network::Ln,
            wallet: "w_1".to_string(),
            to: Some("lnbc1".to_string()),
            amount: None,
            onchain_memo: None,
            local_memo: None,
            mints: None,
            chain_id: None,
            spend_provider_key: "ln".to_string(),
            quote: quote(),
            binding: PlanBinding {
                workspace: "a".into(),
                config: "b".into(),
                wallet: "c".into(),
                limits: "d".into(),
            },
            created_at_epoch_ms: now,
            expires_at_epoch_ms: now + PLAN_TTL_MS,
        }
    }

    /// The claim is the single-use guarantee. Two confirms racing for one plan
    /// must not both get it, whatever idempotency keys they carry.
    #[test]
    fn a_plan_can_be_claimed_exactly_once() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().to_string_lossy().into_owned();
        save(&data_dir, &plan("plan_aaaa")).unwrap();

        let taken = claim(&data_dir, "plan_aaaa").expect("first claim wins");
        assert_eq!(taken.wallet, "w_1");
        let second = claim(&data_dir, "plan_aaaa").expect_err("second claim finds nothing");
        assert_eq!(second.error_code(), "plan_not_found");
    }

    #[test]
    fn a_released_plan_is_confirmable_again_and_a_consumed_one_is_not() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().to_string_lossy().into_owned();

        save(&data_dir, &plan("plan_bbbb")).unwrap();
        claim(&data_dir, "plan_bbbb").unwrap();
        release(&data_dir, "plan_bbbb");
        claim(&data_dir, "plan_bbbb").expect("a released plan is available again");
        consume(&data_dir, "plan_bbbb");
        assert!(claim(&data_dir, "plan_bbbb").is_err());
    }

    #[test]
    fn an_expired_plan_is_swept_and_never_returned() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().to_string_lossy().into_owned();
        let mut stale = plan("plan_cccc");
        stale.expires_at_epoch_ms = stale.created_at_epoch_ms;
        save(&data_dir, &stale).unwrap();
        assert!(
            peek(&data_dir, "plan_cccc")
                .unwrap()
                .expired(now_epoch_ms())
        );
        sweep_expired(&data_dir, now_epoch_ms() + 1);
        assert!(peek(&data_dir, "plan_cccc").is_err());
    }

    /// A plan id is a filename here, so the one thing it must never be is a
    /// path.
    #[test]
    fn a_plan_id_cannot_climb_out_of_the_plan_directory() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().to_string_lossy().into_owned();
        for id in ["../config", "a/b", "", "plan with space"] {
            assert!(peek(&data_dir, id).is_err(), "{id}");
        }
    }

    #[test]
    fn the_binding_names_what_moved_and_ignores_a_per_command_log_flag() {
        let limits = vec![SpendLimitStatus {
            rule_id: "r_1".into(),
            scope: SpendScope::Network,
            network: Some("ln".into()),
            wallet: None,
            window_s: 3600,
            max_spend: 1000,
            spent: 0,
            remaining: 1000,
            token: None,
            window_reset_s: 3600,
        }];
        let config = RuntimeConfig::default();
        let resolved = PlanBinding::resolve("/tmp", &config, None, &limits);

        let mut logged = config.clone();
        logged.log = vec!["pay".to_string()];
        assert!(
            resolved
                .drifted_from(&PlanBinding::resolve("/tmp", &logged, None, &limits))
                .is_empty(),
            "a --log flag is not a configuration change"
        );

        let mut peered = config.clone();
        peered
            .providers
            .insert("ln".to_string(), "elsewhere".to_string());
        assert_eq!(
            resolved.drifted_from(&PlanBinding::resolve("/tmp", &peered, None, &limits)),
            vec!["configuration"]
        );

        let mut spent = limits.clone();
        spent[0].spent = 900;
        spent[0].remaining = 100;
        assert!(
            resolved
                .drifted_from(&PlanBinding::resolve("/tmp", &config, None, &spent))
                .is_empty(),
            "consumption is the reservation's job, not the binding's"
        );

        let mut tightened = limits.clone();
        tightened[0].max_spend = 10;
        assert_eq!(
            resolved.drifted_from(&PlanBinding::resolve("/tmp", &config, None, &tightened)),
            vec!["spend_limits"]
        );

        assert_eq!(
            resolved.drifted_from(&PlanBinding::resolve(
                "/tmp",
                &config,
                Some(&serde_json::json!({"id": "w_1"})),
                &limits
            )),
            vec!["wallet"]
        );
    }
}
