use super::{
    parse_bolt11_amount_sats, LnBackend, LnInvoiceResult, LnInvoiceStatus, LnPayResult,
    LnPaymentInfo, LnPaymentStatus,
};
use crate::provider::PayError;
use crate::types::BalanceInfo;
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

pub(crate) struct PhoenixdBackend {
    endpoint: String,
    password: String,
    client: Client,
}

impl PhoenixdBackend {
    pub fn new(endpoint: &str, password: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            password: password.to_string(),
            client: Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint)
    }
}

// ═══════════════════════════════════════════
// phoenixd API response types
// ═══════════════════════════════════════════

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoenixPayResponse {
    #[serde(default)]
    payment_hash: String,
    #[serde(default)]
    payment_preimage: Option<String>,
    #[serde(default)]
    routing_fee_sat: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoenixInvoiceResponse {
    #[serde(default)]
    serialized: String,
    #[serde(default)]
    payment_hash: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PhoenixPaymentInfo {
    #[serde(default)]
    payment_hash: String,
    #[serde(default)]
    payment_preimage: Option<String>,
    #[serde(default, alias = "receivedSat")]
    received_sat: u64,
    #[serde(default)]
    fees: u64,
    #[serde(default)]
    is_paid: bool,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PhoenixOutgoingPayment {
    #[serde(default)]
    payment_hash: String,
    #[serde(default)]
    payment_preimage: Option<String>,
    #[serde(default)]
    sent_sat: u64,
    #[serde(default)]
    fees_sat: u64,
    #[serde(default)]
    is_paid: bool,
    #[serde(default)]
    created_at: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoenixBalance {
    #[serde(default)]
    balance_sat: u64,
    #[serde(default)]
    fee_credit_sat: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoenixOfferResponse {
    #[serde(default)]
    offer: String,
}

fn map_reqwest_err(e: reqwest::Error) -> PayError {
    PayError::NetworkError(format!("phoenixd: {e}"))
}

#[async_trait]
impl LnBackend for PhoenixdBackend {
    async fn pay_invoice(
        &self,
        bolt11: &str,
        _amount_msats: Option<u64>,
    ) -> Result<LnPayResult, PayError> {
        let mut confirmed_amount_sats = parse_bolt11_amount_sats(bolt11)?;
        let resp = self
            .client
            .post(self.url("/payinvoice"))
            .basic_auth("", Some(&self.password))
            .form(&[("invoice", bolt11)])
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "phoenixd payinvoice {status}: {body}"
            )));
        }
        let data: PhoenixPayResponse = resp.json().await.map_err(map_reqwest_err)?;
        let mut preimage = data.payment_preimage.clone();
        if !data.payment_hash.is_empty() {
            let detail_resp = self
                .client
                .get(self.url(&format!("/payments/outgoing/{}", data.payment_hash)))
                .basic_auth("", Some(&self.password))
                .send()
                .await
                .map_err(map_reqwest_err)?;
            if detail_resp.status().is_success() {
                let detail: PhoenixOutgoingPayment =
                    detail_resp.json().await.map_err(map_reqwest_err)?;
                if detail.sent_sat > 0 {
                    confirmed_amount_sats = detail.sent_sat;
                }
                if detail
                    .payment_preimage
                    .as_deref()
                    .is_some_and(|v| !v.is_empty())
                {
                    preimage = detail.payment_preimage;
                }
            }
        }
        Ok(LnPayResult {
            confirmed_amount_sats,
            fee_msats: Some(data.routing_fee_sat * 1000),
            preimage,
        })
    }

    async fn create_invoice(
        &self,
        amount_sats: u64,
        memo: Option<&str>,
    ) -> Result<LnInvoiceResult, PayError> {
        let desc = memo.unwrap_or("afpay deposit");
        let params = vec![
            ("amountSat", amount_sats.to_string()),
            ("description", desc.to_string()),
        ];
        let resp = self
            .client
            .post(self.url("/createinvoice"))
            .basic_auth("", Some(&self.password))
            .form(&params)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "phoenixd createinvoice {status}: {body}"
            )));
        }
        let data: PhoenixInvoiceResponse = resp.json().await.map_err(map_reqwest_err)?;
        Ok(LnInvoiceResult {
            bolt11: data.serialized,
            payment_hash: data.payment_hash,
        })
    }

    async fn invoice_status(&self, payment_hash: &str) -> Result<LnInvoiceStatus, PayError> {
        let resp = self
            .client
            .get(self.url(&format!("/payments/incoming/{payment_hash}")))
            .basic_auth("", Some(&self.password))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(LnInvoiceStatus::Unknown);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "phoenixd check {status}: {body}"
            )));
        }
        let data: PhoenixPaymentInfo = resp.json().await.map_err(map_reqwest_err)?;
        Ok(if data.is_paid {
            LnInvoiceStatus::Paid {
                confirmed_amount_sats: data.received_sat,
            }
        } else {
            LnInvoiceStatus::Pending
        })
    }

    async fn get_balance(&self) -> Result<BalanceInfo, PayError> {
        let resp = self
            .client
            .get(self.url("/getbalance"))
            .basic_auth("", Some(&self.password))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "phoenixd getbalance {status}: {body}"
            )));
        }
        let data: PhoenixBalance = resp.json().await.map_err(map_reqwest_err)?;
        Ok(BalanceInfo::new(data.balance_sat, 0, "sats")
            .with_additional("fee_credit_sats", data.fee_credit_sat))
    }

    async fn get_default_offer(&self) -> Result<String, PayError> {
        let resp = self
            .client
            .get(self.url("/getoffer"))
            .basic_auth("", Some(&self.password))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "phoenixd getoffer {status}: {body}"
            )));
        }
        let data: PhoenixOfferResponse = resp.json().await.map_err(map_reqwest_err)?;
        if data.offer.is_empty() {
            return Err(PayError::NetworkError(
                "phoenixd returned empty offer".to_string(),
            ));
        }
        Ok(data.offer)
    }

    async fn pay_offer(
        &self,
        offer: &str,
        amount_sats: u64,
        message: Option<&str>,
    ) -> Result<LnPayResult, PayError> {
        let mut params = vec![
            ("offer".to_string(), offer.to_string()),
            ("amountSat".to_string(), amount_sats.to_string()),
        ];
        if let Some(msg) = message {
            params.push(("message".to_string(), msg.to_string()));
        }
        let resp = self
            .client
            .post(self.url("/payoffer"))
            .basic_auth("", Some(&self.password))
            .form(&params)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::NetworkError(format!(
                "phoenixd payoffer {status}: {body}"
            )));
        }
        let data: PhoenixPayResponse = resp.json().await.map_err(map_reqwest_err)?;
        Ok(LnPayResult {
            confirmed_amount_sats: amount_sats,
            fee_msats: Some(data.routing_fee_sat * 1000),
            preimage: data.payment_preimage,
        })
    }

    async fn list_payments(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LnPaymentInfo>, PayError> {
        // Fetch incoming
        let incoming_resp = self
            .client
            .get(self.url("/payments/incoming"))
            .basic_auth("", Some(&self.password))
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())])
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let incoming: Vec<PhoenixPaymentInfo> = if incoming_resp.status().is_success() {
            incoming_resp.json().await.map_err(map_reqwest_err)?
        } else {
            vec![]
        };

        // Fetch outgoing
        let outgoing_resp = self
            .client
            .get(self.url("/payments/outgoing"))
            .basic_auth("", Some(&self.password))
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())])
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let outgoing: Vec<PhoenixOutgoingPayment> = if outgoing_resp.status().is_success() {
            outgoing_resp.json().await.map_err(map_reqwest_err)?
        } else {
            vec![]
        };

        let mut payments: Vec<LnPaymentInfo> = Vec::new();
        for p in incoming {
            payments.push(LnPaymentInfo {
                payment_hash: p.payment_hash,
                amount_msats: p.received_sat * 1000,
                is_outgoing: false,
                status: if p.is_paid {
                    LnPaymentStatus::Paid
                } else {
                    LnPaymentStatus::Pending
                },
                created_at_epoch_s: p.created_at / 1000, // phoenixd uses ms
                memo: p.description,
                preimage: p.payment_preimage,
            });
        }
        for p in outgoing {
            payments.push(LnPaymentInfo {
                payment_hash: p.payment_hash,
                amount_msats: p.sent_sat * 1000,
                is_outgoing: true,
                status: if p.is_paid {
                    LnPaymentStatus::Paid
                } else {
                    LnPaymentStatus::Pending
                },
                created_at_epoch_s: p.created_at / 1000,
                memo: None,
                preimage: p.payment_preimage,
            });
        }
        payments.sort_by(|a, b| b.created_at_epoch_s.cmp(&a.created_at_epoch_s));
        payments.truncate(limit);
        Ok(payments)
    }
}
