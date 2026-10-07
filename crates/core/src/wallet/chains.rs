//! The networks a wallet holds money on: those Letronna knows, and any the
//! user adds.
//!
//! Each is named by its CAIP-2 id (`eip155:8453`, `solana:5eykt…`), as x402
//! names them, so a site's offer finds its network without a table of
//! spellings; the older x402 names (`base`, `solana-devnet`) are kept as
//! aliases. Every EVM chain is one more entry -- a chain id and an RPC
//! address -- as the same key signs on all of them, which is what lets the
//! user add any. Public RPC addresses are the defaults; a busy one can be
//! swapped for the user's own.

use super::Wallet;
use serde::{Deserialize, Serialize};

/// The kind of chain, which decides the key that signs and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Evm,
    Solana,
}

/// A network money can be held and paid on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    /// The CAIP-2 id, such as `eip155:8453`.
    pub id: String,
    pub name: String,
    pub family: Family,
    /// The JSON-RPC address balances are read and payments sent through.
    pub rpc: String,
    /// The native coin, which pays the network's fees.
    pub symbol: String,
    pub decimals: u8,
    /// A transaction's page, with `{tx}` where its hash goes.
    #[serde(default)]
    pub explorer: String,
    #[serde(default)]
    pub testnet: bool,
    /// Other names a site or the user may give it, x402's older ones among them.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Whether the wallet uses it: balances, payments and x402 offers.
    #[serde(skip)]
    pub on: bool,
    /// The tokens it knows on this network, besides the native coin.
    #[serde(skip)]
    pub tokens: Vec<Token>,
}

/// A token on one network.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Token {
    /// The CAIP-2 id of its network.
    pub chain: String,
    pub symbol: String,
    /// The contract (EVM) or mint (Solana).
    pub address: String,
    pub decimals: u8,
    /// Worth one US dollar, so it counts against the limits as it is.
    #[serde(default)]
    pub dollar: bool,
    /// The EIP-712 name and version its contract signs transfers under,
    /// for an x402 site that does not say them.
    #[serde(default)]
    pub signing_name: String,
    #[serde(default)]
    pub signing_version: String,
}

/// What the user changed about one network.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Network {
    /// Switched on or off by hand; none to follow its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// The user's own RPC address in place of the public one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rpc: String,
}

/// Solana mainnet's CAIP-2 id.
pub const SOLANA: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
/// Solana devnet's CAIP-2 id.
pub const SOLANA_DEVNET: &str = "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1";
/// Base's CAIP-2 id, the network most x402 sites take.
pub const BASE: &str = "eip155:8453";

/// A built-in network as the table below writes it: id, name, RPC, coin,
/// decimals, explorer, testnet, and its other names.
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    u8,
    &'static str,
    bool,
    &'static [&'static str],
);

/// Each built-in network.
const BUILT_IN: &[Row] = &[
    (
        BASE,
        "Base",
        "https://mainnet.base.org",
        "ETH",
        18,
        "https://basescan.org/tx/{tx}",
        false,
        &["base"],
    ),
    (
        "eip155:1",
        "Ethereum",
        "https://ethereum-rpc.publicnode.com",
        "ETH",
        18,
        "https://etherscan.io/tx/{tx}",
        false,
        &["ethereum", "mainnet"],
    ),
    (
        "eip155:42161",
        "Arbitrum",
        "https://arb1.arbitrum.io/rpc",
        "ETH",
        18,
        "https://arbiscan.io/tx/{tx}",
        false,
        &["arbitrum", "arbitrum-one"],
    ),
    (
        "eip155:10",
        "Optimism",
        "https://mainnet.optimism.io",
        "ETH",
        18,
        "https://optimistic.etherscan.io/tx/{tx}",
        false,
        &["optimism"],
    ),
    (
        "eip155:137",
        "Polygon",
        "https://polygon-bor-rpc.publicnode.com",
        "POL",
        18,
        "https://polygonscan.com/tx/{tx}",
        false,
        &["polygon"],
    ),
    (
        "eip155:56",
        "BNB Chain",
        "https://bsc-dataseed.bnbchain.org",
        "BNB",
        18,
        "https://bscscan.com/tx/{tx}",
        false,
        &["bsc", "bnb"],
    ),
    (
        "eip155:43114",
        "Avalanche",
        "https://api.avax.network/ext/bc/C/rpc",
        "AVAX",
        18,
        "https://snowtrace.io/tx/{tx}",
        false,
        &["avalanche"],
    ),
    (
        SOLANA,
        "Solana",
        "https://api.mainnet-beta.solana.com",
        "SOL",
        9,
        "https://solscan.io/tx/{tx}",
        false,
        &["solana", "solana-mainnet"],
    ),
    (
        "eip155:84532",
        "Base Sepolia",
        "https://sepolia.base.org",
        "ETH",
        18,
        "https://sepolia.basescan.org/tx/{tx}",
        true,
        &["base-sepolia"],
    ),
    (
        "eip155:11155111",
        "Sepolia",
        "https://ethereum-sepolia-rpc.publicnode.com",
        "ETH",
        18,
        "https://sepolia.etherscan.io/tx/{tx}",
        true,
        &["sepolia"],
    ),
    (
        "eip155:80002",
        "Polygon Amoy",
        "https://polygon-amoy-bor-rpc.publicnode.com",
        "POL",
        18,
        "https://amoy.polygonscan.com/tx/{tx}",
        true,
        &["polygon-amoy"],
    ),
    (
        "eip155:43113",
        "Avalanche Fuji",
        "https://api.avax-test.network/ext/bc/C/rpc",
        "AVAX",
        18,
        "https://testnet.snowtrace.io/tx/{tx}",
        true,
        &["avalanche-fuji"],
    ),
    (
        SOLANA_DEVNET,
        "Solana Devnet",
        "https://api.devnet.solana.com",
        "SOL",
        9,
        "https://solscan.io/tx/{tx}?cluster=devnet",
        true,
        &["solana-devnet"],
    ),
];

