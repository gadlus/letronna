//! Solana: addresses in base58, and transactions built, signed and sent by
//! hand over JSON-RPC.
//!
//! Hand-built because the Solana SDK is hundreds of crates for what is here a
//! few hundred lines: a v0 message with no lookup tables, the System, Token
//! and associated-token-account instructions a payment needs, and Ed25519
//! from ring. A token is held in an account derived from the owner and the
//! mint (an ATA), so a payment moves it between two such accounts, creating
//! the receiver's when it has none -- except in an x402 payment, where the
//! site's must already exist, as its fee payer would otherwise pay its rent.

use super::chains::{Chain, Token};
use super::rpc;
use serde_json::{Value, json};

/// The System program, which moves SOL: 32 zero bytes.
const SYSTEM: [u8; 32] = [0; 32];
/// The SPL Token program, `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`.
pub(crate) const TOKEN: [u8; 32] = [
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237,
    95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
];
/// Token-2022, `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`, which newer
/// mints such as PYUSD live under.
pub(crate) const TOKEN_2022: [u8; 32] = [
    6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218, 182, 26, 252, 77,
    131, 185, 13, 39, 254, 189, 249, 40, 216, 161, 139, 252,
];
/// The associated token account program, `ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL`.
const ATA: [u8; 32] = [
    140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131, 11, 90, 19, 153, 218,
    255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89,
];
/// The compute budget program, `ComputeBudget111111111111111111111111111111`.
const COMPUTE_BUDGET: [u8; 32] = [
    3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231, 188, 140, 229, 187,
    197, 247, 18, 107, 44, 67, 155, 58, 64, 0, 0, 0,
];
/// The memo program, `MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr`.
const MEMO: [u8; 32] = [
    5, 74, 83, 90, 153, 41, 33, 6, 77, 36, 232, 113, 96, 218, 56, 124, 124, 53, 181, 221, 188, 146,
    187, 129, 228, 31, 168, 64, 65, 5, 68, 141,
];
/// Compute units an x402 payment asks for: a checked transfer and a memo
/// take about 10k; the spec allows up to 400k.
const X402_UNITS: u32 = 40_000;
/// Its priority fee, in micro-lamports a unit: the floor the spec's
/// facilitators accept, since the site's fee payer pays it.
const X402_PRICE: u64 = 1;
/// How long a sent transaction is watched for before it is reported as
/// sent but not yet seen confirmed.
const CONFIRM_TIME: std::time::Duration = std::time::Duration::from_secs(45);

