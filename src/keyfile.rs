//! The encrypted key file (`key.enc` in the config directory, see `config.rs`).
//!
//! The password is stretched with scrypt (memory-hard: 512 MiB and a few seconds per guess at the default cost) and the key is
//! sealed with AES-256-GCM, which is authenticated: a wrong password and any change to the file are both detected. The header
//! (version, cost parameters, salt) is bound to the ciphertext as associated data, and a hostile cost is refused before any
//! work is done.
//!
//! The format is versioned (`"v": 2`); `opens_a_file_written_by_an_earlier_build` keeps files written by earlier builds readable.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::RngCore;
use serde_json::{json, Value};
use std::fmt;
use zeroize::Zeroizing;

pub const VERSION: u64 = 2;
pub const DEFAULT_LOG_N: u8 = 19; // N = 2^19, r = 8: 512 MiB
pub const MIN_LOG_N: u8 = 10;
pub const MAX_LOG_N: u8 = 20; // also stops a hostile file from asking for gigabytes
const R: u32 = 8;
const P: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum KeyFileError {
    /// Wrong password, or the file was modified.
    WrongPassword,
    /// Not a key file we can use (unknown version, absurd parameters, malformed).
    Unusable(String),
}

impl fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyFileError::WrongPassword => write!(f, "wrong password, or the file was modified"),
            KeyFileError::Unusable(why) => write!(f, "unusable key file: {why}"),
        }
    }
}

impl std::error::Error for KeyFileError {}

