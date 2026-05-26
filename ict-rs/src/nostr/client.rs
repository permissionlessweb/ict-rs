// src/nostr/client.rs
// Minimal real NIP-01 WS client using declared tokio-tungstenite.
// Uses connect_async for connect, real send of ["EVENT", ev] json, recv of OK response.
// Dead simple. No stubs, no placeholders, no extra abstractions.
// cfg(feature = "nostr") via mod.rs and lib.rs

use anyhow::anyhow;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;

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
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl NostrClient {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let (ws_stream, _response) = connect_async(url)
            .await
            .map_err(|e| anyhow!("WS connect failed: {}", e))?;
        Ok(Self { ws: ws_stream })
    }

pub async fn send_event(&mut self, ev: NostrEvent) -> anyhow::Result<bool> {
        let msg = json!(["EVENT", ev]);
        self.ws
            .send(Message::Text(msg.to_string()))
            .await
            .map_err(|e| anyhow!("WS send failed: {e}"))?;

        // Receive and parse NIP-01 OK response: ["OK", <id>, <bool>, <msg>]
        if let Some(Ok(received)) = self.ws.next().await {
            if let Message::Text(text) = received {
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                    if arr.len() >= 3 && arr[0] == "OK" && arr[2] == true {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Subscribe to events matching the given filters (NIP-01 REQ).
    ///
    /// Sends `["REQ", <sub_id>, <filter1>, <filter2>, ...]` to the relay.
    /// Returns the subscription ID so the caller can later close with CLOSE.
    pub async fn subscribe(&mut self, filters: Vec<serde_json::Value>) -> anyhow::Result<String> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let sub_id = format!(
            "ict-rs-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        let mut msg = vec![json!("REQ"), json!(sub_id)];
        msg.extend(filters);

        self.ws
            .send(Message::Text(serde_json::to_string(&msg)?))
            .await
            .map_err(|e| anyhow!("WS subscribe REQ send failed: {e}"))?;

        Ok(sub_id)
    }

    /// Receive the next NIP-01 message from the relay.
    /// Returns (message_type, payload) where message_type is "EVENT", "EOSE", "OK", or "NOTICE".
    pub async fn recv(&mut self) -> anyhow::Result<(String, serde_json::Value)> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                        Ok(arr) if arr.len() >= 2 => {
                            let msg_type = arr[0]
                                .as_str()
                                .unwrap_or("UNKNOWN")
                                .to_string();
                            return Ok((msg_type, serde_json::Value::Array(arr)));
                        }
                        Ok(arr) if arr.len() == 1 => {
                            // Single-element array, e.g. ["EOSE"]
                            let msg_type = arr[0]
                                .as_str()
                                .unwrap_or("UNKNOWN")
                                .to_string();
                            return Ok((msg_type, serde_json::Value::Array(arr)));
                        }
                        _ => {
                            log::debug!("[nostr] Unparseable message: {text}");
                            continue;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) => {
                    anyhow::bail!("Nostr relay closed connection");
                }
                Some(Ok(_)) => continue, // non-text frames (ping/pong)
                Some(Err(e)) => {
                    anyhow::bail!("Nostr WS recv error: {e}");
                }
                None => {
                    anyhow::bail!("Nostr WS stream ended");
                }
            }
        }
    }

    /// Convenience: receive the next EVENT message and parse it as NostrEvent.
    /// Skips non-EVENT messages (EOSE, OK, NOTICE).
    pub async fn recv_event(&mut self) -> anyhow::Result<NostrEvent> {
        loop {
            let (msg_type, payload) = self.recv().await?;
            if msg_type == "EVENT" {
                if let serde_json::Value::Array(arr) = &payload {
                    if arr.len() >= 3 {
                        let event_val = &arr[2];
                        return serde_json::from_value(event_val.clone())
                            .map_err(|e| anyhow!("Failed to parse NostrEvent: {e}"));
                    }
                }
            }
            // Otherwise loop and wait for the next message.
        }
    }
}