/// The base58 alphabet Solana and Bitcoin write addresses in.
const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// `bytes` in base58, a leading zero byte as `1`.
pub(crate) fn to_base58(bytes: &[u8]) -> String {
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    // Base-58 digits, least significant first.
    let mut digits: Vec<u8> = Vec::new();
    for &byte in &bytes[zeros..] {
        let mut carry = byte as u32;
        for digit in &mut digits {
            carry += (*digit as u32) << 8;
            *digit = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = "1".repeat(zeros);
    out.extend(digits.iter().rev().map(|&d| BASE58[d as usize] as char));
    out
}

/// The bytes a base58 string holds.
pub(crate) fn from_base58(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    let zeros = text.chars().take_while(|&c| c == '1').count();
    // Bytes, least significant first.
    let mut bytes: Vec<u8> = Vec::new();
    for c in text[zeros..].chars() {
        let Some(value) = BASE58.iter().position(|&d| d as char == c) else {
            return Err(format!("`{c}` is not a base58 digit."));
        };
        let mut carry = value as u32;
        for byte in &mut bytes {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let mut out = vec![0u8; zeros];
    out.extend(bytes.iter().rev());
    Ok(out)
}

/// The 32-byte key a Solana address names.
pub(crate) fn key(address: &str) -> Result<[u8; 32], String> {
    from_base58(address)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("`{address}` is not a Solana address."))
}

/// One account an instruction touches.
#[derive(Clone, Debug, PartialEq)]
struct Meta {
    key: [u8; 32],
    signer: bool,
    writable: bool,
}

/// One instruction: a program, the accounts it reads or writes, its data.
#[derive(Clone, Debug, PartialEq)]
struct Instruction {
    program: [u8; 32],
    accounts: Vec<Meta>,
    data: Vec<u8>,
}

fn meta(key: [u8; 32], signer: bool, writable: bool) -> Meta {
    Meta {
        key,
        signer,
        writable,
    }
}

/// A v0 message, the part of a transaction its signers sign, and how many
/// signatures it needs.
fn message(payer: [u8; 32], instructions: &[Instruction], blockhash: [u8; 32]) -> (Vec<u8>, usize) {
    // Every key once, with the strongest use any instruction makes of it;
    // the fee payer first, as the one that signs and pays.
    let mut keys: Vec<Meta> = vec![meta(payer, true, true)];
    let mut add = |wanted: &Meta| match keys.iter_mut().find(|k| k.key == wanted.key) {
        Some(known) => {
            known.signer |= wanted.signer;
            known.writable |= wanted.writable;
        }
        None => keys.push(wanted.clone()),
    };
    for instruction in instructions {
        instruction.accounts.iter().for_each(&mut add);
        add(&meta(instruction.program, false, false));
    }
    // Signers that write, signers that only read, then the same for the
    // rest: the order the header's three counts describe. A stable sort
    // keeps the payer first.
    keys.sort_by_key(|k| (!k.signer, !k.writable));
    let signers = keys.iter().filter(|k| k.signer).count();
    let read_signed = keys.iter().filter(|k| k.signer && !k.writable).count();
    let read_unsigned = keys.iter().filter(|k| !k.signer && !k.writable).count();
    let index = |key: &[u8; 32]| keys.iter().position(|k| k.key == *key).unwrap_or(0) as u8;

    let mut out = vec![0x80, signers as u8, read_signed as u8, read_unsigned as u8];
    compact(&mut out, keys.len());
    for k in &keys {
        out.extend_from_slice(&k.key);
    }
    out.extend_from_slice(&blockhash);
    compact(&mut out, instructions.len());
    for instruction in instructions {
        out.push(index(&instruction.program));
        compact(&mut out, instruction.accounts.len());
        out.extend(instruction.accounts.iter().map(|a| index(&a.key)));
        compact(&mut out, instruction.data.len());
        out.extend_from_slice(&instruction.data);
    }
    // No address lookup tables.
    compact(&mut out, 0);
    (out, signers)
}

/// A whole transaction: a slot for each signature, ours put in at `slot`
/// and the rest left zero for their signers to fill, then the message.
fn transaction(
    message: &[u8],
    signers: usize,
    slot: usize,
    seed: &[u8; 32],
) -> Result<Vec<u8>, String> {
    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| "The Solana key could not sign.".to_string())?;
    let mut out = Vec::new();
    compact(&mut out, signers);
    for at in 0..signers {
        if at == slot {
            out.extend_from_slice(pair.sign(message).as_ref());
        } else {
            out.extend_from_slice(&[0u8; 64]);
        }
    }
    out.extend_from_slice(message);
    Ok(out)
}

/// `n` as Solana's compact-u16: seven bits a byte, low first.
fn compact(out: &mut Vec<u8>, n: usize) {
    let mut n = n;
    loop {
        let low = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// The address a program derives from `seeds`, with the highest bump that
/// lands off the curve, so no key can sign for it.
fn program_address(seeds: &[&[u8]], program: &[u8; 32]) -> Result<[u8; 32], String> {
    for bump in (0..=255u8).rev() {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        for seed in seeds {
            hash.update(seed);
        }
        hash.update(&[bump]);
        hash.update(program);
        hash.update(b"ProgramDerivedAddress");
        let mut address = [0u8; 32];
        address.copy_from_slice(hash.finish().as_ref());
        let on_curve = curve25519_dalek::edwards::CompressedEdwardsY(address)
            .decompress()
            .is_some();
        if !on_curve {
            return Ok(address);
        }
    }
    Err("No program address could be found.".into())
}

/// The account `owner` holds the `mint` token in, under `program`.
pub(crate) fn token_account(
    owner: &[u8; 32],
    mint: &[u8; 32],
    program: &[u8; 32],
) -> Result<[u8; 32], String> {
    program_address(&[owner, program, mint], &ATA)
}

fn transfer_sol(from: [u8; 32], to: [u8; 32], lamports: u64) -> Instruction {
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program: SYSTEM,
        accounts: vec![meta(from, true, true), meta(to, false, true)],
        data,
    }
}

fn create_token_account(
    payer: [u8; 32],
    owner: [u8; 32],
    mint: [u8; 32],
    program: [u8; 32],
) -> Result<Instruction, String> {
    Ok(Instruction {
        program: ATA,
        accounts: vec![
            meta(payer, true, true),
            meta(token_account(&owner, &mint, &program)?, false, true),
            meta(owner, false, false),
            meta(mint, false, false),
            meta(SYSTEM, false, false),
            meta(program, false, false),
        ],
        // CreateIdempotent: nothing happens when the account is there.
        data: vec![1],
    })
}

/// A transfer that names the mint and its decimals, so the program checks
/// both: what x402 requires, and safer than a plain transfer.
fn transfer_checked(
    source: [u8; 32],
    mint: [u8; 32],
    destination: [u8; 32],
    owner: [u8; 32],
    amount: u64,
    decimals: u8,
    program: [u8; 32],
) -> Instruction {
    let mut data = vec![12];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);
    Instruction {
        program,
        accounts: vec![
            meta(source, false, true),
            meta(mint, false, false),
            meta(destination, false, true),
            meta(owner, true, false),
        ],
        data,
    }
}

fn compute_limit(units: u32) -> Instruction {
    let mut data = vec![2];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program: COMPUTE_BUDGET,
        accounts: Vec::new(),
        data,
    }
}

fn compute_price(micro_lamports: u64) -> Instruction {
    let mut data = vec![3];
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program: COMPUTE_BUDGET,
        accounts: Vec::new(),
        data,
    }
}