/// The authenticated header, serialised with sorted keys and compact separators so the associated data is the same every time.
fn header_aad(log_n: u64, salt_b64: &str) -> Vec<u8> {
    format!(r#"{{"kdf":"scrypt","log_n":{log_n},"p":{P},"r":{R},"salt":"{salt_b64}","v":{VERSION}}}"#).into_bytes()
}

fn derive(password: &[u8], salt: &[u8], log_n: u8) -> Result<Zeroizing<[u8; 32]>, KeyFileError> {
    let params = scrypt::Params::new(log_n, R, P).map_err(|e| KeyFileError::Unusable(e.to_string()))?;
    let mut key = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(password, salt, &params, key.as_mut_slice()).map_err(|e| KeyFileError::Unusable(e.to_string()))?;
    Ok(key)
}

/// Seal `key` under `password`; returns the JSON document to store.
pub fn encrypt(key: &[u8], password: &[u8], log_n: u8) -> Result<String, KeyFileError> {
    if !(MIN_LOG_N..=MAX_LOG_N).contains(&log_n) {
        return Err(KeyFileError::Unusable("scrypt cost out of range".into()));
    }
    if password.is_empty() {
        return Err(KeyFileError::Unusable("empty password".into()));
    }
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let salt_b64 = B64.encode(salt);
    let aad = header_aad(log_n as u64, &salt_b64);
    let derived = derive(password, &salt, log_n)?;
    let cipher = Aes256Gcm::new_from_slice(derived.as_slice()).map_err(|e| KeyFileError::Unusable(e.to_string()))?;
    let sealed = cipher
        .encrypt((&nonce).into(), Payload { msg: key, aad: &aad })
        .map_err(|_| KeyFileError::Unusable("encryption failed".into()))?;
    let doc = json!({
        "v": VERSION, "kdf": "scrypt", "log_n": log_n, "r": R, "p": P,
        "salt": salt_b64, "nonce": B64.encode(nonce), "ct": B64.encode(sealed),
    });
    Ok(serde_json::to_string(&doc).expect("a json value serialises") + "\n")
}

/// Open a key file's contents. The key bytes are returned in a buffer that is wiped when dropped.
pub fn decrypt(contents: &str, password: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyFileError> {
    let doc: Value = serde_json::from_str(contents).map_err(|_| KeyFileError::Unusable("not JSON (an old-format file? save the key again)".into()))?;
    let get_u = |k: &str| doc.get(k).and_then(Value::as_u64);
    if get_u("v") != Some(VERSION) || doc.get("kdf").and_then(Value::as_str) != Some("scrypt") || get_u("r") != Some(R as u64) || get_u("p") != Some(P as u64) {
        return Err(KeyFileError::Unusable("unsupported key file".into()));
    }
    let log_n = get_u("log_n").ok_or_else(|| KeyFileError::Unusable("malformed key file".into()))?;
    if !(MIN_LOG_N as u64..=MAX_LOG_N as u64).contains(&log_n) {
        return Err(KeyFileError::Unusable("the file asks for an unreasonable scrypt cost".into()));
    }
    let field = |k: &str| -> Result<String, KeyFileError> {
        doc.get(k).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| KeyFileError::Unusable("malformed key file".into()))
    };
    let salt_b64 = field("salt")?;
    let bytes = |k: &str, s: &str| B64.decode(s).map_err(|_| KeyFileError::Unusable(format!("malformed {k}")));
    let salt = bytes("salt", &salt_b64)?;
    let nonce = bytes("nonce", &field("nonce")?)?;
    let sealed = bytes("ct", &field("ct")?)?;
    if nonce.len() != 12 {
        return Err(KeyFileError::Unusable("malformed nonce".into()));
    }
    let derived = derive(password, &salt, log_n as u8)?;
    let cipher = Aes256Gcm::new_from_slice(derived.as_slice()).map_err(|e| KeyFileError::Unusable(e.to_string()))?;
    let aad = header_aad(log_n, &salt_b64);
    cipher
        .decrypt(nonce.as_slice().try_into().map_err(|_| KeyFileError::Unusable("malformed nonce".into()))?, Payload { msg: &sealed, aad: &aad })
        .map(Zeroizing::new)
        .map_err(|_| KeyFileError::WrongPassword)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"abababababababababababababababababababababababababababababababab";
    const PW: &[u8] = b"correct horse battery staple";

    #[test]
    fn round_trip_and_wrong_password() {
        let doc = encrypt(KEY, PW, MIN_LOG_N).unwrap();
        assert_eq!(decrypt(&doc, PW).unwrap().as_slice(), KEY);
        assert_eq!(decrypt(&doc, b"wrong password!").unwrap_err(), KeyFileError::WrongPassword);
        assert!(!doc.contains("abab"), "the key must not appear in the file");
    }

    #[test]
    fn every_modification_is_detected() {
        let doc: Value = serde_json::from_str(&encrypt(KEY, PW, MIN_LOG_N).unwrap()).unwrap();
        for field in ["ct", "nonce", "salt"] {
            let mut bad = doc.clone();
            let mut raw = B64.decode(bad[field].as_str().unwrap()).unwrap();
            raw[0] ^= 1;
            bad[field] = json!(B64.encode(raw));
            let r = decrypt(&bad.to_string(), PW);
            assert!(r.is_err(), "modified {field} must be refused");
        }
        let mut bad = doc.clone();
        bad["log_n"] = json!(MIN_LOG_N + 1);
        assert_eq!(decrypt(&bad.to_string(), PW).unwrap_err(), KeyFileError::WrongPassword, "the cost is authenticated");
    }

    #[test]
    fn hostile_or_unknown_files_are_refused_without_computing() {
        let doc: Value = serde_json::from_str(&encrypt(KEY, PW, MIN_LOG_N).unwrap()).unwrap();
        let mut huge = doc.clone();
        huge["log_n"] = json!(40);
        let t = std::time::Instant::now();
        assert!(matches!(decrypt(&huge.to_string(), PW), Err(KeyFileError::Unusable(_))));
        assert!(t.elapsed().as_millis() < 500);
        let mut v1 = doc.clone();
        v1["v"] = json!(1);
        assert!(matches!(decrypt(&v1.to_string(), PW), Err(KeyFileError::Unusable(_))));
        assert!(matches!(decrypt("U2FsdGVkX1+notjson", PW), Err(KeyFileError::Unusable(_))));
        assert!(matches!(encrypt(KEY, PW, MAX_LOG_N + 1), Err(KeyFileError::Unusable(_))));
        assert!(matches!(encrypt(KEY, b"", MIN_LOG_N), Err(KeyFileError::Unusable(_))));
    }

    /// A file written by an earlier build must keep opening: same format, same associated data. Do not regenerate the fixture;
    /// if this fails, the format changed and old key files would stop working.
    #[test]
    fn opens_a_file_written_by_an_earlier_build() {
        let fixture = include_str!("../tests/fixtures/key-v2.json");
        assert_eq!(decrypt(fixture, PW).unwrap().as_slice(), KEY);
        assert_eq!(decrypt(fixture, b"nope nope nope").unwrap_err(), KeyFileError::WrongPassword);
    }

    #[test]
    fn default_cost_is_the_strong_one() {
        assert!(DEFAULT_LOG_N >= 19 && MAX_LOG_N <= 20);
    }
}
