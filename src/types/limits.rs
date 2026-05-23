use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendDebit {
    pub amount_native: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendScope {
    #[serde(alias = "all")]
    GlobalUsdCents,
    Network,
    Wallet,
}

fn default_spend_scope_network() -> SpendScope {
    SpendScope::Network
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendLimit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default = "default_spend_scope_network")]
    pub scope: SpendScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet: Option<String>,
    pub window_s: u64,
    pub max_spend: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendLimitStatus {
    pub rule_id: String,
    pub scope: SpendScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet: Option<String>,
    pub window_s: u64,
    pub max_spend: u64,
    pub spent: u64,
    pub remaining: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub window_reset_s: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamLimitNode {
    pub name: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<SpendLimitStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downstream: Vec<DownstreamLimitNode>,
}
