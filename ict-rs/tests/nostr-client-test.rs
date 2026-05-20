// tests/nostr-client-test.rs
// TDD RED phase for Task 2: minimal NIP-01 WS client in ict-rs
// Run with: cargo test --features nostr --test nostr-client-test test_nostr_ws_connect_and_send_event -- --nocapture
// Expect: compile error (no NostrClient / NostrEvent / nostr mod yet)

#![cfg(feature = "nostr")]
use ict_rs::nostr::client::{NostrClient, NostrEvent};

#[tokio::test]
async fn test_nostr_ws_connect_and_send_event() {
    // Target: basic NIP-01 EVENT send over WS (tokio-tungstenite)
    let mut client = NostrClient::connect("ws://localhost:7777").await.expect("connect should succeed to local relay or mock");
    
    // Minimal NostrEvent per NIP-01 (id, pubkey, created_at, kind, tags, content, sig)
    let event = NostrEvent::new(
        "id123".to_string(),
        "pubkey456".to_string(),
        1234567890,
        1, // kind 1 = text note
        vec![],
        "Hello from ict-rs Nostr test".to_string(),
        "sig789".to_string(),
    );
    
    let ok = client.send_event(event).await.expect("send_event should return OK");
    assert!(ok, "NIP-01 EVENT send should receive OK response");
}