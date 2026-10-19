//! EVM chains: balances read over JSON-RPC, the EIP-712 signature an x402
//! payment is, and plain transfers sent as EIP-1559 transactions.
//!
//! The same secp256k1 key and the same calls serve every EVM chain, so this
//! is written once against a chain's id and RPC address rather than a
//! library per network. An x402 payment is gasless for the payer: it is an
//! EIP-3009 `transferWithAuthorization` signed off-chain, which the site's
//! facilitator submits and pays the gas for. Amounts are u128 -- 3.4e38 of a
//! token's smallest unit, far past any balance a person holds -- and are
//! written into the 32-byte words the chain uses with the top half zero.

use super::chains::{Chain, Token};
use super::keys::{evm_bytes, from_hex, keccak, to_hex};
use super::{Unsent, broadcast, rpc};
use k256::ecdsa::SigningKey;
use serde_json::{Value, json};

/// How long a sent transaction is watched for before it is reported as sent
/// but not yet mined.
const RECEIPT_TIME: std::time::Duration = std::time::Duration::from_secs(60);
/// Gas a plain transfer of the native coin to an ordinary account takes.
const TRANSFER_GAS: u128 = 21_000;
/// The tip offered when a chain will not suggest one: 0.1 gwei, which L2s
/// take and L1s still include.
const FALLBACK_TIP: u128 = 100_000_000;
/// The most a send's network fee may come to, in micro-dollars. A transfer
/// costs under a cent on an L2 and a dollar or two on Ethereum in an
/// ordinary hour, while the fee is whatever the RPC answers: one that
/// answered wildly could spend the whole balance on gas.
pub(crate) const FEE_CEILING: u64 = 5 * super::policy::DOLLAR;

/// ERC-20 `balanceOf(address)`.
const BALANCE_OF: &str = "70a08231";
/// ERC-20 `transfer(address,uint256)`.
const TRANSFER: &str = "a9059cbb";
/// ERC-20 `decimals()`.
const DECIMALS: &str = "313ce567";
/// ERC-20 `symbol()`.
const SYMBOL: &str = "95d89b41";

/// What an EIP-3009 authorization lets the token contract move.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Authorization {
    pub from: [u8; 20],
    pub to: [u8; 20],
    pub value: u128,
    pub valid_after: u64,
    pub valid_before: u64,
    pub nonce: [u8; 32],
}

/// The native coin `address` holds, in wei.
pub(crate) fn balance(chain: &Chain, address: &str) -> Result<u128, String> {
    let answer = rpc(chain, "eth_getBalance", json!([address, "latest"]))?;
    quantity(&answer)
}

/// What `address` holds of `token`, in its smallest unit.
pub(crate) fn token_balance(chain: &Chain, token: &Token, address: &str) -> Result<u128, String> {
    let call = format!("0x{BALANCE_OF}{}", word_of_address(&evm_bytes(address)?));
    let answer = eth_call(chain, &token.address, &call)?;
    word(&answer)
}

/// The decimals and symbol a token contract reports, for one the user adds.
pub(crate) fn describe(chain: &Chain, contract: &str) -> Result<(u8, String), String> {
    evm_bytes(contract)?;
    let decimals = word(&eth_call(chain, contract, &format!("0x{DECIMALS}"))?)?;
    let symbol = text(&eth_call(chain, contract, &format!("0x{SYMBOL}"))?);
    let decimals = u8::try_from(decimals).map_err(|_| "That contract's decimals make no sense.")?;
    Ok((decimals, symbol))
}

/// The signature over `authorization` an x402 site takes as payment: EIP-712
/// typed data under the token's own name and version, 65 bytes as r, s, v.
pub(crate) fn authorize(
    key: &[u8; 32],
    chain_id: u64,
    contract: &str,
    name: &str,
    version: &str,
    authorization: &Authorization,
) -> Result<String, String> {
    let domain = domain_separator(name, version, chain_id, &evm_bytes(contract)?);
    let digest = typed_digest(&domain, &transfer_hash(authorization));
    let (signature, recovery) = sign(key, &digest)?;
    let mut out = signature.to_vec();
    out.push(27 + recovery);
    Ok(format!("0x{}", to_hex(&out)))
}

