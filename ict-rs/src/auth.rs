use async_trait::async_trait;
use bech32::{Bech32, Hrp};
use bip32::DerivationPath;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

use crate::error::{IctError, Result};

/// Pluggable authenticator for signing and broadcasting transactions.
///
/// Inspired by layer-climb's `TxSigner` pattern. The default implementation
/// uses a BIP39 mnemonic with BIP32 key derivation (Secp256k1). Users can
/// provide custom implementations for Ledger, AWS KMS, HSM, or other signers.
#[async_trait]
pub trait Authenticator: Send + Sync {
    /// Sign a raw sign doc and return the signature bytes.
    async fn sign(&self, sign_doc: &[u8]) -> Result<Vec<u8>>;

    /// Return the public key bytes.
    async fn public_key(&self) -> Result<Vec<u8>>;

    /// Derive the bech32 address for a given prefix.
    async fn address(&self, prefix: &str) -> Result<String>;
}

/// Default authenticator using a BIP39 mnemonic + BIP32 Secp256k1 derivation.
///
/// Derives a signing key from a BIP39 mnemonic using the BIP32 HD derivation
/// path `m/44'/{coin_type}'/0'/0/0` (standard Cosmos key path).
pub struct KeyringAuthenticator {
    signing_key: SigningKey,
}

impl KeyringAuthenticator {
    /// Create a new `KeyringAuthenticator` from a BIP39 mnemonic phrase and coin type.
    ///
    /// The derivation path used is `m/44'/{coin_type}'/0'/0/0`.
    ///
    /// # Errors
    ///
    /// Returns `IctError::Wallet` if the mnemonic is invalid or key derivation fails.
    pub fn new(mnemonic: &str, coin_type: u32) -> Result<Self> {
        let parsed = bip39::Mnemonic::parse(mnemonic)
            .map_err(|e| IctError::Wallet(format!("invalid mnemonic: {e}")))?;

        let seed = parsed.to_seed("");

        let path: DerivationPath = format!("m/44'/{coin_type}'/0'/0/0")
            .parse()
            .map_err(|e| IctError::Wallet(format!("invalid derivation path: {e}")))?;

        let child_xprv = bip32::XPrv::derive_from_path(seed, &path)
            .map_err(|e| IctError::Wallet(format!("BIP32 derivation failed: {e}")))?;

        let signing_key = SigningKey::from_bytes(&child_xprv.to_bytes().into())
            .map_err(|e| IctError::Wallet(format!("invalid signing key bytes: {e}")))?;

        Ok(Self { signing_key })
    }

    /// Return a reference to the underlying `k256` signing key.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Return the `k256` verifying (public) key.
    pub fn verifying_key(&self) -> VerifyingKey {
        *self.signing_key.verifying_key()
    }

    /// Compute the compressed SEC1 public key bytes (33 bytes).
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .to_vec()
    }

    /// Derive the raw 20-byte cosmos address: `ripemd160(sha256(compressed_pubkey))`.
    pub fn address_bytes(&self) -> Vec<u8> {
        let pubkey = self.public_key_bytes();
        let sha_hash = Sha256::digest(&pubkey);
        Ripemd160::digest(sha_hash).to_vec()
    }

    /// Derive the bech32-encoded address for a given human-readable prefix.
    pub fn bech32_address(&self, prefix: &str) -> Result<String> {
        let addr_bytes = self.address_bytes();
        let hrp = Hrp::parse(prefix)
            .map_err(|e| IctError::Wallet(format!("invalid bech32 prefix '{prefix}': {e}")))?;
        let encoded = bech32::encode::<Bech32>(hrp, &addr_bytes)
            .map_err(|e| IctError::Wallet(format!("bech32 encoding failed: {e}")))?;
        Ok(encoded)
    }
}

#[async_trait]
impl Authenticator for KeyringAuthenticator {
    /// Sign the raw sign-doc bytes.
    ///
    /// Computes `SHA-256(sign_doc)` then produces an ECDSA signature over
    /// that hash, returning the 64-byte `(r || s)` signature.
    async fn sign(&self, sign_doc: &[u8]) -> Result<Vec<u8>> {
        let digest = Sha256::digest(sign_doc);
        let signature: Signature = self
            .signing_key
            .sign_prehash(&digest)
            .map_err(|e| IctError::Wallet(format!("signing failed: {e}")))?;
        Ok(signature.to_bytes().to_vec())
    }

    /// Return the compressed SEC1 public key bytes (33 bytes).
    async fn public_key(&self) -> Result<Vec<u8>> {
        Ok(self.public_key_bytes())
    }

    /// Derive the bech32 address for the given prefix.
    async fn address(&self, prefix: &str) -> Result<String> {
        self.bech32_address(prefix)
    }
}

/// Generate a random 24-word BIP39 mnemonic phrase.
pub fn generate_mnemonic() -> String {
    let mut entropy = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut entropy);
    bip39::Mnemonic::from_entropy(&entropy)
        .expect("32 bytes of entropy is always valid for a 24-word mnemonic")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn test_generate_mnemonic() {
        let m = generate_mnemonic();
        let words: Vec<&str> = m.split_whitespace().collect();
        assert_eq!(words.len(), 24);
        // Should parse back successfully
        bip39::Mnemonic::parse(&m).expect("generated mnemonic should be valid");
    }

    #[test]
    fn test_keyring_authenticator_construction() {
        let auth = KeyringAuthenticator::new(TEST_MNEMONIC, 118).unwrap();
        let pubkey = auth.public_key_bytes();
        assert_eq!(pubkey.len(), 33, "compressed pubkey should be 33 bytes");
    }

    #[test]
    fn test_keyring_authenticator_address() {
        let auth = KeyringAuthenticator::new(TEST_MNEMONIC, 118).unwrap();
        let addr = auth.bech32_address("cosmos").unwrap();
        assert!(addr.starts_with("cosmos1"), "address should start with cosmos1");
    }

    #[test]
    fn test_address_bytes_length() {
        let auth = KeyringAuthenticator::new(TEST_MNEMONIC, 118).unwrap();
        let addr_bytes = auth.address_bytes();
        assert_eq!(addr_bytes.len(), 20, "address bytes should be 20 bytes (ripemd160 output)");
    }

    #[tokio::test]
    async fn test_sign_and_verify() {
        let auth = KeyringAuthenticator::new(TEST_MNEMONIC, 118).unwrap();
        let msg = b"test sign doc";
        let sig_bytes = auth.sign(msg).await.unwrap();
        assert_eq!(sig_bytes.len(), 64, "ECDSA signature should be 64 bytes");

        // Verify the signature
        let digest = Sha256::digest(msg);
        let signature = Signature::from_slice(&sig_bytes).unwrap();
        let vk = auth.verifying_key();
        use k256::ecdsa::signature::hazmat::PrehashVerifier;
        vk.verify_prehash(&digest, &signature)
            .expect("signature should verify");
    }

    #[test]
    fn test_invalid_mnemonic() {
        let result = KeyringAuthenticator::new("not a valid mnemonic", 118);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_authenticator_trait_methods() {
        let auth = KeyringAuthenticator::new(TEST_MNEMONIC, 118).unwrap();

        let pk = auth.public_key().await.unwrap();
        assert_eq!(pk.len(), 33);

        let addr = auth.address("cosmos").await.unwrap();
        assert!(addr.starts_with("cosmos1"));
    }
}
