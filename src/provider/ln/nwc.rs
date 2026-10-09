use super::{
    LnBackend, LnInvoiceResult, LnInvoiceStatus, LnPayResult, LnPaymentInfo, LnPaymentStatus,
    parse_bolt11_amount_sats, parse_bolt11_payment_hash,
};
use crate::provider::PayError;
use crate::types::BalanceInfo;
use async_trait::async_trait;
use nostr_wallet_connect::NWC;
use nostr_wallet_connect::nostr::nips::nip47::{
    ListTransactionsRequest, LookupInvoiceRequest, LookupInvoiceResponse, MakeInvoiceRequest,
    NostrWalletConnectURI, PayInvoiceRequest, PayInvoiceResponse, TransactionState,
    TransactionType,
};

pub(crate) struct NwcBackend {
    client: NWC,
}

impl NwcBackend {
    pub fn new(nwc_uri: &str) -> Result<Self, PayError> {
        let uri = NostrWalletConnectURI::parse(nwc_uri)
            .map_err(|e| PayError::invalid_amount(format!("invalid NWC URI: {e}")))?;
        Ok(Self {
            client: NWC::new(uri),
        })
    }
}

// Only the irreversible payment and its optional enrichment share this seam.
#[async_trait]
trait NwcInvoiceClient: Sync {
    async fn pay(&self, request: PayInvoiceRequest) -> Result<PayInvoiceResponse, PayError>;
    async fn lookup(
        &self,
        request: LookupInvoiceRequest,
    ) -> Result<LookupInvoiceResponse, PayError>;
}

#[async_trait]
impl NwcInvoiceClient for NWC {
    async fn pay(&self, request: PayInvoiceRequest) -> Result<PayInvoiceResponse, PayError> {
        self.pay_invoice(request)
            .await
            .map_err(|error| PayError::network_error(format!("nwc pay_invoice: {error}")))
    }
    async fn lookup(
        &self,
        request: LookupInvoiceRequest,
    ) -> Result<LookupInvoiceResponse, PayError> {
        self.lookup_invoice(request)
            .await
            .map_err(|error| PayError::network_error(format!("nwc lookup_invoice: {error}")))
    }
}

async fn pay_invoice_with_client(
    client: &impl NwcInvoiceClient,
    bolt11: &str,
    amount_msats: Option<u64>,
) -> Result<LnPayResult, PayError> {
    let fallback_amount_sats = parse_bolt11_amount_sats(bolt11)?;
    let paid = client
        .pay(PayInvoiceRequest {
            id: None,
            invoice: bolt11.to_owned(),
            amount: amount_msats,
        })
        .await?;
    let lookup = client
        .lookup(LookupInvoiceRequest {
            payment_hash: None,
            invoice: Some(bolt11.to_owned()),
        })
        .await
        .ok();
    let confirmed_amount_sats = lookup
        .filter(|lookup| lookup.amount > 0)
        .map(|lookup| lookup.amount.saturating_add(999) / 1000)
        .unwrap_or(fallback_amount_sats);
    Ok(LnPayResult {
        confirmed_amount_sats,
        fee_msats: paid.fees_paid,
        preimage: if paid.preimage.is_empty() {
            None
        } else {
            Some(paid.preimage)
        },
    })
}

#[async_trait]
impl LnBackend for NwcBackend {
    async fn pay_invoice(
        &self,
        bolt11: &str,
        amount_msats: Option<u64>,
    ) -> Result<LnPayResult, PayError> {
        pay_invoice_with_client(&self.client, bolt11, amount_msats).await
    }

    async fn create_invoice(
        &self,
        amount_sats: u64,
        memo: Option<&str>,
    ) -> Result<LnInvoiceResult, PayError> {
        let req = MakeInvoiceRequest {
            amount: amount_sats * 1000, // NWC uses msats
            description: memo.map(|s| s.to_string()),
            description_hash: None,
            expiry: None,
        };
        let res = self
            .client
            .make_invoice(req)
            .await
            .map_err(|e| PayError::network_error(format!("nwc make_invoice: {e}")))?;
        let payment_hash = res
            .payment_hash
            .filter(|s| !s.is_empty())
            .or_else(|| parse_bolt11_payment_hash(&res.invoice).ok())
            .unwrap_or_default();
        Ok(LnInvoiceResult {
            bolt11: res.invoice,
            payment_hash,
        })
    }

