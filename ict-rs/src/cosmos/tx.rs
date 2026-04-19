use serde::{Deserialize, Serialize};

/// Canonical flags for a Cosmos SDK `tx` CLI command.
///
/// Centralizes the ~10 flags that every transaction needs (keyring, chain-id,
/// gas, broadcast mode, etc.) so callers never forget one.
///
/// # Example
///
/// ```
/// use ict_rs::tx::TxOptions;
///
/// let opts = TxOptions::new("mychain-1", "0.025uakt")
///     .from("deployer")
///     .memo("funding round 1")
///     .gas_adjustment(2.0);
///
/// let flags = opts.to_flags();
/// assert!(flags.contains(&"--chain-id".to_string()));
/// ```
#[derive(Debug, Clone)]
pub struct TxOptions {
    pub keyring_backend: String,
    pub chain_id: String,
    pub gas_prices: String,
    pub gas: String,
    pub gas_adjustment: f64,
    pub broadcast_mode: String,
    pub output: String,
    pub yes: bool,
    pub from: Option<String>,
    pub memo: Option<String>,
    pub extra: Vec<(String, String)>,
}

impl TxOptions {
    /// Create a new `TxOptions` with sensible defaults.
    pub fn new(chain_id: impl Into<String>, gas_prices: impl Into<String>) -> Self {
        Self {
            keyring_backend: "test".to_string(),
            chain_id: chain_id.into(),
            gas_prices: gas_prices.into(),
            gas: "auto".to_string(),
            gas_adjustment: 1.5,
            broadcast_mode: "sync".to_string(),
            output: "json".to_string(),
            yes: true,
            from: None,
            memo: None,
            extra: Vec::new(),
        }
    }

    /// Set the `--from` key name.
    pub fn from(mut self, key: impl Into<String>) -> Self {
        self.from = Some(key.into());
        self
    }

    /// Set the `--memo` field.
    pub fn memo(mut self, memo: impl Into<String>) -> Self {
        self.memo = Some(memo.into());
        self
    }

    /// Override gas adjustment.
    pub fn gas_adjustment(mut self, adj: f64) -> Self {
        self.gas_adjustment = adj;
        self
    }

    /// Override gas value (default "auto").
    pub fn gas(mut self, gas: impl Into<String>) -> Self {
        self.gas = gas.into();
        self
    }

    /// Override broadcast mode (default "sync").
    pub fn broadcast_mode(mut self, mode: impl Into<String>) -> Self {
        self.broadcast_mode = mode.into();
        self
    }

    /// Add an arbitrary extra flag (e.g. `.flag("--no-validate", "")` or `.flag("--amount", "100uakt")`).
    pub fn flag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.push((key.into(), value.into()));
        self
    }

    /// Render to a `Vec<String>` of CLI flags.
    ///
    /// Order: `--from`, `--keyring-backend`, `--chain-id`, `--gas-prices`,
    /// `--gas`, `--gas-adjustment`, `--broadcast-mode`, `--output`, `-y`,
    /// `--memo`, extras.
    pub fn to_flags(&self) -> Vec<String> {
        let gas_adj = format!("{}", self.gas_adjustment);
        let mut flags = Vec::with_capacity(20);

        if let Some(ref from) = self.from {
            flags.extend(["--from".to_string(), from.clone()]);
        }
        flags.extend([
            "--keyring-backend".to_string(),
            self.keyring_backend.clone(),
            "--chain-id".to_string(),
            self.chain_id.clone(),
            "--gas-prices".to_string(),
            self.gas_prices.clone(),
            "--gas".to_string(),
            self.gas.clone(),
            "--gas-adjustment".to_string(),
            gas_adj,
            "--broadcast-mode".to_string(),
            self.broadcast_mode.clone(),
            "--output".to_string(),
            self.output.clone(),
        ]);
        if self.yes {
            flags.push("-y".to_string());
        }
        if let Some(ref memo) = self.memo {
            flags.extend(["--memo".to_string(), memo.clone()]);
        }
        for (k, v) in &self.extra {
            flags.push(k.clone());
            if !v.is_empty() {
                flags.push(v.clone());
            }
        }
        flags
    }
}

/// Output from executing a command in a container.
#[derive(Debug, Clone, Default)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i64,
}

impl ExecOutput {
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }
}

/// A committed transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tx {
    pub height: u64,
    pub tx_hash: String,
    pub gas_spent: u64,
    pub packet: Option<Packet>,
}

/// An IBC packet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Packet {
    pub sequence: u64,
    pub source_port: String,
    pub source_channel: String,
    pub dest_port: String,
    pub dest_channel: String,
    pub data: Vec<u8>,
    pub timeout_height: String,
    pub timeout_timestamp: u64,
}

/// A packet with its acknowledgement data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketAcknowledgement {
    pub packet: Packet,
    pub acknowledgement: Vec<u8>,
}

/// A timed-out packet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketTimeout {
    pub packet: Packet,
}

/// An amount of tokens associated with an address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletAmount {
    pub address: String,
    pub denom: String,
    pub amount: u128,
}

/// Options for IBC transfers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferOptions {
    pub timeout_height: Option<u64>,
    pub timeout_timestamp: Option<u64>,
    pub memo: Option<String>,
}
