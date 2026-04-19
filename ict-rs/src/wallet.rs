use crate::auth::KeyringAuthenticator;
use crate::error::Result;

/// Abstraction over a blockchain account/wallet.
pub trait Wallet: Send + Sync {
    fn key_name(&self) -> &str;
    fn address(&self) -> &[u8];
    fn formatted_address(&self) -> String;
    fn mnemonic(&self) -> &str;
}

/// A basic wallet backed by a mnemonic and derived address.
#[derive(Debug, Clone)]
pub struct KeyWallet {
    pub key_name: String,
    pub address_bytes: Vec<u8>,
    pub bech32_address: String,
    pub mnemonic_phrase: String,
}

impl KeyWallet {
    /// Construct a `KeyWallet` from a BIP39 mnemonic phrase.
    ///
    /// Derives the signing key via BIP32 path `m/44'/{coin_type}'/0'/0/0`,
    /// computes the cosmos address (`ripemd160(sha256(pubkey))`), and
    /// bech32-encodes it with the given prefix.
    pub fn from_mnemonic(key_name: &str, mnemonic: &str, prefix: &str, coin_type: u32) -> Result<Self> {
        let auth = KeyringAuthenticator::new(mnemonic, coin_type)?;
        let address_bytes = auth.address_bytes();
        let bech32_address = auth.bech32_address(prefix)?;

        Ok(Self {
            key_name: key_name.to_string(),
            address_bytes,
            bech32_address,
            mnemonic_phrase: mnemonic.to_string(),
        })
    }
}

impl Wallet for KeyWallet {
    fn key_name(&self) -> &str {
        &self.key_name
    }

    fn address(&self) -> &[u8] {
        &self.address_bytes
    }

    fn formatted_address(&self) -> String {
        self.bech32_address.clone()
    }

    fn mnemonic(&self) -> &str {
        &self.mnemonic_phrase
    }
}

/// An Ethereum wallet, typically backed by an Anvil pre-funded account.
#[cfg(feature = "ethereum")]
#[derive(Debug, Clone)]
pub struct EthWallet {
    pub key_name: String,
    pub address_bytes: Vec<u8>,
    pub hex_address: String,
    pub mnemonic_phrase: String,
    pub private_key: Option<String>,
}

#[cfg(feature = "ethereum")]
impl EthWallet {
    /// Create an EthWallet from an Anvil pre-funded account.
    pub fn from_anvil_account(index: usize, private_key: &str, address: &str) -> Self {
        let clean_addr = address.strip_prefix("0x").unwrap_or(address);
        let address_bytes = hex::decode(clean_addr).unwrap_or_default();
        let checksummed = eip55_checksum(clean_addr);

        Self {
            key_name: format!("anvil-{index}"),
            address_bytes,
            hex_address: checksummed,
            mnemonic_phrase: String::new(),
            private_key: Some(private_key.to_string()),
        }
    }
}

#[cfg(feature = "ethereum")]
impl Wallet for EthWallet {
    fn key_name(&self) -> &str {
        &self.key_name
    }

    fn address(&self) -> &[u8] {
        &self.address_bytes
    }

    fn formatted_address(&self) -> String {
        self.hex_address.clone()
    }

    fn mnemonic(&self) -> &str {
        &self.mnemonic_phrase
    }
}

/// EIP-55 mixed-case checksum encoding for Ethereum addresses.
#[cfg(feature = "ethereum")]
fn eip55_checksum(addr_hex: &str) -> String {
    use sha3::{Digest, Keccak256};

    let lower = addr_hex.to_lowercase();
    let hash = Keccak256::digest(lower.as_bytes());
    let hash_hex = hex::encode(hash);

    let mut checksummed = String::with_capacity(42);
    checksummed.push_str("0x");
    for (i, c) in lower.chars().enumerate() {
        if c.is_ascii_alphabetic() {
            let nibble = u8::from_str_radix(&hash_hex[i..i + 1], 16).unwrap_or(0);
            if nibble >= 8 {
                checksummed.push(c.to_ascii_uppercase());
            } else {
                checksummed.push(c);
            }
        } else {
            checksummed.push(c);
        }
    }
    checksummed
}
