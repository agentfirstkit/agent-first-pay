use super::BtcChainSource;
use crate::provider::PayError;
use crate::store::wallet::WalletMetadata;
use async_trait::async_trait;
use bdk_bitcoind_rpc::bitcoincore_rpc::{Auth, Client, RpcApi};
use bdk_bitcoind_rpc::{Emitter, NO_EXPECTED_MEMPOOL_TXS};
use bdk_wallet::bitcoin::Transaction;
use bdk_wallet::Wallet;
use std::sync::Arc;

pub(crate) struct CoreRpcSource {
    url: String,
    auth: Auth,
}

impl CoreRpcSource {
    pub fn new(meta: &WalletMetadata) -> Result<Self, PayError> {
        let url = meta
            .btc_core_url
            .as_deref()
            .ok_or_else(|| {
                PayError::InternalError("btc_core_url is required for core-rpc backend".to_string())
            })?
            .to_string();

        let auth = if let Some(ref auth_str) = meta.btc_core_auth_secret {
            if let Some((user, pass)) = auth_str.split_once(':') {
                Auth::UserPass(user.to_string(), pass.to_string())
            } else {
                Auth::CookieFile(auth_str.into())
            }
        } else {
            Auth::None
        };

        Ok(Self { url, auth })
    }

    fn make_client(&self) -> Result<Client, PayError> {
        Client::new(&self.url, self.auth.clone())
            .map_err(|e| PayError::NetworkError(format!("bitcoind rpc client: {e}")))
    }

    fn sync_blocks(&self, wallet: &mut Wallet) -> Result<(), PayError> {
        let client = self.make_client()?;
        let tip = wallet.latest_checkpoint();
        let mut emitter = Emitter::new(&client, tip.clone(), tip.height(), NO_EXPECTED_MEMPOOL_TXS);

        while let Some(block) = emitter
            .next_block()
            .map_err(|e| PayError::NetworkError(format!("bitcoind next_block: {e}")))?
        {
            wallet
                .apply_block_connected_to(&block.block, block.block_height(), block.connected_to())
                .map_err(|e| PayError::InternalError(format!("apply block: {e}")))?;
        }

        let mempool = emitter
            .mempool()
            .map_err(|e| PayError::NetworkError(format!("bitcoind mempool: {e}")))?;
        let txs: Vec<(Arc<Transaction>, u64)> = mempool.update;
        wallet.apply_unconfirmed_txs(txs);

        Ok(())
    }
}

#[async_trait]
impl BtcChainSource for CoreRpcSource {
    async fn sync(&self, wallet: &mut Wallet) -> Result<(), PayError> {
        self.sync_blocks(wallet)
    }

    async fn full_scan(&self, wallet: &mut Wallet) -> Result<(), PayError> {
        // For bitcoind RPC, full_scan is the same block-by-block sync.
        self.sync_blocks(wallet)
    }

    async fn broadcast(&self, tx: &Transaction) -> Result<(), PayError> {
        let client = self.make_client()?;
        client
            .send_raw_transaction(tx)
            .map_err(|e| PayError::NetworkError(format!("broadcast tx: {e}")))?;
        Ok(())
    }
}