fn memo(text: &str) -> Instruction {
    Instruction {
        program: MEMO,
        accounts: Vec::new(),
        data: text.as_bytes().to_vec(),
    }
}

/// The lamports `owner` holds.
pub(crate) fn balance(chain: &Chain, owner: &str) -> Result<u128, String> {
    let answer = rpc(
        chain,
        "getBalance",
        json!([owner, {"commitment": "confirmed"}]),
    )?;
    answer["value"]
        .as_u64()
        .map(u128::from)
        .ok_or_else(|| format!("{} gave no balance.", chain.name))
}

/// What `owner` holds of `token`, in its smallest unit, across every account
/// it holds it in.
pub(crate) fn token_balance(chain: &Chain, owner: &str, token: &Token) -> Result<u128, String> {
    let answer = rpc(
        chain,
        "getTokenAccountsByOwner",
        json!([owner, {"mint": token.address}, {"encoding": "jsonParsed", "commitment": "confirmed"}]),
    )?;
    let accounts = answer["value"].as_array().cloned().unwrap_or_default();
    Ok(accounts
        .iter()
        .filter_map(|account| {
            account["account"]["data"]["parsed"]["info"]["tokenAmount"]["amount"]
                .as_str()?
                .parse::<u128>()
                .ok()
        })
        .sum())
}

/// The program a mint lives under and its decimals, read from the chain.
fn mint_of(chain: &Chain, mint: &str) -> Result<([u8; 32], u8), String> {
    let answer = rpc(
        chain,
        "getAccountInfo",
        json!([mint, {"encoding": "base64"}]),
    )?;
    let value = &answer["value"];
    if value.is_null() {
        return Err(format!("There is no token `{mint}` on {}.", chain.name));
    }
    let program = key(value["owner"].as_str().unwrap_or_default())?;
    if program != TOKEN && program != TOKEN_2022 {
        return Err(format!("`{mint}` is not a token mint."));
    }
    let data = value["data"][0].as_str().unwrap_or_default();
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
        .map_err(|_| "The mint's data could not be read.".to_string())?;
    // A mint is its authority option (36 bytes), then its supply (8), then
    // its decimals.
    let decimals = *bytes.get(44).ok_or("The mint's data is too short.")?;
    Ok((program, decimals))
}

/// The decimals of the token `mint`, for one the user adds.
pub(crate) fn decimals(chain: &Chain, mint: &str) -> Result<u8, String> {
    mint_of(chain, mint).map(|(_, decimals)| decimals)
}

/// Whether an account exists at `address`.
fn exists(chain: &Chain, address: &[u8; 32]) -> Result<bool, String> {
    let answer = rpc(
        chain,
        "getAccountInfo",
        json!([to_base58(address), {"encoding": "base64"}]),
    )?;
    Ok(!answer["value"].is_null())
}