    async fn invoice_status(&self, payment_hash: &str) -> Result<LnInvoiceStatus, PayError> {
        let req = LookupInvoiceRequest {
            payment_hash: Some(payment_hash.to_string()),
            invoice: None,
        };
        let res = self
            .client
            .lookup_invoice(req)
            .await
            .map_err(|e| PayError::network_error(format!("nwc lookup_invoice: {e}")))?;
        Ok(match res.state {
            Some(TransactionState::Settled) => LnInvoiceStatus::Paid {
                confirmed_amount_sats: res.amount.saturating_add(999) / 1000,
            },
            Some(TransactionState::Pending) => LnInvoiceStatus::Pending,
            Some(TransactionState::Failed) => LnInvoiceStatus::Failed,
            Some(TransactionState::Expired) => LnInvoiceStatus::Failed,
            None => LnInvoiceStatus::Unknown,
        })
    }

    async fn get_balance(&self) -> Result<BalanceInfo, PayError> {
        let balance_msats = self
            .client
            .get_balance()
            .await
            .map_err(|e| PayError::network_error(format!("nwc get_balance: {e}")))?;
        Ok(BalanceInfo::new(balance_msats / 1000, 0, "sats"))
    }

    async fn list_payments(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LnPaymentInfo>, PayError> {
        let req = ListTransactionsRequest {
            from: None,
            until: None,
            limit: Some(limit as u64),
            offset: Some(offset as u64),
            unpaid: Some(false),
            transaction_type: None,
        };
        let txs = self
            .client
            .list_transactions(req)
            .await
            .map_err(|e| PayError::network_error(format!("nwc list_transactions: {e}")))?;
        Ok(txs
            .into_iter()
            .map(|t| LnPaymentInfo {
                payment_hash: t.payment_hash,
                amount_msats: t.amount,
                is_outgoing: t.transaction_type == Some(TransactionType::Outgoing),
                status: match t.state {
                    Some(TransactionState::Settled) => LnPaymentStatus::Paid,
                    Some(TransactionState::Pending) => LnPaymentStatus::Pending,
                    Some(TransactionState::Failed) => LnPaymentStatus::Failed,
                    Some(TransactionState::Expired) => LnPaymentStatus::Failed,
                    None => LnPaymentStatus::Unknown,
                },
                created_at_epoch_s: t.created_at.as_secs(),
                memo: t.description,
                preimage: t.preimage,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct PaidWithoutLookup(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl NwcInvoiceClient for PaidWithoutLookup {
        async fn pay(&self, _: PayInvoiceRequest) -> Result<PayInvoiceResponse, PayError> {
            assert_eq!(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0);
            Ok(PayInvoiceResponse {
                preimage: "receipt-preimage".into(),
                fees_paid: Some(2000),
            })
        }
        async fn lookup(&self, _: LookupInvoiceRequest) -> Result<LookupInvoiceResponse, PayError> {
            assert_eq!(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 1);
            Err(PayError::network_error("lookup connection lost"))
        }
    }
    #[tokio::test]
    async fn paid_invoice_survives_nwc_lookup_failure() {
        let client = PaidWithoutLookup(std::sync::atomic::AtomicUsize::new(0));
        let result = pay_invoice_with_client(&client, "lnbc100n1p5z3a63pp56854ytysg7e5z9fl3w5mgvrlqjfcytnjv8ff5hm5qt6gl6alxesqdqqcqzzsxqyz5vqsp5p0x0dlhn27s63j4emxnk26p7f94u0lyarnfp5yqmac9gzy4ngdss9qxpqysgqne3v0hnzt2lp0hc69xpzckk0cdcar7glvjhq60lsrfe8gejdm8c564prrnsft6ctxxyrewp4jtezrq3gxxqnfjj0f9tw2qs9y0lslmqpfu7et9", None).await.unwrap();
        assert_eq!(result.confirmed_amount_sats, 10);
        assert_eq!(result.preimage.as_deref(), Some("receipt-preimage"));
        assert_eq!(result.fee_msats, Some(2000));
        assert_eq!(client.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
