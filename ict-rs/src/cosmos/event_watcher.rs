// src/cosmos/event_watcher.rs
// Subscribes to Tendermint WebSocket events and yields parsed chain events.
// Used by NostrTestEnv to bridge on-chain events to a Nostr relay.

use futures::StreamExt;
use tendermint_rpc::{
    event::{Event, EventData},
    query::EventType,
    SubscriptionClient,
    WebSocketClient,
};
use tokio::sync::mpsc;

/// A parsed on-chain event with metadata suitable for Nostr relay.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChainEvent {
    pub event_type: String,
    /// Flat key-value pairs from the event's attribute map.
    /// Keys are like "tx.height", "wasm.action", "_contract_address", etc.
    pub attributes: Vec<(String, String)>,
    pub height: u64,
    pub action: Option<String>,
    pub contract_address: Option<String>,
}

/// Watches chain events over a Tendermint WebSocket connection.
pub struct ChainEventWatcher {
    pub rx: mpsc::Receiver<ChainEvent>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ChainEventWatcher {
    /// Connect to a chain's WebSocket endpoint and subscribe to events.
    pub async fn connect(
        ws_url: &str,
        event_type: Option<EventType>,
    ) -> anyhow::Result<Self> {
        let (client, driver) = WebSocketClient::new(ws_url)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create WS client: {e}"))?;

        let driver_handle = tokio::spawn(async move {
            driver.run().await;
        });

        let query: tendermint_rpc::query::Query = event_type
            .unwrap_or(EventType::Tx)
            .into();

        let mut subscription = client
            .subscribe(query)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe: {e}"))?;

        let (tx, rx) = mpsc::channel::<ChainEvent>(256);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(event_result) = subscription.next() => {
                        match event_result {
                            Ok(event) => {
                                if let Some(chain_event) = Self::parse_event(event) {
                                    let _ = tx.send(chain_event).await;
                                }
                            }
                            Err(e) => {
                                log::warn!("[event_watcher] Subscription error: {e}");
                                break;
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        log::info!("[event_watcher] Shutting down subscription");
                        break;
                    }
                }
            }
            drop(subscription);
            driver_handle.abort();
        });

        Ok(Self {
            rx,
            shutdown: Some(shutdown_tx),
        })
    }

    fn parse_event(event: Event) -> Option<ChainEvent> {
        // Determine event type from the EventData variant.
        let event_type = match &event.data {
            EventData::Tx { .. } => "tx",
            EventData::NewBlock { .. } => "new_block",
            EventData::LegacyNewBlock { .. } => "new_block",
            _ => "unknown",
        }
        .to_string();

        // Extract height from TxInfo if available, or from flat events map.
        let height = match &event.data {
            EventData::Tx { tx_result } => tx_result.height as u64,
            _ => 0,
        };

        // Flatten the events HashMap into a Vec of (key, value) pairs.
        let mut attributes: Vec<(String, String)> = Vec::new();
        let mut action: Option<String> = None;
        let mut contract_address: Option<String> = None;

        if let Some(ref events_map) = event.events {
            for (ev_type, values) in events_map {
                for value in values {
                    let key = ev_type.clone();
                    if key == "action" || key.ends_with(".action") {
                        action = Some(value.clone());
                    }
                    if key == "_contract_address" || key.ends_with("._contract_address") {
                        contract_address = Some(value.clone());
                    }
                    attributes.push((key.clone(), value.clone()));
                }
            }
        }

        // If no action found in flat map, try EventData Tx path.
        if action.is_none() {
            if let EventData::Tx { tx_result } = &event.data {
                // Look through ABCI events for wasm contract addresses.
                for abci_ev in &tx_result.result.events {
                    if abci_ev.kind == "wasm" || abci_ev.kind == "execute" {
                        for attr in &abci_ev.attributes {
                            if let (Ok(k), Ok(v)) = (attr.key_str(), attr.value_str()) {
                                if k == "_contract_address" {
                                    contract_address = Some(v.to_string());
                                }
                                if k == "action" {
                                    action = Some(v.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        Some(ChainEvent {
            event_type,
            attributes,
            height,
            action,
            contract_address,
        })
    }

    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chain_event_serde() {
        let ev = ChainEvent {
            event_type: "tx".into(),
            attributes: vec![("key".into(), "value".into())],
            height: 42,
            action: Some("wasm-execute".into()),
            contract_address: Some("terp1...".into()),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let deserialized: ChainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type, "tx");
        assert_eq!(deserialized.height, 42);
    }
}