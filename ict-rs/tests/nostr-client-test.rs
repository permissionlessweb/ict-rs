// tests/nostr-client-test.rs
// Integration test for minimal real NIP-01 WS client in ict-rs (optional "nostr" feature).
// Per Linus Torvalds review: this test is marked #[ignore] until a relay is present.
// It does NOT rig a lie to pass. It drives real WS behavior (connect + send_event + OK recv) only when
// executed with --ignored and a real relay (ws://localhost:7777) is running (e.g. via Docker in harness).
// cargo test --features nostr --test nostr-client-test -- --ignored --nocapture

#![cfg(feature = "nostr")]
use ict_rs::nostr::client::{NostrClient, NostrEvent};

#[tokio::test]
#[ignore = "requires running local Nostr relay at ws://localhost:7777; run with --ignored when relay present"]
async fn test_nostr_ws_connect_and_send_event() {
    // Target: basic NIP-01 EVENT send over real WS (tokio-tungstenite connect_async)
    let mut client = NostrClient::connect("ws://localhost:7777").await.expect("connect should succeed to local relay");

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