/// Each built-in token: network, symbol, address, decimals, and the EIP-712
/// name and version its transfers are signed under (none where it takes no
/// signed transfers).
const TOKENS: &[(&str, &str, &str, u8, &str, &str)] = &[
    (
        BASE,
        "USDC",
        "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        6,
        "USD Coin",
        "2",
    ),
    (
        "eip155:1",
        "USDC",
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        6,
        "USD Coin",
        "2",
    ),
    (
        "eip155:1",
        "USDT",
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        6,
        "",
        "",
    ),
    (
        "eip155:42161",
        "USDC",
        "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
        6,
        "USD Coin",
        "2",
    ),
    (
        "eip155:10",
        "USDC",
        "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85",
        6,
        "USD Coin",
        "2",
    ),
    (
        "eip155:137",
        "USDC",
        "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359",
        6,
        "USD Coin",
        "2",
    ),
    (
        "eip155:56",
        "USDC",
        "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
        18,
        "",
        "",
    ),
    (
        "eip155:43114",
        "USDC",
        "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E",
        6,
        "USD Coin",
        "2",
    ),
    (
        SOLANA,
        "USDC",
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        6,
        "",
        "",
    ),
    (
        SOLANA,
        "USDT",
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
        6,
        "",
        "",
    ),
    (
        "eip155:84532",
        "USDC",
        "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
        6,
        "USDC",
        "2",
    ),
    (
        "eip155:11155111",
        "USDC",
        "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
        6,
        "USDC",
        "2",
    ),
    (
        "eip155:80002",
        "USDC",
        "0x41E94Eb019C0762f9Bfcf9Fb1E58725BfB0e7582",
        6,
        "USDC",
        "2",
    ),
    (
        "eip155:43113",
        "USDC",
        "0x5425890298aed601595a70AB815c96711a31Bc65",
        6,
        "USD Coin",
        "2",
    ),
    (
        SOLANA_DEVNET,
        "USDC",
        "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
        6,
        "",
        "",
    ),
];

/// Every network: the built-in ones and the user's, each with its tokens,
/// whether it is on, and the RPC address the user set for it.
pub fn all(wallet: &Wallet) -> Vec<Chain> {
    let built_in = BUILT_IN.iter().map(
        |&(id, name, rpc, symbol, decimals, explorer, testnet, aliases)| Chain {
            id: id.into(),
            name: name.into(),
            family: if id.starts_with("solana:") {
                Family::Solana
            } else {
                Family::Evm
            },
            rpc: rpc.into(),
            symbol: symbol.into(),
            decimals,
            explorer: explorer.into(),
            testnet,
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            on: false,
            tokens: Vec::new(),
        },
    );
    let mut chains: Vec<Chain> = built_in.collect();
    for added in &wallet.chains {
        if !chains.iter().any(|chain| chain.id == added.id) {
            chains.push(added.clone());
        }
    }
    let tokens = TOKENS
        .iter()
        .map(|&(chain, symbol, address, decimals, name, version)| Token {
            chain: chain.into(),
            symbol: symbol.into(),
            address: address.into(),
            decimals,
            dollar: true,
            signing_name: name.into(),
            signing_version: version.into(),
        })
        .chain(wallet.tokens.iter().cloned());
    let tokens: Vec<Token> = tokens.collect();
    for chain in &mut chains {
        let set = wallet.networks.get(&chain.id).cloned().unwrap_or_default();
        chain.on = set.on.unwrap_or(!chain.testnet || wallet.policy.testnets);
        if !set.rpc.is_empty() {
            chain.rpc = set.rpc;
        }
        for token in tokens.iter().filter(|token| token.chain == chain.id) {
            if chain.token(&token.address).is_none() {
                chain.tokens.push(token.clone());
            }
        }
    }
    chains
}

