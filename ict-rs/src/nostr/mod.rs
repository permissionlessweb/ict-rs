// src/nostr/mod.rs
// Nostr NIP-01 support for ict-rs (optional "nostr" feature)
// Re-exports for ict_rs::nostr::client

pub mod client;
pub use client::NostrClient;
pub use client::NostrEvent;