/// The EIP-712 domain separator of a token contract.
fn domain_separator(name: &str, version: &str, chain_id: u64, contract: &[u8; 20]) -> [u8; 32] {
    let mut data = keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    )
    .to_vec();
    data.extend(keccak(name.as_bytes()));
    data.extend(keccak(version.as_bytes()));
    data.extend(uint(chain_id as u128));
    data.extend(address_word(contract));
    keccak(&data)
}

/// The EIP-712 hash of a `TransferWithAuthorization`.
fn transfer_hash(authorization: &Authorization) -> [u8; 32] {
    let mut data = keccak(b"TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)").to_vec();
    data.extend(address_word(&authorization.from));
    data.extend(address_word(&authorization.to));
    data.extend(uint(authorization.value));
    data.extend(uint(authorization.valid_after as u128));
    data.extend(uint(authorization.valid_before as u128));
    data.extend(authorization.nonce);
    keccak(&data)
}

/// The digest an authorization is signed over, for tests elsewhere that
/// check a payment as a facilitator would.
#[cfg(test)]
pub(crate) fn test_digest(
    name: &str,
    version: &str,
    chain_id: u64,
    contract: &str,
    authorization: &Authorization,
) -> [u8; 32] {
    let contract = evm_bytes(contract).unwrap_or_default();
    typed_digest(
        &domain_separator(name, version, chain_id, &contract),
        &transfer_hash(authorization),
    )
}

/// What is signed for typed data: `0x1901`, the domain, then the struct.
fn typed_digest(domain: &[u8; 32], hash: &[u8; 32]) -> [u8; 32] {
    let mut data = vec![0x19, 0x01];
    data.extend(domain);
    data.extend(hash);
    keccak(&data)
}

/// A signature over a 32-byte digest, low-s as Ethereum requires, with the
/// recovery id that names the public key.
fn sign(key: &[u8; 32], digest: &[u8; 32]) -> Result<([u8; 64], u8), String> {
    let secret = SigningKey::from_slice(key).map_err(|_| "The EVM key could not sign.")?;
    let (signature, recovery) = secret.sign_prehash_recoverable(digest);
    let mut out = [0u8; 64];
    out.copy_from_slice(&signature.to_bytes());
    Ok((out, recovery.to_byte()))
}

/// Send `amount` of the native coin, or of `token`, from `key` to `to`, and
/// give the transaction's hash once it is mined, or as soon as it is sent
/// when the network is slow to mine it.
pub(crate) fn send(
    chain: &Chain,
    key: &[u8; 32],
    to: &str,
    amount: u128,
    token: Option<&Token>,
) -> Result<String, Unsent> {
    let chain_id = chain
        .evm_id()
        .ok_or_else(|| format!("{} has no EVM chain id.", chain.name))?;
    let secret = SigningKey::from_slice(key).map_err(|_| "The EVM key could not sign.")?;
    let from = super::keys::address_of(secret.verifying_key());
    let recipient = evm_bytes(to)?;
    let (target, value, data) = match token {
        None => (recipient, amount, Vec::new()),
        Some(token) => {
            let mut data = from_hex(TRANSFER)?;
            data.extend(address_word(&recipient));
            data.extend(uint(amount));
            (evm_bytes(&token.address)?, 0, data)
        }
    };
    let nonce = quantity(&rpc(
        chain,
        "eth_getTransactionCount",
        json!([from, "pending"]),
    )?)?;
    let gas = gas_for(chain, &from, &target, value, &data)?;
    let fees = fees(chain)?;
    let per_gas = match fees {
        Fees::Eip1559 { max, .. } => max,
        Fees::Legacy { price } => price,
    };
    within_ceiling(chain, gas, per_gas, |wei| {
        super::policy::worth(wei, chain.decimals, false, &chain.symbol)
    })?;
    let call = Call {
        chain_id,
        nonce,
        gas,
        fees,
        to: target,
        value,
        data,
    };
    let raw = signed(key, &call)?;
    let hash = format!("0x{}", to_hex(&keccak(&raw)));
    let raw = format!("0x{}", to_hex(&raw));
    let hash = broadcast(chain, "eth_sendRawTransaction", json!([raw]), &hash)?;
    let hash = hash
        .as_str()
        .ok_or_else(|| format!("{} did not take the transaction.", chain.name))?
        .to_owned();
    receipt(chain, &hash)?;
    Ok(hash)
}

/// Everything a transaction says, before it is signed.
struct Call {
    chain_id: u64,
    nonce: u128,
    gas: u128,
    fees: Fees,
    to: [u8; 20],
    value: u128,
    data: Vec<u8>,
}

