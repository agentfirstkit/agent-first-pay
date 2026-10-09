use super::{
    LnBackend, LnInvoiceResult, LnInvoiceStatus, LnPayResult, LnPaymentInfo, LnPaymentStatus,
    parse_bolt11_amount_sats, parse_bolt11_payment_hash,
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
    client: Result<Client, String>,
}

impl LnbitsBackend {
    fn client(&self) -> Result<&Client, PayError> {
        self.client
            .as_ref()
            .map_err(|error| PayError::internal_error(error.clone()))
    }

    pub fn new(endpoint: &str, admin_key: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            admin_key: admin_key.to_string(),
            client: crate::provider::bounded_http_client(false),
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
    PayError::network_error(format!("lnbits: {e}"))
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
            .client()?
            .post(self.url("/api/v1/payments"))
            .header("X-Api-Key", &self.admin_key)
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
                "lnbits pay {status}: {text}"
            )));
        }
        let data = resp.json::<Value>().await.ok();
        let mut fee_msats = data.as_ref().and_then(payment_fee_msats);
        let mut preimage = data.as_ref().and_then(payment_preimage);
        let hash = data
            .as_ref()
            .and_then(|value| value.get("payment_hash"))
            .and_then(Value::as_str)
            .filter(|hash| !hash.is_empty());
        // A success status alone proves nothing: a proxy or maintenance page can
        // answer 200 without LNbits ever seeing the request. Only a body that
        // identifies the payment makes this a send.
        if hash.is_none() && preimage.is_none() {
            return Err(PayError::submission_unknown(
                "lnbits returned a success status without a readable payment receipt",
                Some(parse_bolt11_payment_hash(bolt11)?),
            ));
        }
        // The payment is accepted; optional receipt enrichment cannot undo it.
        if let Some(hash) = hash
            && let Ok(detail_resp) = self
                .client()?
                .get(self.url(&format!("/api/v1/payments/{hash}")))
                .header("X-Api-Key", &self.admin_key)
                .send()
                .await
            && detail_resp.status().is_success()
            && let Ok(detail) = detail_resp.json::<Value>().await
        {
            if let Some(msats) = payment_amount_msats(&detail) {
                confirmed_amount_sats = msats_to_sats_ceil(msats);
            }
            fee_msats = payment_fee_msats(&detail).or(fee_msats);
            preimage = payment_preimage(&detail).or(preimage);
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
            .client()?
            .post(self.url("/api/v1/payments"))
            .header("X-Api-Key", &self.admin_key)
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
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
            .client()?
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
            return Err(PayError::network_error(format!(
                "lnbits check {status}: {text}"
            )));
        }
        let data: Value = resp.json().await.map_err(map_reqwest_err)?;
        let paid = data.get("paid").and_then(Value::as_bool).unwrap_or(false);
        if paid {
            let amount_msats = payment_amount_msats(&data).ok_or_else(|| {
                PayError::network_error("lnbits paid invoice missing amount".to_string())
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
            .client()?
            .get(self.url("/api/v1/wallet"))
            .header("X-Api-Key", &self.admin_key)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
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
            .client()?
            .get(self.url("/api/v1/payments"))
            .header("X-Api-Key", &self.admin_key)
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())])
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const INVOICE: &str = "lnbc100n1p5z3a63pp56854ytysg7e5z9fl3w5mgvrlqjfcytnjv8ff5hm5qt6gl6alxesqdqqcqzzsxqyz5vqsp5p0x0dlhn27s63j4emxnk26p7f94u0lyarnfp5yqmac9gzy4ngdss9qxpqysgqne3v0hnzt2lp0hc69xpzckk0cdcar7glvjhq60lsrfe8gejdm8c564prrnsft6ctxxyrewp4jtezrq3gxxqnfjj0f9tw2qs9y0lslmqpfu7et9";
    async fn stub(
        responses: Vec<Option<(u16, &'static str)>>,
    ) -> (LnbitsBackend, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut paths = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                let header_end = loop {
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0, "request closed before headers");
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(pos) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                paths.push(headers.lines().next().unwrap().to_string());
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0, "request closed before body");
                    request.extend_from_slice(&chunk[..count]);
                }
                if let Some((status, body)) = response {
                    let response = format!(
                        "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            }
            paths
        });
        (LnbitsBackend::new(&endpoint, "test-admin-key"), task)
    }

    #[tokio::test]
    async fn paid_invoice_survives_lnbits_detail_failure() {
        for detail in [None, Some((503, "unavailable")), Some((200, "not json"))] {
            let (backend, task) = stub(vec![
                Some((
                    200,
                    r#"{"payment_hash":"hash","preimage":"receipt-preimage","fee":-2000}"#,
                )),
                detail,
            ])
            .await;
            let result = backend.pay_invoice(INVOICE, None).await.unwrap();
            assert_eq!(result.confirmed_amount_sats, 10);
            assert_eq!(result.preimage.as_deref(), Some("receipt-preimage"));
            assert_eq!(result.fee_msats, Some(2000));
            assert_eq!(
                task.await.unwrap(),
                [
                    "POST /api/v1/payments HTTP/1.1",
                    "GET /api/v1/payments/hash HTTP/1.1"
                ]
            );
        }
    }
    #[tokio::test]
    async fn lnbits_success_status_without_readable_receipt_is_outcome_unknown() {
        for body in [
            "not json",
            "<html><body>Maintenance</body></html>",
            "{}",
            r#"{"payment_hash":""}"#,
        ] {
            let (backend, task) = stub(vec![Some((200, body))]).await;
            let error = backend.pay_invoice(INVOICE, None).await.unwrap_err();
            assert!(
                matches!(error, PayError::SubmissionUnknown { .. }),
                "{body}: {error}"
            );
            assert_eq!(
                error.submission_transaction_id(),
                Some(parse_bolt11_payment_hash(INVOICE).unwrap().as_str())
            );
            assert_eq!(task.await.unwrap(), ["POST /api/v1/payments HTTP/1.1"]);
        }
    }

    #[tokio::test]
    async fn paid_invoice_with_hash_only_survives_lnbits_detail_failure() {
        let (backend, task) = stub(vec![
            Some((200, r#"{"payment_hash":"hash"}"#)),
            Some((503, "unavailable")),
        ])
        .await;
        let result = backend.pay_invoice(INVOICE, None).await.unwrap();
        assert_eq!(result.confirmed_amount_sats, 10);
        assert_eq!(result.preimage, None);
        assert_eq!(task.await.unwrap().len(), 2);
    }
}
