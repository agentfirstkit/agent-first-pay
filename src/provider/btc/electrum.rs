use super::BtcChainSource;
use crate::provider::PayError;
use crate::store::wallet::WalletMetadata;
use async_trait::async_trait;
use bdk_electrum::electrum_client;
use bdk_electrum::electrum_client::ElectrumApi;
use bdk_electrum::BdkElectrumClient;
use bdk_wallet::bitcoin::Transaction;
use bdk_wallet::Wallet;

const STOP_GAP: usize = 20;
const BATCH_SIZE: usize = 10;

pub(crate) struct ElectrumSource {
    url: String,
}

impl ElectrumSource {
    pub fn new(meta: &WalletMetadata) -> Result<Self, PayError> {
        let url = meta
            .btc_electrum_url
            .as_deref()
            .ok_or_else(|| {
                PayError::InternalError(
                    "btc_electrum_url is required for electrum backend".to_string(),
                )
            })?
            .to_string();
        Ok(Self { url })
    }

    fn make_client(&self) -> Result<BdkElectrumClient<electrum_client::Client>, PayError> {
        let inner = electrum_client::Client::new(&self.url)
            .map_err(|e| PayError::NetworkError(format!("electrum client: {e}")))?;
        Ok(BdkElectrumClient::new(inner))
    }
}

#[async_trait]
impl BtcChainSource for ElectrumSource {
    async fn sync(&self, wallet: &mut Wallet) -> Result<(), PayError> {
        let client = self.make_client()?;
        let request = wallet.start_sync_with_revealed_spks();
        let update = client
            .sync(request, BATCH_SIZE, false)
            .map_err(|e| PayError::NetworkError(format!("electrum sync: {e}")))?;
        wallet
            .apply_update(update)
            .map_err(|e| PayError::InternalError(format!("apply sync update: {e}")))?;
        Ok(())
    }

    async fn full_scan(&self, wallet: &mut Wallet) -> Result<(), PayError> {
        let client = self.make_client()?;
        let request = wallet.start_full_scan();
        let update = client
            .full_scan(request, STOP_GAP, BATCH_SIZE, false)
            .map_err(|e| PayError::NetworkError(format!("electrum full_scan: {e}")))?;
        wallet
            .apply_update(update)
            .map_err(|e| PayError::InternalError(format!("apply full_scan update: {e}")))?;
        Ok(())
    }

    async fn broadcast(&self, tx: &Transaction) -> Result<(), PayError> {
        let inner = electrum_client::Client::new(&self.url)
            .map_err(|e| PayError::NetworkError(format!("electrum client: {e}")))?;
        inner
            .transaction_broadcast(tx)
            .map_err(|e| PayError::NetworkError(format!("broadcast tx: {e}")))?;
        Ok(())
    }
}