/// `call` signed by `key`, as the bytes `eth_sendRawTransaction` takes: an
/// EIP-1559 transaction, or an EIP-155 one on a chain without a base fee.
fn signed(key: &[u8; 32], call: &Call) -> Result<Vec<u8>, String> {
    let Call {
        chain_id,
        nonce,
        gas,
        fees,
        to: target,
        value,
        ref data,
    } = *call;
    let fields = |signed: Option<([u8; 64], u8)>| -> Vec<u8> {
        let mut items = vec![];
        match fees {
            Fees::Eip1559 { tip, max } => {
                items.extend([
                    rlp_uint(chain_id as u128),
                    rlp_uint(nonce),
                    rlp_uint(tip),
                    rlp_uint(max),
                    rlp_uint(gas),
                    rlp_bytes(&target),
                    rlp_uint(value),
                    rlp_bytes(data),
                    rlp_list(&[]),
                ]);
                if let Some((signature, recovery)) = signed {
                    items.extend([
                        rlp_uint(recovery as u128),
                        rlp_bytes(trimmed(&signature[..32])),
                        rlp_bytes(trimmed(&signature[32..])),
                    ]);
                }
                let mut out = vec![0x02];
                out.extend(rlp_list(&items));
                out
            }
            Fees::Legacy { price } => {
                items.extend([
                    rlp_uint(nonce),
                    rlp_uint(price),
                    rlp_uint(gas),
                    rlp_bytes(&target),
                    rlp_uint(value),
                    rlp_bytes(data),
                ]);
                // EIP-155: the chain id stands in for v until it is signed.
                match signed {
                    Some((signature, recovery)) => items.extend([
                        rlp_uint(recovery as u128 + chain_id as u128 * 2 + 35),
                        rlp_bytes(trimmed(&signature[..32])),
                        rlp_bytes(trimmed(&signature[32..])),
                    ]),
                    None => items.extend([rlp_uint(chain_id as u128), rlp_uint(0), rlp_uint(0)]),
                }
                rlp_list(&items)
            }
        }
    };
    let signature = sign(key, &keccak(&fields(None)))?;
    Ok(fields(Some(signature)))
}

/// What a chain charges for gas now.
#[derive(Clone, Copy, Debug)]
enum Fees {
    /// A tip and a cap per gas: twice the base fee plus the tip leaves room
    /// for six full blocks of rises before it is stuck.
    Eip1559 { tip: u128, max: u128 },
    /// One price per gas, on a chain whose blocks carry no base fee.
    Legacy { price: u128 },
}

fn fees(chain: &Chain) -> Result<Fees, String> {
    let block = rpc(chain, "eth_getBlockByNumber", json!(["latest", false]))?;
    let Some(base) = block["baseFeePerGas"].as_str() else {
        let price = quantity(&rpc(chain, "eth_gasPrice", json!([]))?)?;
        return Ok(Fees::Legacy { price });
    };
    let base = quantity(&Value::from(base))?;
    let tip = rpc(chain, "eth_maxPriorityFeePerGas", json!([]))
        .and_then(|tip| quantity(&tip))
        .unwrap_or(FALLBACK_TIP);
    let max = (base.checked_mul(2))
        .and_then(|twice| twice.checked_add(tip))
        .ok_or_else(|| format!("{} answered an impossible fee.", chain.name))?;
    Ok(Fees::Eip1559 { tip, max })
}

/// Refuse a send whose fee, `gas` at `per_gas` at most, could pass
/// `FEE_CEILING`, at what the chain's coin is `worth`.
fn within_ceiling(
    chain: &Chain,
    gas: u128,
    per_gas: u128,
    worth: impl Fn(u128) -> Option<u64>,
) -> Result<(), String> {
    let ceiling = super::policy::dollars(FEE_CEILING);
    let Some(usd) = gas.checked_mul(per_gas).map(worth) else {
        return Err(format!(
            "Nothing was sent: {} answered an impossible fee.",
            chain.name
        ));
    };
    let Some(usd) = usd else {
        return Err(format!(
            "Nothing was sent: what {} is worth is not known, so the network's fee cannot be held to its {ceiling} ceiling.",
            chain.symbol
        ));
    };
    if usd > FEE_CEILING {
        return Err(format!(
            "Nothing was sent: the network's fee could come to {}, over the {ceiling} ceiling on one fee. Try again when {} is less busy.",
            super::policy::dollars(usd),
            chain.name
        ));
    }
    Ok(())
}