/// The network `name` means: its CAIP-2 id, its name, one of its other names,
/// or an EVM chain id alone, in any case.
pub fn find<'a>(chains: &'a [Chain], name: &str) -> Option<&'a Chain> {
    let wanted = name.trim().to_lowercase();
    let dashed = wanted.replace(' ', "-");
    chains.iter().find(|chain| {
        chain.id.to_lowercase() == wanted
            || chain.name.to_lowercase() == wanted
            || chain.name.to_lowercase().replace(' ', "-") == dashed
            || chain.aliases.contains(&dashed)
            || chain.id.strip_prefix("eip155:") == Some(wanted.as_str())
    })
}

impl Chain {
    /// The token named by its symbol or address, on this network.
    pub fn token(&self, name: &str) -> Option<&Token> {
        let name = name.trim();
        self.tokens.iter().find(|token| {
            token.symbol.eq_ignore_ascii_case(name)
                || match self.family {
                    Family::Evm => token.address.eq_ignore_ascii_case(name),
                    Family::Solana => token.address == name,
                }
        })
    }

    /// Whether `name` means the native coin rather than a token.
    pub fn is_native(&self, name: &str) -> bool {
        let name = name.trim();
        name.is_empty() || name.eq_ignore_ascii_case(&self.symbol) || name == "native"
    }

    /// The EVM chain id; none on Solana or for a malformed id.
    pub fn evm_id(&self) -> Option<u64> {
        self.id.strip_prefix("eip155:")?.parse().ok()
    }

    /// The page for transaction `tx`, when the network has an explorer.
    pub fn link(&self, tx: &str) -> String {
        if self.explorer.is_empty() || tx.is_empty() {
            return String::new();
        }
        self.explorer.replace("{tx}", tx)
    }
}

/// A network the user adds by hand: an EVM chain by its id and RPC address.
pub fn custom_evm(name: &str, chain_id: u64, rpc: &str, symbol: &str) -> Result<Chain, String> {
    let rpc = rpc.trim();
    if !(rpc.starts_with("https://") || rpc.starts_with("http://")) {
        return Err(format!("`{rpc}` is not an RPC address (https://…)."));
    }
    if name.trim().is_empty() {
        return Err("Give the network a name.".into());
    }
    let symbol = if symbol.trim().is_empty() {
        "ETH"
    } else {
        symbol.trim()
    };
    Ok(Chain {
        id: format!("eip155:{chain_id}"),
        name: name.trim().into(),
        family: Family::Evm,
        rpc: rpc.into(),
        symbol: symbol.to_uppercase(),
        decimals: 18,
        explorer: String::new(),
        testnet: false,
        aliases: Vec::new(),
        on: true,
        tokens: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_is_found_by_any_of_its_names() {
        let chains = all(&Wallet::default());
        for name in ["base", "Base", "eip155:8453", "8453", " BASE "] {
            assert_eq!(
                find(&chains, name).map(|c| c.id.as_str()),
                Some(BASE),
                "{name}"
            );
        }
        assert_eq!(find(&chains, "solana-devnet").unwrap().id, SOLANA_DEVNET);
        assert_eq!(find(&chains, "Base Sepolia").unwrap().id, "eip155:84532");
        assert_eq!(find(&chains, "base-sepolia").unwrap().id, "eip155:84532");
        assert!(find(&chains, "dogechain").is_none());
    }

    #[test]
    fn test_networks_stay_off_until_asked_for_and_a_choice_by_hand_wins() {
        let mut wallet = Wallet::default();
        let on = |wallet: &Wallet, id: &str| all(wallet).iter().find(|c| c.id == id).unwrap().on;
        assert!(on(&wallet, BASE) && !on(&wallet, "eip155:84532"));
        wallet.policy.testnets = true;
        assert!(on(&wallet, "eip155:84532"));
        wallet.networks.insert(
            BASE.into(),
            Network {
                on: Some(false),
                rpc: "https://mine.example/rpc".into(),
            },
        );
        let chains = all(&wallet);
        let base = find(&chains, "base").unwrap();
        assert!(!base.on);
        assert_eq!(base.rpc, "https://mine.example/rpc");
    }

    #[test]
    fn a_users_network_and_token_join_the_built_in_ones() {
        let mut wallet = Wallet::default();
        wallet
            .chains
            .push(custom_evm("Sei", 1329, "https://evm-rpc.sei-apis.com", "sei").unwrap());
        wallet.tokens.push(Token {
            chain: BASE.into(),
            symbol: "EURC".into(),
            address: "0x60a3E35Cc302bFA44Cb288Bc5a4F316Fdb1adb42".into(),
            decimals: 6,
            dollar: false,
            signing_name: String::new(),
            signing_version: String::new(),
        });
        let chains = all(&wallet);
        let sei = find(&chains, "1329").unwrap();
        assert!(sei.on && sei.symbol == "SEI" && sei.family == Family::Evm);
        let base = find(&chains, "base").unwrap();
        assert!(base.token("eurc").is_some() && base.token("usdc").unwrap().dollar);
        assert!(
            base.token("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
                .is_some()
        );
        assert!(base.is_native("eth") && base.is_native("") && !base.is_native("usdc"));
        assert!(custom_evm("x", 1, "ftp://nope", "").is_err());
    }
}