/// A recent blockhash, which a transaction must name to be taken.
fn blockhash(chain: &Chain) -> Result<[u8; 32], String> {
    let answer = rpc(
        chain,
        "getLatestBlockhash",
        json!([{"commitment": "finalized"}]),
    )?;
    key(answer["value"]["blockhash"].as_str().unwrap_or_default())
}

/// Send `amount` of SOL, or of `token`, from the key `seed` to `to`, and
/// give the transaction's signature once it is confirmed, or as soon as it
/// is sent when the network is slow to say.
pub(crate) fn send(
    chain: &Chain,
    seed: &[u8; 32],
    to: &str,
    amount: u64,
    token: Option<&Token>,
) -> Result<String, String> {
    let from: [u8; 32] = super::keys::solana_public(seed)?
        .try_into()
        .map_err(|_| "Not a Solana key.")?;
    let to = key(to)?;
    let instructions = match token {
        None => vec![transfer_sol(from, to, amount)],
        Some(token) => {
            let mint = key(&token.address)?;
            let (program, decimals) = mint_of(chain, &token.address)?;
            vec![
                create_token_account(from, to, mint, program)?,
                transfer_checked(
                    token_account(&from, &mint, &program)?,
                    mint,
                    token_account(&to, &mint, &program)?,
                    from,
                    amount,
                    decimals,
                    program,
                ),
            ]
        }
    };
    let (message, signers) = message(from, &instructions, blockhash(chain)?);
    let signed = transaction(&message, signers, 0, seed)?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &signed);
    let answer = rpc(
        chain,
        "sendTransaction",
        json!([encoded, {"encoding": "base64", "preflightCommitment": "confirmed"}]),
    )?;
    let signature = answer
        .as_str()
        .ok_or_else(|| format!("{} did not take the transaction.", chain.name))?
        .to_owned();
    confirm(chain, &signature)?;
    Ok(signature)
}

/// Wait for `signature` to be confirmed; a failed transaction is the error,
/// and one still pending when the wait ends is left to finish on its own.
fn confirm(chain: &Chain, signature: &str) -> Result<(), String> {
    let until = std::time::Instant::now() + CONFIRM_TIME;
    while std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let Ok(answer) = rpc(chain, "getSignatureStatuses", json!([[signature]])) else {
            continue;
        };
        let status = &answer["value"][0];
        if !status["err"].is_null() {
            return Err(format!(
                "The transaction failed on {}: {}",
                chain.name, status["err"]
            ));
        }
        if matches!(
            status["confirmationStatus"].as_str(),
            Some("confirmed" | "finalized")
        ) {
            return Ok(());
        }
    }
    Ok(())
}

