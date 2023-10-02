use std::error::Error;
use std::fmt;

use veilid_core::{CryptoKey, CryptoKind, CryptoTyped, PublicKey, SecretKey, CRYPTO_KIND_VLD0};

pub mod config;
pub mod utils;

pub const CRYPTO_KIND: CryptoKind = CRYPTO_KIND_VLD0;

#[derive(Debug, Default)]
pub struct ServiceKeys {
    //pub key_pair: TypedKeyPair,
    pub public_key: PublicKey,
    pub secret_key: SecretKey,
    pub dht_key: Option<CryptoTyped<CryptoKey>>,
    pub dht_owner_key: Option<PublicKey>,
    pub dht_owner_secret_key: Option<SecretKey>,
}

#[derive(Debug)]
pub struct BevyVeilidError {
    pub details: String,
}

impl BevyVeilidError {
    pub fn new(msg: &str) -> BevyVeilidError {
        BevyVeilidError {
            details: msg.to_string(),
        }
    }
}

impl fmt::Display for BevyVeilidError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl Error for BevyVeilidError {
    fn description(&self) -> &str {
        &self.details
    }
}
