use super::{
    parse_bolt11_amount_sats, parse_bolt11_payment_hash, LnBackend, LnInvoiceResult,
    LnInvoiceStatus, LnPayResult, LnPaymentInfo, LnPaymentStatus,
};
use crate::provider::PayError;
use crate::types::BalanceInfo;
use async_trait::async_trait;
use nostr_wallet_connect::nostr::nips::nip47::{
    ListTransactionsRequest, LookupInvoiceRequest, MakeInvoiceRequest, NostrWalletConnectURI,
    PayInvoiceRequest, TransactionState, TransactionType,
};
use nostr_wallet_connect::NWC;

pub(crate) struct NwcBackend {
    client: NWC,
}

impl NwcBackend {
    pub fn new(nwc_uri: &str) -> Result<Self, PayError> {
        let uri = NostrWalletConnectURI::parse(nwc_uri)
            .map_err(|e| PayError::InvalidAmount(format!("invalid NWC URI: {e}")))?;
        Ok(Self {
            client: NWC::new(uri),
        })
    }
}

#[async_trait]
impl LnBackend for NwcBackend {
    async fn pay_invoice(
        &self,
        bolt11: &str,
        amount_msats: Option<u64>,
    ) -> Result<LnPayResult, PayError> {
        let req = PayInvoiceRequest {
            id: None,
            invoice: bolt11.to_string(),
            amount: amount_msats,
        };
        let res = self
            .client
            .pay_invoice(req)
            .await
            .map_err(|e| PayError::NetworkError(format!("nwc pay_invoice: {e}")))?;
        let lookup_req = LookupInvoiceRequest {
            payment_hash: None,
            invoice: Some(bolt11.to_string()),
        };
        let lookup = self
            .client
            .lookup_invoice(lookup_req)
            .await
            .map_err(|e| PayError::NetworkError(format!("nwc lookup_invoice: {e}")))?;
        let confirmed_amount_sats = if lookup.amount > 0 {
            lookup.amount.saturating_add(999) / 1000
        } else {
            parse_bolt11_amount_sats(bolt11)?
        };
        Ok(LnPayResult {
            confirmed_amount_sats,
            fee_msats: res.fees_paid,
            preimage: if res.preimage.is_empty() {
                None
            } else {
                Some(res.preimage)
            },
        })
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
            .map_err(|e| PayError::NetworkError(format!("nwc make_invoice: {e}")))?;
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
            .map_err(|e| PayError::NetworkError(format!("nwc lookup_invoice: {e}")))?;
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
            .map_err(|e| PayError::NetworkError(format!("nwc get_balance: {e}")))?;
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
            .map_err(|e| PayError::NetworkError(format!("nwc list_transactions: {e}")))?;
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
