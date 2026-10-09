use super::{
    LnBackend, LnInvoiceResult, LnInvoiceStatus, LnPayResult, LnPaymentInfo, LnPaymentStatus,
    parse_bolt11_amount_sats, parse_bolt11_payment_hash,
};
use crate::provider::PayError;
use crate::types::BalanceInfo;
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

pub(crate) struct PhoenixdBackend {
    endpoint: String,
    password: String,
    client: Result<Client, String>,
}

impl PhoenixdBackend {
    fn client(&self) -> Result<&Client, PayError> {
        self.client
            .as_ref()
            .map_err(|error| PayError::internal_error(error.clone()))
    }

    pub fn new(endpoint: &str, password: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            password: password.to_string(),
            client: crate::provider::bounded_http_client(false),
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

fn payment_receipt(
    value: Option<serde_json::Value>,
    fallback_hash: Option<String>,
) -> Result<PhoenixPayResponse, PayError> {
    if let Some(value) = &value
        && value.get("reason").is_some_and(|reason| !reason.is_null())
        && value
            .get("paymentPreimage")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
    {
        let hash = value
            .get("paymentHash")
            .and_then(serde_json::Value::as_str)
            .filter(|hash| !hash.is_empty())
            .map(str::to_owned)
            .or(fallback_hash);
        return Err(PayError::submission_unknown(
            "phoenixd reports payment failure",
            hash,
        ));
    }
    // A success status alone proves nothing: a proxy or maintenance page can
    // answer 200 without phoenixd ever seeing the request. Only a body that
    // identifies the payment makes this a send.
    value
        .and_then(|value| serde_json::from_value::<PhoenixPayResponse>(value).ok())
        .filter(|receipt| {
            !receipt.payment_hash.is_empty()
                || receipt
                    .payment_preimage
                    .as_deref()
                    .is_some_and(|preimage| !preimage.is_empty())
        })
        .ok_or_else(|| {
            PayError::submission_unknown(
                "phoenixd returned a success status without a readable payment receipt",
                fallback_hash,
            )
        })
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
    #[serde(alias = "preimage")]
    payment_preimage: Option<String>,
    #[serde(default)]
    #[serde(alias = "sent")]
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
    PayError::network_error(format!("phoenixd: {e}"))
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
            .client()?
            .post(self.url("/payinvoice"))
            .basic_auth("", Some(&self.password))
            .form(&[("invoice", bolt11)])
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
                "phoenixd payinvoice {status}: {body}"
            )));
        }
        let data = payment_receipt(
            resp.json::<serde_json::Value>().await.ok(),
            Some(parse_bolt11_payment_hash(bolt11)?),
        )?;
        let mut preimage = data.payment_preimage.clone();
        let fee_msats = data.routing_fee_sat.checked_mul(1000);
        // An accepted payment cannot become a failed send merely because the
        // optional follow-up request could not be read.
        if !data.payment_hash.is_empty()
            && let Ok(detail_resp) = self
                .client()?
                .get(self.url(&format!("/payments/outgoingbyhash/{}", data.payment_hash)))
                .basic_auth("", Some(&self.password))
                .send()
                .await
            && detail_resp.status().is_success()
            && let Ok(detail) = detail_resp.json::<PhoenixOutgoingPayment>().await
        {
            if !detail.is_paid
                && preimage.as_deref().is_none_or(str::is_empty)
                && detail.payment_preimage.as_deref().is_none_or(str::is_empty)
            {
                return Err(PayError::submission_unknown(
                    "phoenixd reports an unpaid payment without a preimage",
                    Some(data.payment_hash),
                ));
            }
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
        let desc = memo.unwrap_or("afpay deposit");
        let params = vec![
            ("amountSat", amount_sats.to_string()),
            ("description", desc.to_string()),
        ];
        let resp = self
            .client()?
            .post(self.url("/createinvoice"))
            .basic_auth("", Some(&self.password))
            .form(&params)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
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
            .client()?
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
            return Err(PayError::network_error(format!(
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
            .client()?
            .get(self.url("/getbalance"))
            .basic_auth("", Some(&self.password))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
                "phoenixd getbalance {status}: {body}"
            )));
        }
        let data: PhoenixBalance = resp.json().await.map_err(map_reqwest_err)?;
        Ok(BalanceInfo::new(data.balance_sat, 0, "sats")
            .with_additional("fee_credit_sats", data.fee_credit_sat))
    }

    async fn get_default_offer(&self) -> Result<String, PayError> {
        let resp = self
            .client()?
            .get(self.url("/getoffer"))
            .basic_auth("", Some(&self.password))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
                "phoenixd getoffer {status}: {body}"
            )));
        }
        let data: PhoenixOfferResponse = resp.json().await.map_err(map_reqwest_err)?;
        if data.offer.is_empty() {
            return Err(PayError::network_error(
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
            .client()?
            .post(self.url("/payoffer"))
            .basic_auth("", Some(&self.password))
            .form(&params)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PayError::network_error(format!(
                "phoenixd payoffer {status}: {body}"
            )));
        }
        let data = payment_receipt(resp.json::<serde_json::Value>().await.ok(), None)?;
        Ok(LnPayResult {
            confirmed_amount_sats: amount_sats,
            fee_msats: data.routing_fee_sat.checked_mul(1000),
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
            .client()?
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
            .client()?
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
        payments.sort_by_key(|payment| std::cmp::Reverse(payment.created_at_epoch_s));
        payments.truncate(limit);
        Ok(payments)
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
    ) -> (PhoenixdBackend, tokio::task::JoinHandle<Vec<String>>) {
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
        (PhoenixdBackend::new(&endpoint, "test-password"), task)
    }

    #[tokio::test]
    async fn phoenixd_success_status_with_failure_reason_is_not_sent() {
        for body in [
            r#"{"paymentHash":"recovery-hash","reason":"route unavailable"}"#,
            r#"{"paymentHash":null,"reason":"route unavailable"}"#,
        ] {
            let (backend, task) = stub(vec![Some((200, body))]).await;
            let error = backend.pay_invoice(INVOICE, None).await.unwrap_err();
            assert!(matches!(error, PayError::SubmissionUnknown { .. }));
            assert!(error.submission_transaction_id().is_some());
            assert_eq!(task.await.unwrap().len(), 1);
        }
        let (backend, task) = stub(vec![Some((
            200,
            r#"{"paymentHash":null,"reason":"offer unavailable"}"#,
        ))])
        .await;
        assert!(matches!(
            backend.pay_offer("lno1fixture", 10, None).await,
            Err(PayError::SubmissionUnknown { .. })
        ));
        assert_eq!(task.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn paid_invoice_enriches_current_upstream_fields() {
        let (backend, task) = stub(vec![
            Some((200, r#"{"paymentHash":"hash"}"#)),
            Some((
                200,
                r#"{"paymentHash":"hash","isPaid":true,"preimage":"detail-preimage","sent":12}"#,
            )),
        ])
        .await;
        let result = backend.pay_invoice(INVOICE, None).await.unwrap();
        assert_eq!(result.confirmed_amount_sats, 12);
        assert_eq!(result.preimage.as_deref(), Some("detail-preimage"));
        assert_eq!(
            task.await.unwrap()[1],
            "GET /payments/outgoingbyhash/hash HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn unpaid_invoice_without_preimage_is_not_sent() {
        let (backend, task) = stub(vec![
            Some((200, r#"{"paymentHash":"recovery-hash"}"#)),
            Some((200, r#"{"paymentHash":"recovery-hash","isPaid":false}"#)),
        ])
        .await;
        let error = backend.pay_invoice(INVOICE, None).await.unwrap_err();
        assert!(matches!(error, PayError::SubmissionUnknown { .. }));
        assert_eq!(error.submission_transaction_id(), Some("recovery-hash"));
        assert_eq!(task.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn paid_invoice_survives_each_detail_failure() {
        for detail in [None, Some((503, "unavailable")), Some((200, "not json"))] {
            let (backend, task) = stub(vec![
                Some((200, r#"{"paymentHash":"hash","paymentPreimage":"receipt-preimage","routingFeeSat":2}"#)),
                detail,
            ]).await;
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                backend.pay_invoice(INVOICE, None),
            )
            .await
            .expect("payment must finish")
            .expect("detail failure cannot fail an accepted payment");
            assert_eq!(result.confirmed_amount_sats, 10);
            assert_eq!(result.fee_msats, Some(2000));
            assert_eq!(result.preimage.as_deref(), Some("receipt-preimage"));
            assert_eq!(
                task.await.unwrap(),
                [
                    "POST /payinvoice HTTP/1.1",
                    "GET /payments/outgoingbyhash/hash HTTP/1.1"
                ]
            );
        }
    }

    #[tokio::test]
    async fn invoice_success_status_without_readable_receipt_is_outcome_unknown() {
        for body in ["not json", "<html><body>Maintenance</body></html>", "{}"] {
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
            assert_eq!(task.await.unwrap(), ["POST /payinvoice HTTP/1.1"]);
        }
    }

    #[tokio::test]
    async fn offer_success_status_without_readable_receipt_is_outcome_unknown() {
        for body in ["not json", "<html><body>Maintenance</body></html>", "{}"] {
            let (backend, task) = stub(vec![Some((200, body))]).await;
            let error = backend
                .pay_offer("lno1fixture", 10, None)
                .await
                .unwrap_err();
            assert!(
                matches!(error, PayError::SubmissionUnknown { .. }),
                "{body}: {error}"
            );
            assert_eq!(task.await.unwrap(), ["POST /payoffer HTTP/1.1"]);
        }
    }

    #[tokio::test]
    async fn paid_offer_with_readable_receipt_is_sent() {
        let (backend, task) = stub(vec![Some((
            200,
            r#"{"paymentHash":"hash","paymentPreimage":"receipt-preimage","routingFeeSat":2}"#,
        ))])
        .await;
        let result = backend.pay_offer("lno1fixture", 10, None).await.unwrap();
        assert_eq!(result.confirmed_amount_sats, 10);
        assert_eq!(result.fee_msats, Some(2000));
        assert_eq!(result.preimage.as_deref(), Some("receipt-preimage"));
        assert_eq!(task.await.unwrap(), ["POST /payoffer HTTP/1.1"]);
    }
}
