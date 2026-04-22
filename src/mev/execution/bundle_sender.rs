use crate::mev::execution::payload_builder::ExecutionPayload;
use ethers_flashbots::BundleRequest;

pub struct BundleSender;

impl BundleSender {
    pub fn build_private_bundle(
        target_block: ethers::types::U64,
        payload: &ExecutionPayload,
    ) -> BundleRequest {
        BundleRequest::new()
            .set_block(target_block)
            .push_transaction(payload.tx.clone())
    }
}
