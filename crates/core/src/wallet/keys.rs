//! Keys: a recovery phrase, or a key taken from another wallet app, turned
//! into what signs on each chain.
//!
//! One phrase gives an EVM key and a Solana key on the paths the common
//! wallet apps use -- MetaMask's `m/44'/60'/0'/0/i` and Phantom's
//! `m/44'/501'/i'/0'` -- so a phrase made here opens the same addresses
//! there, and one written down from there opens them here. EVM keys follow
//! BIP32 on secp256k1; Solana's follow SLIP-10 on Ed25519, which allows only
//! hardened steps.

use k256::ecdsa::SigningKey;
use k256::elliptic_curve::PrimeField;
use ring::hmac;

/// Where BIP32 hardens a step: indexes from here are hardened.
const HARDENED: u32 = 0x8000_0000;
/// Bytes of entropy in a new phrase: 16 is twelve words, what every wallet
/// app reads and most make.
const NEW_PHRASE_BYTES: usize = 16;

/// What a pasted secret turned out to be.
#[derive(Debug, PartialEq)]
pub(crate) enum Secret {
    /// A BIP39 recovery phrase, as normalized words.
    Phrase(String),
    /// A 32-byte secp256k1 key, from MetaMask's or another EVM app's export.
    Evm([u8; 32]),
    /// A Solana key's 32-byte seed, from Phantom's base58 export or a
    /// solana-keygen JSON file.
    Solana([u8; 32]),
}

/// A new twelve-word phrase from the system's random source.
pub(crate) fn new_phrase() -> Result<String, String> {
    let mut entropy = [0u8; NEW_PHRASE_BYTES];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut entropy)
        .map_err(|_| "The system gave no random bytes for a new wallet.".to_string())?;
    let phrase = bip39::Mnemonic::from_entropy(&entropy).map_err(|e| e.to_string())?;
    Ok(phrase.to_string())
}

/// What `text` holds: a phrase, an EVM key in hex, or a Solana key in base58
/// or as a JSON list of bytes.
pub(crate) fn read_secret(text: &str) -> Result<Secret, String> {
    let text = text.trim();
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() >= 12 {
        let phrase = bip39::Mnemonic::parse(words.join(" ").to_lowercase())
            .map_err(|e| format!("That is not a recovery phrase a wallet app would make ({e})."))?;
        return Ok(Secret::Phrase(phrase.to_string()));
    }
    let hex = text.strip_prefix("0x").unwrap_or(text);
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let key = from_hex(hex)?;
        let bytes: [u8; 32] = key.try_into().map_err(|_| "Not a 32-byte key.")?;
        SigningKey::from_slice(&bytes).map_err(|_| "That key is not a valid EVM key.")?;
        return Ok(Secret::Evm(bytes));
    }
    let bytes = if text.starts_with('[') {
        serde_json::from_str::<Vec<u8>>(text)
            .map_err(|_| "That list is not a solana-keygen key file.".to_string())?
    } else {
        super::solana::from_base58(text).map_err(|_| {
            "That is not a recovery phrase, an EVM key in hex, or a Solana key.".to_string()
        })?
    };
    // A Solana secret key is its 32-byte seed followed by its public key,
    // which must be the one the seed makes, or the export was cut or mixed.
    if bytes.len() != 64 {
        return Err("A Solana key is 64 bytes; this one is not.".into());
    }
    let seed: [u8; 32] = bytes[..32].try_into().map_err(|_| "Not a Solana key.")?;
    if solana_public(&seed)? != bytes[32..] {
        return Err("That Solana key's two halves do not belong together.".into());
    }
    Ok(Secret::Solana(seed))
}

/// The 64-byte seed a phrase opens, with no extra passphrase, as the wallet
/// apps use it.
pub(crate) fn seed(phrase: &str) -> Result<[u8; 64], String> {
    let phrase = bip39::Mnemonic::parse(phrase).map_err(|e| e.to_string())?;
    Ok(phrase.to_seed(""))
}

/// The EVM key at `m/44'/60'/0'/0/index` under `seed`, as MetaMask finds its
/// accounts.
pub(crate) fn evm_from_seed(seed: &[u8], index: u32) -> Result<[u8; 32], String> {
    let (mut key, mut chain) = split(&mac(b"Bitcoin seed", seed));
    for step in [44 | HARDENED, 60 | HARDENED, HARDENED, 0, index] {
        let mut data = Vec::with_capacity(37);
        if step >= HARDENED {
            data.push(0);
            data.extend_from_slice(&key);
        } else {
            let secret = SigningKey::from_slice(&key).map_err(|e| e.to_string())?;
            data.extend_from_slice(secret.verifying_key().to_sec1_point(true).as_bytes());
        }
        data.extend_from_slice(&step.to_be_bytes());
        let (tweak, next_chain) = split(&mac(&chain, &data));
        key = add_scalars(&tweak, &key)?;
        chain = next_chain;
    }
    Ok(key)
}

