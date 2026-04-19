use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum IctError {
    #[cfg(feature = "docker")]
    #[error("Docker error: {0}")]
    Docker(#[from] bollard::errors::Error),

    #[error("Chain error on {chain_id}: {source}")]
    Chain {
        chain_id: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("Relayer error on {relayer}: {source}")]
    Relayer {
        relayer: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("Runtime error: {0}")]
    Runtime(anyhow::Error),

    #[error("IBC error: {0}")]
    Ibc(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Key/wallet error: {0}")]
    Wallet(String),

    #[error("Timeout waiting for {what} after {duration:?}")]
    Timeout { what: String, duration: Duration },

    #[error("Container exec failed (exit {exit_code}): {stderr}")]
    ExecFailed { exit_code: i64, stderr: String },

    #[error(transparent)]
    Other(#[from] anyhow::Error),

    #[error(transparent)]
    CwEnvError(#[from] cw_orch_core::CwEnvError),
}

pub type Result<T> = std::result::Result<T, IctError>;
