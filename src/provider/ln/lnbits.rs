use super::{
    parse_bolt11_amount_sats, LnBackend, LnInvoiceResult, LnInvoiceStatus, LnPayResult,
    LnPaymentInfo, LnPaymentStatus,
};
use crate::provider::PayError;
use crate::types::BalanceInfo;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) struct LnbitsBackend {
    endpoint: String,
    admin_key: String,
    client: Client,
}

impl LnbitsBackend {
    pub fn new(endpoint: &str, admin_key: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            admin_key: admin_key.to_string(),
            client: Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint)
    }
}

// ═══════════════════════════════════════════
// LNbits API types
// ═══════════════════════════════════════════

#[derive(Serialize)]
struct LnbitsPayRequest {
    out: bool,
    bolt11: String,
}

#[derive(Serialize)]
struct LnbitsInvoiceRequest {
    out: bool,
    amount: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    memo: Option<String>,
}

#[derive(Deserialize)]
struct LnbitsPaymentResponse {
    #[serde(default)]
    payment_hash: String,
}

#[derive(Deserialize)]
struct LnbitsInvoiceResponse {
    #[serde(default)]
    payment_hash: String,
    #[serde(default)]
    payment_request: String,
}

#[derive(Deserialize)]
struct LnbitsWalletInfo {
    #[serde(default)]
    balance: i64, // msats
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct LnbitsPaymentRecord {
    #[serde(default)]
    payment_hash: String,
    #[serde(default)]
    amount: i64, // msats (negative = outgoing)
    #[serde(default)]
    pending: bool,
    #[serde(default)]
    time: u64,
    #[serde(default)]
    memo: Option<String>,
    #[serde(default)]
    fee: i64,
    #[serde(default)]
    preimage: Option<String>,
}

fn map_reqwest_err(e: reqwest::Error) -> PayError {
    PayError::NetworkError(format!("lnbits: {e}"))
}

fn msats_to_sats_ceil(msats: u64) -> u64 {
    msats.saturating_add(999) / 1000
}

fn payment_amount_msats(value: &Value) -> Option<u64> {
    value
        .get("amount")
        .and_then(Value::as_i64)
        .map(i64::unsigned_abs)
        .or_else(|| {
            value
                .get("details")
                .and_then(|v| v.get("amount"))
                .and_then(Value::as_i64)
                .map(i64::unsigned_abs)
        })
}

fn payment_fee_msats(value: &Value) -> Option<u64> {
    value
        .get("fee")
        .and_then(Value::as_i64)
        .map(i64::unsigned_abs)
        .or_else(|| {
            value
                .get("details")
                .and_then(|v| v.get("fee"))
                .and_then(Value::as_i64)
                .map(i64::unsigned_abs)
        })
}

fn payment_preimage(value: &Value) -> Option<String> {
    value
        .get("preimage")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("details")
                .and_then(|v| v.get("preimage"))
                .and_then(Value::as_str)
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

#[async_trait]
impl LnBackend for LnbitsBackend {
    async fn pay_invoice(
        &self,
        bolt11: &str,
        _amount_msats: Option<u64>,
    ) -> Result<LnPayResult, PayError> {
        let mut confirmed_amount_sats = parse_bolt11_amount_sats(bolt11)?;
        let body = LnbitsPayRequest {
            out: true,
            bolt11: bolt11.to_string(),
        };
        let resp = self
            .client
            .post(self.url("/api/v1/payments"))
            .header("X-Api-Key", &self.admin_key)
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "lnbits pay {status}: {text}"
            )));
        }
        let data: LnbitsPaymentResponse = resp.json().await.map_err(map_reqwest_err)?;
        let mut fee_msats = None;
        let mut preimage = None;
        if !data.payment_hash.is_empty() {
            let detail_resp = self
                .client
                .get(self.url(&format!("/api/v1/payments/{}", data.payment_hash)))
                .header("X-Api-Key", &self.admin_key)
                .send()
                .await
                .map_err(map_reqwest_err)?;
            if detail_resp.status().is_success() {
                let detail: Value = detail_resp.json().await.map_err(map_reqwest_err)?;
                if let Some(msats) = payment_amount_msats(&detail) {
                    confirmed_amount_sats = msats_to_sats_ceil(msats);
                }
                fee_msats = payment_fee_msats(&detail);
                preimage = payment_preimage(&detail);
            }
        }
        Ok(LnPayResult {
            confirmed_amount_sats,
            fee_msats,
            preimage,
        })
    }

    async fn create_invoice(
        &self,
        amount_sats: u64,
        memo: Option<&str>,
    ) -> Result<LnInvoiceResult, PayError> {
        let body = LnbitsInvoiceRequest {
            out: false,
            amount: amount_sats,
            memo: memo.map(|s| s.to_string()),
        };
        let resp = self
            .client
            .post(self.url("/api/v1/payments"))
            .header("X-Api-Key", &self.admin_key)
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "lnbits invoice {status}: {text}"
            )));
        }
        let data: LnbitsInvoiceResponse = resp.json().await.map_err(map_reqwest_err)?;
        Ok(LnInvoiceResult {
            bolt11: data.payment_request,
            payment_hash: data.payment_hash,
        })
    }

    async fn invoice_status(&self, payment_hash: &str) -> Result<LnInvoiceStatus, PayError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/v1/payments/{payment_hash}")))
            .header("X-Api-Key", &self.admin_key)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(LnInvoiceStatus::Unknown);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "lnbits check {status}: {text}"
            )));
        }
        let data: Value = resp.json().await.map_err(map_reqwest_err)?;
        let paid = data.get("paid").and_then(Value::as_bool).unwrap_or(false);
        if paid {
            let amount_msats = payment_amount_msats(&data).ok_or_else(|| {
                PayError::NetworkError("lnbits paid invoice missing amount".to_string())
            })?;
            Ok(LnInvoiceStatus::Paid {
                confirmed_amount_sats: msats_to_sats_ceil(amount_msats),
            })
        } else {
            Ok(LnInvoiceStatus::Pending)
        }
    }

    async fn get_balance(&self) -> Result<BalanceInfo, PayError> {
        let resp = self
            .client
            .get(self.url("/api/v1/wallet"))
            .header("X-Api-Key", &self.admin_key)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "lnbits wallet {status}: {text}"
            )));
        }
        let data: LnbitsWalletInfo = resp.json().await.map_err(map_reqwest_err)?;
        let sats = if data.balance >= 0 {
            (data.balance as u64) / 1000
        } else {
            0
        };
        Ok(BalanceInfo::new(sats, 0, "sats"))
    }

    async fn list_payments(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LnPaymentInfo>, PayError> {
        let resp = self
            .client
            .get(self.url("/api/v1/payments"))
            .header("X-Api-Key", &self.admin_key)
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())])
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "lnbits payments {status}: {text}"
            )));
        }
        let records: Vec<LnbitsPaymentRecord> = resp.json().await.map_err(map_reqwest_err)?;
        Ok(records
            .into_iter()
            .map(|r| {
                let is_outgoing = r.amount < 0;
                let abs_msats = r.amount.unsigned_abs();
                LnPaymentInfo {
                    payment_hash: r.payment_hash,
                    amount_msats: abs_msats,
                    is_outgoing,
                    status: if r.pending {
                        LnPaymentStatus::Pending
                    } else {
                        LnPaymentStatus::Paid
                    },
                    created_at_epoch_s: r.time,
                    memo: r.memo,
                    preimage: r.preimage,
                }
            })
            .collect())
    }
}
