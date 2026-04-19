/// Errors that can occur when bridging ict-rs to cw-orch.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("chain has no host gRPC address — is the container running with ports resolved?")]
    NoGrpcAddress,

    #[error("ict-rs error: {0}")]
    Ict(#[from] ict_rs::error::IctError),
}