/// The Solana seed at `m/44'/501'/index'/0'` under `seed`, as Phantom finds
/// its accounts.
pub(crate) fn solana_from_seed(seed: &[u8], index: u32) -> [u8; 32] {
    let (mut key, mut chain) = split(&mac(b"ed25519 seed", seed));
    for step in [44, 501, index, 0] {
        let mut data = Vec::with_capacity(37);
        data.push(0);
        data.extend_from_slice(&key);
        data.extend_from_slice(&(step | HARDENED).to_be_bytes());
        (key, chain) = split(&mac(&chain, &data));
    }
    key
}

/// The checksummed 0x address of an EVM key.
pub(crate) fn evm_address(key: &[u8; 32]) -> Result<String, String> {
    let secret = SigningKey::from_slice(key).map_err(|e| e.to_string())?;
    Ok(address_of(secret.verifying_key()))
}

/// The checksummed 0x address of a public key.
pub(crate) fn address_of(public: &k256::ecdsa::VerifyingKey) -> String {
    let point = public.to_sec1_point(false);
    let hash = keccak(&point.as_bytes()[1..]);
    checksummed(&hash[12..])
}

/// An EVM address as EIP-55 writes it: each letter capitalized where the
/// matching nibble of the lowercase address's hash is 8 or more.
pub(crate) fn checksummed(address: &[u8]) -> String {
    let lower = to_hex(address);
    let hash = keccak(lower.as_bytes());
    let mixed: String = lower
        .chars()
        .enumerate()
        .map(|(at, c)| {
            let nibble = (hash[at / 2] >> if at % 2 == 0 { 4 } else { 0 }) & 0xf;
            if nibble >= 8 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    format!("0x{mixed}")
}

/// The 20 bytes of an EVM address written as 0x and 40 hex digits; a mixed
/// case one must carry a right checksum, as a typo would not.
pub(crate) fn evm_bytes(address: &str) -> Result<[u8; 20], String> {
    let bad = || format!("`{address}` is not an EVM address.");
    let hex = address.trim().strip_prefix("0x").ok_or_else(bad)?;
    if hex.len() != 40 {
        return Err(bad());
    }
    let bytes: [u8; 20] = from_hex(hex)
        .map_err(|_| bad())?
        .try_into()
        .map_err(|_| bad())?;
    let mixed =
        hex.chars().any(|c| c.is_ascii_lowercase()) && hex.chars().any(|c| c.is_ascii_uppercase());
    if mixed && checksummed(&bytes)[2..] != *hex {
        return Err(format!(
            "`{address}` has a wrong checksum; check it for a typo."
        ));
    }
    Ok(bytes)
}

/// The base58 address of a Solana seed.
pub(crate) fn solana_address(seed: &[u8; 32]) -> Result<String, String> {
    Ok(super::solana::to_base58(&solana_public(seed)?))
}

/// The 32-byte public key a Solana seed makes.
pub(crate) fn solana_public(seed: &[u8; 32]) -> Result<Vec<u8>, String> {
    use ring::signature::KeyPair;
    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| "That Solana key could not be read.".to_string())?;
    Ok(pair.public_key().as_ref().to_vec())
}

/// Keccak-256, Ethereum's hash (not SHA3-256, which pads differently).
pub(crate) fn keccak(bytes: &[u8]) -> [u8; 32] {
    use tiny_keccak::Hasher;
    let mut hasher = tiny_keccak::Keccak::v256();
    hasher.update(bytes);
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    out
}

/// Lowercase hex, without 0x.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Bytes from hex, with or without 0x.
pub(crate) fn from_hex(text: &str) -> Result<Vec<u8>, String> {
    let text = text.strip_prefix("0x").unwrap_or(text);
    if !text.len().is_multiple_of(2) {
        return Err("Odd-length hex.".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|at| {
            u8::from_str_radix(text.get(at..at + 2).unwrap_or("x"), 16)
                .map_err(|_| "Not hex.".to_string())
        })
        .collect()
}

/// HMAC-SHA512 of `data` under `key`.
fn mac(key: &[u8], data: &[u8]) -> [u8; 64] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA512, key), data);
    let mut out = [0u8; 64];
    out.copy_from_slice(tag.as_ref());
    out
}

