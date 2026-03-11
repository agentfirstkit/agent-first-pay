use super::BtcChainSource;
use crate::provider::PayError;
use crate::store::wallet::WalletMetadata;
use async_trait::async_trait;
use bdk_esplora::esplora_client;
use bdk_esplora::EsploraAsyncExt;
use bdk_wallet::bitcoin::Transaction;
use bdk_wallet::Wallet;

pub(crate) const DEFAULT_ESPLORA_MAINNET: &str = "https://mempool.space/api";
pub(crate) const DEFAULT_ESPLORA_SIGNET: &str = "https://mempool.space/signet/api";
const STOP_GAP: usize = 20;
const PARALLEL_REQUESTS: usize = 4;

pub(crate) struct EsploraSource {
    url: String,
}

impl EsploraSource {
    pub fn new(meta: &WalletMetadata) -> Self {
        let url = if let Some(url) = &meta.btc_esplora_url {
            url.clone()
        } else {
            match meta.btc_network.as_deref() {
                Some("signet") => DEFAULT_ESPLORA_SIGNET.to_string(),
                _ => DEFAULT_ESPLORA_MAINNET.to_string(),
            }
        };
        Self { url }
    }

    fn make_client(&self) -> Result<esplora_client::AsyncClient, PayError> {
        esplora_client::Builder::new(&self.url)
            .build_async()
            .map_err(|e| PayError::NetworkError(format!("esplora client: {e}")))
    }
}

#[async_trait]
impl BtcChainSource for EsploraSource {
    async fn sync(&self, wallet: &mut Wallet) -> Result<(), PayError> {
        let client = self.make_client()?;
        let request = wallet.start_sync_with_revealed_spks();
        let update = client
            .sync(request, PARALLEL_REQUESTS)
            .await
            .map_err(|e| PayError::NetworkError(format!("esplora sync: {e}")))?;
        wallet
            .apply_update(update)
            .map_err(|e| PayError::InternalError(format!("apply sync update: {e}")))?;
        Ok(())
    }

    async fn full_scan(&self, wallet: &mut Wallet) -> Result<(), PayError> {
        let client = self.make_client()?;
        let request = wallet.start_full_scan();
        let update = client
            .full_scan(request, STOP_GAP, PARALLEL_REQUESTS)
            .await
            .map_err(|e| PayError::NetworkError(format!("esplora full_scan: {e}")))?;
        wallet
            .apply_update(update)
            .map_err(|e| PayError::InternalError(format!("apply full_scan update: {e}")))?;
        Ok(())
    }

    async fn broadcast(&self, tx: &Transaction) -> Result<(), PayError> {
        let client = self.make_client()?;
        client
            .broadcast(tx)
            .await
            .map_err(|e| PayError::NetworkError(format!("broadcast tx: {e}")))
    }
}
