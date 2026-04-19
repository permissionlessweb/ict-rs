use serde::{Deserialize, Serialize};

/// IBC channel output from relayer queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelOutput {
    pub state: String,
    pub ordering: String,
    pub version: String,
    pub port_id: String,
    pub channel_id: String,
    pub connection_hops: Vec<String>,
    pub counterparty: ChannelCounterparty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelCounterparty {
    pub port_id: String,
    pub channel_id: String,
}

/// IBC connection output from relayer queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionOutput {
    pub id: String,
    pub client_id: String,
    pub state: String,
    pub counterparty_client_id: String,
    pub counterparty_connection_id: String,
}

/// IBC client output from relayer queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientOutput {
    pub client_id: String,
    pub client_type: String,
    pub chain_id: String,
}

/// Options for creating IBC channels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelOptions {
    pub src_port: String,
    pub dst_port: String,
    pub ordering: ChannelOrdering,
    pub version: String,
}

/// Options for creating IBC clients.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientOptions {
    pub trusting_period: Option<String>,
    pub max_clock_drift: Option<String>,
}

/// IBC channel ordering.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum ChannelOrdering {
    #[default]
    Unordered,
    Ordered,
}

impl std::fmt::Display for ChannelOrdering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unordered => write!(f, "unordered"),
            Self::Ordered => write!(f, "ordered"),
        }
    }
}

/// Compute the IBC denomination for a token that has been transferred
/// through a single hop (port/channel).
///
/// The IBC denom is `ibc/` followed by the uppercase hex SHA-256 hash of
/// `"{port}/{channel}/{base_denom}"`.
pub fn ibc_denom(port: &str, channel: &str, base_denom: &str) -> String {
    use sha2::{Digest, Sha256};
    let path = format!("{port}/{channel}/{base_denom}");
    let hash = Sha256::digest(path.as_bytes());
    format!("ibc/{}", hex::encode_upper(hash))
}

/// Compute the IBC denomination for a token that has traversed multiple
/// hops. Each hop is a `(port, channel)` pair applied in order.
///
/// Example: a token transferred A→B→C with hops
/// `[("transfer","channel-0"), ("transfer","channel-1")]` and base denom
/// `"uterp"` produces `SHA256("transfer/channel-0/transfer/channel-1/uterp")`.
pub fn ibc_denom_multi_hop(hops: &[(&str, &str)], base_denom: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut path = String::new();
    for (port, channel) in hops {
        path.push_str(port);
        path.push('/');
        path.push_str(channel);
        path.push('/');
    }
    path.push_str(base_denom);
    let hash = Sha256::digest(path.as_bytes());
    format!("ibc/{}", hex::encode_upper(hash))
}
