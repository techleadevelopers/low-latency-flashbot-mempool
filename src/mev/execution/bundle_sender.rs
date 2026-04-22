use crate::config::Config;
use crate::mev::execution::payload_builder::ExecutionPayload;
use ethers::middleware::SignerMiddleware;
use ethers::prelude::*;
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware, PendingBundle};
use url::Url;

pub struct BundleSender;

impl BundleSender {
    pub async fn send_private_bundle<M: Middleware + 'static>(
        provider: std::sync::Arc<M>,
        searcher_wallet: LocalWallet,
        config: &Config,
        target_block: U64,
        payload: &ExecutionPayload,
    ) -> Result<PendingBundle<'_, M::Provider>, Box<dyn std::error::Error>>
    where
        M::Provider: JsonRpcClient,
    {
        let relay_url = Url::parse(&config.flashbots_relay)?;
        let relay_signer = searcher_wallet.clone();
        let flashbots_client = SignerMiddleware::new(provider, searcher_wallet);
        let flashbots = FlashbotsMiddleware::new(flashbots_client, relay_url, relay_signer);
        let bundle = BundleRequest::new()
            .set_block(target_block)
            .push_transaction(payload.tx.clone());
        Ok(flashbots.send_bundle(&bundle).await?)
    }
}