/// The transaction an x402 site asked for, signed by us and waiting for the
/// fee payer the site named: a checked transfer of `amount` of `mint` to the
/// account `pay_to` holds it in, with the site's memo or a random one.
pub(crate) fn x402_payment(
    chain: &Chain,
    seed: &[u8; 32],
    extra: &Value,
    pay_to: &str,
    mint: &str,
    amount: u64,
) -> Result<Vec<u8>, String> {
    let owner: [u8; 32] = super::keys::solana_public(seed)?
        .try_into()
        .map_err(|_| "Not a Solana key.")?;
    let fee_payer = key(extra["feePayer"].as_str().unwrap_or_default())
        .map_err(|_| "The site named no fee payer for its Solana payment.".to_string())?;
    let mint_key = key(mint)?;
    let (program, decimals) = mint_of(chain, mint)?;
    let destination = token_account(&key(pay_to)?, &mint_key, &program)?;
    if !exists(chain, &destination)? {
        return Err(format!(
            "The site has no account for this token on {}, so it cannot be paid there.",
            chain.name
        ));
    }
    let recent = match extra["recentBlockhash"].as_str() {
        Some(hash) => key(hash)?,
        None => blockhash(chain)?,
    };
    let note = match extra["memo"].as_str() {
        Some(note) => note.to_owned(),
        None => super::keys::to_hex(&super::random::<16>()?),
    };
    let instructions = [
        compute_limit(X402_UNITS),
        compute_price(X402_PRICE),
        transfer_checked(
            token_account(&owner, &mint_key, &program)?,
            mint_key,
            destination,
            owner,
            amount,
            decimals,
            program,
        ),
        memo(&note),
    ];
    let (message, signers) = message(fee_payer, &instructions, recent);
    // The fee payer is first; we are the only other signer.
    transaction(&message, signers, 1, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base58_round_trips_and_keeps_leading_zeros() {
        for bytes in [
            vec![],
            vec![0],
            vec![0, 0, 1, 2, 3],
            vec![255; 32],
            (0..64).collect(),
        ] {
            assert_eq!(from_base58(&to_base58(&bytes)).unwrap(), bytes);
        }
        assert_eq!(to_base58(b"hello world"), "StV1DL6CwTryKyV");
        assert_eq!(to_base58(&[0, 0, 0x28, 0x7f, 0xb4, 0xcd]), "11233QC4");
        assert!(from_base58("0OIl").is_err());
    }

    #[test]
    fn the_program_ids_are_the_addresses_they_say() {
        assert_eq!(to_base58(&SYSTEM), "11111111111111111111111111111111");
        assert_eq!(
            to_base58(&TOKEN),
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        );
        assert_eq!(
            to_base58(&TOKEN_2022),
            "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        );
        assert_eq!(
            to_base58(&ATA),
            "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        );
        assert_eq!(
            to_base58(&COMPUTE_BUDGET),
            "ComputeBudget111111111111111111111111111111"
        );
        assert_eq!(
            to_base58(&MEMO),
            "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr"
        );
    }

    #[test]
    fn compact_u16_is_seven_bits_a_byte() {
        let encoded = |n| {
            let mut out = Vec::new();
            compact(&mut out, n);
            out
        };
        assert_eq!(encoded(0), [0]);
        assert_eq!(encoded(0x7f), [0x7f]);
        assert_eq!(encoded(0x80), [0x80, 0x01]);
        assert_eq!(encoded(0x3fff), [0xff, 0x7f]);
        assert_eq!(encoded(0x4000), [0x80, 0x80, 0x01]);
    }

    #[test]
    fn a_token_account_is_where_wallets_find_it() {
        // The USDC account of the test phrase's Phantom address, as worked out
        // apart from this code: SHA-256 and RFC 8032's point decoding.
        let owner = key("HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk").unwrap();
        let usdc = key("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let account = token_account(&owner, &usdc, &TOKEN).unwrap();
        assert_eq!(
            to_base58(&account),
            "5N3f1tj9v1vc5TUZ8S7mCAnVmjVKrfnzXWhxLaxyZAgt"
        );
    }

    #[test]
    fn an_x402_transaction_leaves_the_fee_payer_its_slot_and_is_signed_by_us() {
        let seed = [7u8; 32];
        let owner: [u8; 32] = super::super::keys::solana_public(&seed)
            .unwrap()
            .try_into()
            .unwrap();
        let payer = [9u8; 32];
        let mint = key("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let to = [5u8; 32];
        let instructions = [
            compute_limit(X402_UNITS),
            compute_price(X402_PRICE),
            transfer_checked(
                token_account(&owner, &mint, &TOKEN).unwrap(),
                mint,
                token_account(&to, &mint, &TOKEN).unwrap(),
                owner,
                1_000,
                6,
                TOKEN,
            ),
            memo("note"),
        ];
        let (message, signers) = message(payer, &instructions, [1u8; 32]);
        // v0, two signers, we only read; the mint and three programs only read.
        assert_eq!(&message[..4], &[0x80, 2, 1, 4]);
        assert_eq!(&message[5..37], &payer);
        assert_eq!(&message[37..69], &owner);
        let signed = transaction(&message, signers, 1, &seed).unwrap();
        assert_eq!(signed[0], 2);
        assert_eq!(&signed[1..65], &[0u8; 64]);
        let public = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, owner);
        assert!(public.verify(&signed[129..], &signed[65..129]).is_ok());
        assert_eq!(&signed[129..], &message[..]);
    }

    #[test]
    #[ignore = "reads Solana devnet; run with --ignored when online"]
    fn devnet_answers_with_a_blockhash_and_a_balance() {
        let chains = super::super::chains::all(&super::super::Wallet::default());
        let devnet = super::super::chains::find(&chains, "solana-devnet").unwrap();
        assert_ne!(blockhash(devnet).unwrap(), [0u8; 32]);
        balance(devnet, "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk").unwrap();
        let usdc = devnet.token("usdc").unwrap();
        assert_eq!(mint_of(devnet, &usdc.address).unwrap(), (TOKEN, 6));
    }
}