/// The two halves of a 64-byte MAC: a key and a chain code.
fn split(both: &[u8; 64]) -> ([u8; 32], [u8; 32]) {
    let mut left = [0u8; 32];
    let mut right = [0u8; 32];
    left.copy_from_slice(&both[..32]);
    right.copy_from_slice(&both[32..]);
    (left, right)
}

/// `a + b` modulo the curve's order, refused where BIP32 says to skip the
/// index: a tweak past the order, or a sum of zero (each one in 2^127).
fn add_scalars(a: &[u8; 32], b: &[u8; 32]) -> Result<[u8; 32], String> {
    let scalar = |bytes: &[u8; 32]| {
        Option::<k256::Scalar>::from(k256::Scalar::from_repr((*bytes).into()))
            .ok_or_else(|| "That index makes no key; try the next one.".to_string())
    };
    let sum = scalar(a)? + scalar(b)?;
    if bool::from(sum.is_zero()) {
        return Err("That index makes no key; try the next one.".into());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&sum.to_repr());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phrase every wallet library tests with; its addresses are known.
    const ABANDON: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn the_test_phrase_opens_the_addresses_metamask_and_phantom_show() {
        let seed = seed(ABANDON).unwrap();
        let evm = evm_from_seed(&seed, 0).unwrap();
        assert_eq!(
            evm_address(&evm).unwrap(),
            "0x9858EfFD232B4033E47d90003D41EC34EcaEda94"
        );
        let second = evm_from_seed(&seed, 1).unwrap();
        assert_eq!(
            evm_address(&second).unwrap(),
            "0x6Fac4D18c912343BF86fa7049364Dd4E424Ab9C0"
        );
        let solana = solana_from_seed(&seed, 0);
        assert_eq!(
            solana_address(&solana).unwrap(),
            "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk"
        );
    }

    #[test]
    fn a_new_phrase_is_twelve_words_that_read_back() {
        let phrase = new_phrase().unwrap();
        assert_eq!(phrase.split(' ').count(), 12);
        assert_eq!(
            read_secret(&phrase).unwrap(),
            Secret::Phrase(phrase.clone())
        );
        assert_ne!(new_phrase().unwrap(), phrase);
    }

    #[test]
    fn a_pasted_secret_is_told_apart_by_its_shape() {
        // Capitals and extra spaces, as a phrase is often pasted.
        let shouted = format!("  {}  ", ABANDON.to_uppercase().replace(' ', "   "));
        assert_eq!(
            read_secret(&shouted).unwrap(),
            Secret::Phrase(ABANDON.into())
        );
        let mut words: Vec<&str> = ABANDON.split(' ').collect();
        words[11] = "abandon";
        assert!(
            read_secret(&words.join(" "))
                .unwrap_err()
                .contains("recovery phrase")
        );

        let key = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let Secret::Evm(bytes) = read_secret(&format!("0x{key}")).unwrap() else {
            panic!("not read as an EVM key");
        };
        assert_eq!(to_hex(&bytes), key);
        assert_eq!(
            evm_address(&bytes).unwrap(),
            "0x2c7536E3605D9C16a7a3D7b1898e529396a65c23"
        );

        let seed = solana_from_seed(&super::seed(ABANDON).unwrap(), 0);
        let mut full = seed.to_vec();
        full.extend(solana_public(&seed).unwrap());
        let phantom = super::super::solana::to_base58(&full);
        assert_eq!(read_secret(&phantom).unwrap(), Secret::Solana(seed));
        let keygen = serde_json::to_string(&full).unwrap();
        assert_eq!(read_secret(&keygen).unwrap(), Secret::Solana(seed));
        // The public half from another key.
        full[40] ^= 1;
        let mixed = super::super::solana::to_base58(&full);
        assert!(read_secret(&mixed).unwrap_err().contains("halves"));
        assert!(read_secret("hello").is_err());
    }

    #[test]
    fn an_address_with_a_wrong_checksum_is_refused_and_lowercase_is_fine() {
        let good = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";
        assert!(evm_bytes(good).is_ok());
        assert!(evm_bytes(&good.to_lowercase()).is_ok());
        assert!(
            evm_bytes(&good.replace("Ef", "eF"))
                .unwrap_err()
                .contains("checksum")
        );
        assert!(evm_bytes("0x1234").is_err());
        assert!(evm_bytes("9858EfFD232B4033E47d90003D41EC34EcaEda94").is_err());
    }

    #[test]
    fn keccak_is_ethereums_and_not_sha3() {
        assert_eq!(
            to_hex(&keccak(b"")),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }
}
