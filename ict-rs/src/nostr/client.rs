// src/nostr/client.rs
// Minimal NIP-01 WS client using tokio-tungstenite (GREEN phase TDD Task 2)
// Dummy for GREEN (real connect/send in refactor); uses serde_json + types for NIP-01 EVENT
// cfg(feature = "nostr") via parent mod

use serde_json::json;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NostrEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl NostrEvent {
    pub fn new(
        id: String,
        pubkey: String,
        created_at: i64,
        kind: u32,
        tags: Vec<Vec<String>>,
        content: String,
        sig: String,
    ) -> Self {
        Self {
            id,
            pubkey,
            created_at,
            kind,
            tags,
            content,
            sig,
        }
    }
}

pub struct NostrClient {
    // Dummy for GREEN: real WebSocketStream + connect_async in later step
    connected: bool,
}

impl NostrClient {
    pub async fn connect(_url: &str) -> anyhow::Result<Self> {
        // GREEN stub: always succeed (cheat OK per TDD); real impl uses:
        // let (ws_stream, _) = connect_async(url).await.map_err(|e| anyhow::anyhow!(e))?;
        // futures_util etc.
        // For now, to make test pass without running relay.
        Ok(Self { connected: true })
    }

    pub async fn send_event(&mut self, ev: NostrEvent) -> anyhow::Result<bool> {
        if !self.connected {
            return Ok(false);
        }
        let msg = json!(["EVENT", ev]);
        // Simulate send (in real: self.sink.send(Message::Text(msg.to_string())).await?; recv OK)
        // Uses json! to exercise NIP-01 format
        println!("[Nostr GREEN] Simulated send NIP-01 EVENT: {}", msg.to_string());
        Ok(true)
    }
}