/// Gas for a call, with a fifth more than the chain's estimate, since a
/// token contract's state can change between the estimate and the block.
fn gas_for(
    chain: &Chain,
    from: &str,
    to: &[u8; 20],
    value: u128,
    data: &[u8],
) -> Result<u128, String> {
    let call = json!({
        "from": from,
        "to": format!("0x{}", to_hex(to)),
        "value": format!("0x{value:x}"),
        "data": format!("0x{}", to_hex(data)),
    });
    let estimate = quantity(&rpc(chain, "eth_estimateGas", json!([call]))?)?;
    if data.is_empty() && estimate == TRANSFER_GAS {
        return Ok(TRANSFER_GAS);
    }
    // Too large to add to is too large to pay for, which the ceiling says.
    Ok(estimate.saturating_add(estimate / 5))
}

/// Wait for `hash` to be mined; one that reverted is the error, and one still
/// pending when the wait ends is left to finish on its own.
fn receipt(chain: &Chain, hash: &str) -> Result<(), String> {
    let until = std::time::Instant::now() + RECEIPT_TIME;
    while std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let Ok(receipt) = rpc(chain, "eth_getTransactionReceipt", json!([hash])) else {
            continue;
        };
        match receipt["status"].as_str() {
            Some("0x1") => return Ok(()),
            Some("0x0") => {
                return Err(format!(
                    "The transaction was mined on {} but reverted: {hash}",
                    chain.name
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn eth_call(chain: &Chain, to: &str, data: &str) -> Result<Value, String> {
    rpc(
        chain,
        "eth_call",
        json!([{"to": to, "data": data}, "latest"]),
    )
}

/// A JSON-RPC quantity (`0x1a`) as a number.
fn quantity(value: &Value) -> Result<u128, String> {
    let text = value.as_str().ok_or("The chain answered no number.")?;
    let digits = text.trim_start_matches("0x").trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(digits, 16).map_err(|_| format!("`{text}` is too large a number."))
}

/// A 32-byte word a contract returned, as a number; one past u128 reads as
/// the most there can be rather than failing a balance.
fn word(value: &Value) -> Result<u128, String> {
    let bytes = from_hex(value.as_str().ok_or("The contract gave no answer.")?)?;
    if bytes.len() < 32 {
        return Err("That address is not a token contract.".into());
    }
    if bytes[..16].iter().any(|&b| b != 0) {
        return Ok(u128::MAX);
    }
    Ok(bytes[16..32]
        .iter()
        .fold(0u128, |n, &b| (n << 8) | b as u128))
}

/// A string a contract returned, ABI-encoded; empty when it is not one.
fn text(value: &Value) -> String {
    let Ok(bytes) = from_hex(value.as_str().unwrap_or_default()) else {
        return String::new();
    };
    let Some(length) = bytes
        .get(32..64)
        .map(|w| w.iter().fold(0usize, |n, &b| (n << 8) | b as usize))
    else {
        return String::new();
    };
    bytes
        .get(64..64 + length)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .unwrap_or_default()
}

/// A number as the chain's 32-byte word.
fn uint(n: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&n.to_be_bytes());
    out
}

/// An address as the chain's 32-byte word.
fn address_word(address: &[u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(address);
    out
}

/// An address as call data's hex word.
fn word_of_address(address: &[u8; 20]) -> String {
    to_hex(&address_word(address))
}

/// `bytes` without leading zeros, as RLP writes a number.
fn trimmed(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    &bytes[start..]
}

/// A number in RLP: its big-endian bytes with no leading zeros.
fn rlp_uint(n: u128) -> Vec<u8> {
    rlp_bytes(trimmed(&n.to_be_bytes()))
}

/// A byte string in RLP.
fn rlp_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return bytes.to_vec();
    }
    let mut out = rlp_length(bytes.len(), 0x80);
    out.extend_from_slice(bytes);
    out
}

/// A list of items already in RLP.
fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = items.concat();
    let mut out = rlp_length(body.len(), 0xc0);
    out.extend(body);
    out
}

/// RLP's prefix for a string (`0x80`) or a list (`0xc0`) of `len` bytes.
fn rlp_length(len: usize, offset: u8) -> Vec<u8> {
    if len <= 55 {
        return vec![offset + len as u8];
    }
    let digits = trimmed(&(len as u64).to_be_bytes()).to_vec();
    let mut out = vec![offset + 55 + digits.len() as u8];
    out.extend(digits);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_domain_is_the_one_its_contract_reports() {
        // DOMAIN_SEPARATOR() as the contracts answer it (read with eth_call
        // on 2026-10-07).
        let base = evm_bytes("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        assert_eq!(
            to_hex(&domain_separator("USD Coin", "2", 8453, &base)),
            "02fa7265e7c5d81118673727957699e4d68f74cd74b7db77da710fe8a2c7834f"
        );
        // Paxos's USDG on Robinhood Chain, whose version() reverts.
        let usdg = evm_bytes("0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168").unwrap();
        assert_eq!(
            to_hex(&domain_separator("Global Dollar", "1", 4663, &usdg)),
            "7a3d7400b27830f4f91c2c16a082486d67c1befecaec2f53b33f1f35d5b62036"
        );
        let sepolia = evm_bytes("0x036CbD53842c5426634e7929541eC2318f3dCF7e").unwrap();
        assert_eq!(
            to_hex(&domain_separator("USDC", "2", 84532, &sepolia)),
            "71f17a3b2ff373b803d70a5a07c046c1a2bc8e89c09ef722fcb047abe94c9818"
        );
    }

    #[test]
    fn a_signed_authorization_recovers_to_the_payer() {
        let key = [0x11u8; 32];
        let payer = SigningKey::from_slice(&key).unwrap();
        let authorization = Authorization {
            from: evm_bytes(&super::super::keys::address_of(payer.verifying_key())).unwrap(),
            to: [0x22; 20],
            value: 10_000,
            valid_after: 1_740_672_089,
            valid_before: 1_740_672_154,
            nonce: [0x33; 32],
        };
        let contract = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
        let signature = authorize(&key, 84532, contract, "USDC", "2", &authorization).unwrap();
        let bytes = from_hex(&signature).unwrap();
        assert_eq!(bytes.len(), 65);
        assert!(bytes[64] == 27 || bytes[64] == 28);
        let digest = typed_digest(
            &domain_separator("USDC", "2", 84532, &evm_bytes(contract).unwrap()),
            &transfer_hash(&authorization),
        );
        let parsed = k256::ecdsa::Signature::from_slice(&bytes[..64]).unwrap();
        // Low s, or a chain that follows EIP-2 would refuse it.
        assert!(parsed.normalize_s() == parsed);
        let recovery = k256::ecdsa::RecoveryId::from_byte(bytes[64] - 27).unwrap();
        let found =
            k256::ecdsa::VerifyingKey::recover_from_prehash(&digest, &parsed, recovery).unwrap();
        assert_eq!(&found, payer.verifying_key());
    }

    #[test]
    fn a_legacy_transaction_is_signed_as_eip_155_shows() {
        // The worked example in EIP-155 itself: its key is 0x46 repeated.
        let call = Call {
            chain_id: 1,
            nonce: 9,
            gas: 21_000,
            fees: Fees::Legacy {
                price: 20_000_000_000,
            },
            to: [0x35; 20],
            value: 1_000_000_000_000_000_000,
            data: Vec::new(),
        };
        assert_eq!(
            to_hex(&signed(&[0x46; 32], &call).unwrap()),
            "f86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83"
        );
    }

    #[test]
    fn an_eip_1559_transaction_recovers_to_its_sender() {
        let key = [0x46u8; 32];
        let call = Call {
            chain_id: 8453,
            nonce: 3,
            gas: 65_000,
            fees: Fees::Eip1559 {
                tip: 1_000_000,
                max: 20_000_000,
            },
            to: [0x83; 20],
            value: 0,
            data: vec![0xa9, 0x05, 0x9c, 0xbb],
        };
        // EIP-1559's fields in its order, the access list empty.
        let items = vec![
            rlp_uint(8453),
            rlp_uint(3),
            rlp_uint(1_000_000),
            rlp_uint(20_000_000),
            rlp_uint(65_000),
            rlp_bytes(&[0x83; 20]),
            rlp_uint(0),
            rlp_bytes(&[0xa9, 0x05, 0x9c, 0xbb]),
            rlp_list(&[]),
        ];
        let mut unsigned = vec![0x02];
        unsigned.extend(rlp_list(&items));
        let digest = keccak(&unsigned);
        let (signature, recovery) = sign(&key, &digest).unwrap();
        let parsed = k256::ecdsa::Signature::from_slice(&signature).unwrap();
        let id = k256::ecdsa::RecoveryId::from_byte(recovery).unwrap();
        let found = k256::ecdsa::VerifyingKey::recover_from_prehash(&digest, &parsed, id).unwrap();
        assert_eq!(
            &found,
            SigningKey::from_slice(&key).unwrap().verifying_key()
        );
        let mut whole = items;
        whole.extend([
            rlp_uint(recovery as u128),
            rlp_bytes(trimmed(&signature[..32])),
            rlp_bytes(trimmed(&signature[32..])),
        ]);
        let mut expected = vec![0x02];
        expected.extend(rlp_list(&whole));
        assert_eq!(signed(&key, &call).unwrap(), expected);
    }

    #[test]
    fn a_fee_over_the_ceiling_or_of_unknown_worth_is_refused() {
        let chains = super::super::chains::all(&super::super::Wallet::default());
        let ethereum = super::super::chains::find(&chains, "ethereum").unwrap();
        // ETH at $4,000, in micro-dollars for so many wei.
        let at = |wei: u128| u64::try_from((wei * 4_000).div_ceil(10u128.pow(12))).ok();
        // A token transfer at 2 gwei, about fifty cents.
        assert!(within_ceiling(ethereum, 65_000, 2_000_000_000, at).is_ok());
        // At 100 gwei, $26.
        let why = within_ceiling(ethereum, 65_000, 100_000_000_000, at).unwrap_err();
        assert!(why.contains("$26.00, over the $5.00 ceiling"), "{why}");
        let why = within_ceiling(ethereum, 65_000, 2_000_000_000, |_| None).unwrap_err();
        assert!(why.contains("not known"), "{why}");
        let why = within_ceiling(ethereum, u128::MAX, 2, at).unwrap_err();
        assert!(why.contains("impossible"), "{why}");
    }

    #[test]
    fn rlp_writes_what_the_yellow_paper_says() {
        assert_eq!(rlp_bytes(b"dog"), [0x83, b'd', b'o', b'g']);
        assert_eq!(
            rlp_list(&[rlp_bytes(b"cat"), rlp_bytes(b"dog")]),
            [0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g']
        );
        assert_eq!(rlp_bytes(b""), [0x80]);
        assert_eq!(rlp_list(&[]), [0xc0]);
        assert_eq!(rlp_uint(0), [0x80]);
        assert_eq!(rlp_uint(15), [0x0f]);
        assert_eq!(rlp_uint(1024), [0x82, 0x04, 0x00]);
        let long = vec![b'a'; 56];
        assert_eq!(&rlp_bytes(&long)[..2], &[0xb8, 56]);
    }

    #[test]
    fn numbers_from_the_chain_are_read_in_hex() {
        assert_eq!(quantity(&json!("0x0")).unwrap(), 0);
        assert_eq!(quantity(&json!("0x1a")).unwrap(), 26);
        assert_eq!(quantity(&json!("0x00ff")).unwrap(), 255);
        assert!(quantity(&json!("0x1ffffffffffffffffffffffffffffffff")).is_err());
        let answer = format!("0x{}", to_hex(&uint(1_234_567)));
        assert_eq!(word(&json!(answer)).unwrap(), 1_234_567);
        assert!(word(&json!("0x")).is_err());
        let symbol = "0x000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000045553444300000000000000000000000000000000000000000000000000000000";
        assert_eq!(text(&json!(symbol)), "USDC");
    }

    #[test]
    #[ignore = "reads Base Sepolia; run with --ignored when online"]
    fn base_sepolia_reports_a_balance_and_the_usdc_contract() {
        let chains = super::super::chains::all(&super::super::Wallet::default());
        let chain = super::super::chains::find(&chains, "base-sepolia").unwrap();
        balance(chain, "0x9858EfFD232B4033E47d90003D41EC34EcaEda94").unwrap();
        let usdc = chain.token("usdc").unwrap();
        token_balance(chain, usdc, "0x9858EfFD232B4033E47d90003D41EC34EcaEda94").unwrap();
        assert_eq!(describe(chain, &usdc.address).unwrap(), (6, "USDC".into()));
        let separator = eth_call(chain, &usdc.address, "0x3644e515").unwrap();
        assert_eq!(
            separator.as_str().unwrap(),
            "0x71f17a3b2ff373b803d70a5a07c046c1a2bc8e89c09ef722fcb047abe94c9818"
        );
    }
}
