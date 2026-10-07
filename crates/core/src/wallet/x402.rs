//! x402: paying a site that answers HTTP 402 with what it wants, then
//! asking again with the payment.
//!
//! A site lists what it takes -- each an `exact` amount of a token on a
//! network, to an address -- in a `PAYMENT-REQUIRED` header (version 2) or
//! in the 402's JSON body (version 1). The first offer this wallet can pay
//! is chosen, the user's preferred networks first; the payment is signed
//! here and sent back in `PAYMENT-SIGNATURE` (or `X-PAYMENT`), and the site's
//! facilitator settles it on the chain. On an EVM chain the payment is an
//! EIP-3009 authorization, so the payer spends no gas; on Solana it is a
//! token transfer the site's fee payer co-signs.
//!
//! Only an offer whose worth in dollars is known can be paid, and each goes
//! through the ledger's caps first. Payments are refused while the wallet is
//! paused or x402 is off. What a page costs is remembered for a while by its
//! address, so `paid_fetch`'s card can show the price before the user says
//! yes.

use super::chains::{self, Chain, Family, Token};
use super::policy::{self, Entry, Kind, Status, X402};
use super::{Account, Wallet};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a page's price is remembered for its card.
const QUOTE_TIME: Duration = Duration::from_secs(10 * 60);
/// How far back an EVM authorization is dated, for a site whose clock runs
/// behind ours; the reference clients use ten minutes.
const CLOCK_SLACK: u64 = 600;
/// The shortest an authorization is valid for, whatever the site asked.
const MIN_VALID: u64 = 60;

/// What a site asked to be paid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Quote {
    /// 1 or 2: which headers the payment goes back in.
    pub version: u64,
    /// The resource the site described (version 2), echoed back.
    pub resource: Value,
    /// Each offer, as the site wrote it.
    pub accepts: Vec<Value>,
    /// What the site said, when it said why.
    pub error: String,
}

/// An offer this wallet can pay, and who pays it.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub quote: Quote,
    /// The offer, as the site wrote it.
    pub offer: Value,
    pub chain: Chain,
    pub token: Token,
    pub amount: u128,
    pub pay_to: String,
    pub account: Account,
    /// Its worth in micro-dollars, when known.
    pub usd: Option<u64>,
    /// The site's host.
    pub site: String,
}

impl Plan {
    /// The price as a person reads it: `0.01 USDC ($0.01) on Base`.
    pub fn price(&self) -> String {
        let amount = super::amount_text(self.amount, self.token.decimals);
        let usd = self
            .usd
            .map(|usd| format!(" ({})", policy::dollars(usd)))
            .unwrap_or_default();
        format!("{amount} {}{usd} on {}", self.token.symbol, self.chain.name)
    }
}

/// What the site at `headers` and `body` asked for, when it is an x402 402.
pub(crate) fn read(headers: &HeaderMap, body: &[u8]) -> Option<Quote> {
    let from_header = headers
        .get("payment-required")
        .and_then(|value| STANDARD.decode(value.as_bytes().trim_ascii()).ok())
        .and_then(|json| serde_json::from_slice::<Value>(&json).ok());
    let required = from_header.or_else(|| serde_json::from_slice::<Value>(body).ok())?;
    let version = required["x402Version"].as_u64()?;
    let accepts = required["accepts"].as_array()?.clone();
    if accepts.is_empty() || !matches!(version, 1 | 2) {
        return None;
    }
    Some(Quote {
        version,
        resource: required["resource"].clone(),
        accepts,
        error: required["error"].as_str().unwrap_or_default().to_owned(),
    })
}

/// The first offer in `quote` the wallet can pay to `site`: an `exact` one,
/// on a network that is on, in a token of known decimals, from a wallet with
/// a key for that network, and, when `funded` is asked, enough to pay it.
/// The reason none can be paid otherwise.
pub(crate) fn choose(
    quote: &Quote,
    wallet: &Wallet,
    site: &str,
    funded: bool,
) -> Result<Plan, String> {
    let networks = chains::all(wallet);
    let mut offers: Vec<&Value> = quote.accepts.iter().collect();
    let rank = |offer: &&Value| {
        let id = chains::find(&networks, offer["network"].as_str().unwrap_or_default())
            .map(|chain| chain.id.clone())
            .unwrap_or_default();
        wallet
            .policy
            .prefer
            .iter()
            .position(|p| *p == id)
            .unwrap_or(usize::MAX)
    };
    offers.sort_by_key(rank);
    let mut why: Vec<String> = Vec::new();
    for offer in offers {
        let network = offer["network"].as_str().unwrap_or_default();
        if offer["scheme"].as_str() != Some("exact") {
            why.push(format!(
                "its `{}` payments",
                offer["scheme"].as_str().unwrap_or("?")
            ));
            continue;
        }
        let Some(chain) = chains::find(&networks, network) else {
            why.push(format!("{network}, a network the wallet does not know"));
            continue;
        };
        if !chain.on {
            why.push(format!("{}, switched off in the wallet", chain.name));
            continue;
        }
        let asset = offer["asset"].as_str().unwrap_or_default();
        let Some(token) = chain.token(asset).cloned() else {
            why.push(format!(
                "a token on {} the wallet does not know ({asset})",
                chain.name
            ));
            continue;
        };
        let amount = offer["amount"]
            .as_str()
            .or(offer["maxAmountRequired"].as_str())
            .and_then(|amount| amount.parse::<u128>().ok());
        let Some(amount) = amount else {
            why.push(format!("an offer on {} with no amount", chain.name));
            continue;
        };
        let pay_to = offer["payTo"].as_str().unwrap_or_default().to_owned();
        let payers = wallet.accounts.iter().filter(|account| {
            account.holds != super::Holds::Watch && !account.address(chain.family).is_empty()
        });
        let mut payers: Vec<&Account> = payers.collect();
        payers.sort_by_key(|account| account.id != wallet.default);
        if payers.is_empty() {
            why.push(format!(
                "{} on {}, which no wallet here can pay on",
                token.symbol, chain.name
            ));
            continue;
        }
        for account in payers {
            if funded {
                let held = match chain.family {
                    Family::Evm => super::evm::token_balance(chain, &token, &account.evm),
                    Family::Solana => super::solana::token_balance(chain, &account.solana, &token),
                };
                if !held.is_ok_and(|held| held >= amount) {
                    why.push(format!(
                        "{} {} on {}, more than {} holds",
                        super::amount_text(amount, token.decimals),
                        token.symbol,
                        chain.name,
                        account.label
                    ));
                    continue;
                }
            }
            return Ok(Plan {
                quote: quote.clone(),
                offer: offer.clone(),
                usd: policy::worth(amount, token.decimals, token.dollar, &token.symbol),
                chain: chain.clone(),
                token,
                amount,
                pay_to,
                account: account.clone(),
                site: site.to_owned(),
            });
        }
    }
    Err(format!(
        "The site asks for payment (x402), but the wallet cannot pay it: it takes {}.",
        why.join("; ")
    ))
}

/// The header that pays for `plan`: its name and value.
pub(crate) fn pay(plan: &Plan) -> Result<(&'static str, String), String> {
    let payload = match plan.chain.family {
        Family::Evm => {
            let key = super::evm_key(&plan.account)?;
            let chain_id = plan
                .chain
                .evm_id()
                .ok_or_else(|| format!("{} has no chain id.", plan.chain.name))?;
            let extra = &plan.offer["extra"];
            let name = extra["name"]
                .as_str()
                .unwrap_or(plan.token.signing_name.as_str());
            let version = extra["version"]
                .as_str()
                .unwrap_or(plan.token.signing_version.as_str());
            if name.is_empty() || version.is_empty() {
                return Err(format!(
                    "{} on {} cannot be paid by signature.",
                    plan.token.symbol, plan.chain.name
                ));
            }
            let now = crate::chat::now();
            let timeout = plan.offer["maxTimeoutSeconds"]
                .as_u64()
                .unwrap_or(MIN_VALID);
            let authorization = super::evm::Authorization {
                from: super::keys::evm_bytes(&plan.account.evm)?,
                to: super::keys::evm_bytes(&plan.pay_to)?,
                value: plan.amount,
                valid_after: now.saturating_sub(CLOCK_SLACK),
                valid_before: now + timeout.max(MIN_VALID),
                nonce: super::random::<32>()?,
            };
            let signature = super::evm::authorize(
                &key,
                chain_id,
                &plan.token.address,
                name,
                version,
                &authorization,
            )?;
            json!({
                "signature": signature,
                "authorization": {
                    "from": plan.account.evm,
                    "to": plan.pay_to,
                    "value": plan.amount.to_string(),
                    "validAfter": authorization.valid_after.to_string(),
                    "validBefore": authorization.valid_before.to_string(),
                    "nonce": format!("0x{}", super::keys::to_hex(&authorization.nonce)),
                }
            })
        }
        Family::Solana => {
            let seed = super::solana_key(&plan.account)?;
            let amount =
                u64::try_from(plan.amount).map_err(|_| "Too large an amount for Solana.")?;
            let transaction = super::solana::x402_payment(
                &plan.chain,
                &seed,
                &plan.offer["extra"],
                &plan.pay_to,
                &plan.token.address,
                amount,
            )?;
            json!({"transaction": STANDARD.encode(transaction)})
        }
    };
    let (name, body) = match plan.quote.version {
        1 => (
            "X-PAYMENT",
            json!({
                "x402Version": 1,
                "scheme": "exact",
                "network": plan.offer["network"],
                "payload": payload,
            }),
        ),
        _ => {
            let mut body = json!({
                "x402Version": 2,
                "accepted": plan.offer,
                "payload": payload,
            });
            if !plan.quote.resource.is_null() {
                body["resource"] = plan.quote.resource.clone();
            }
            ("PAYMENT-SIGNATURE", body)
        }
    };
    Ok((name, STANDARD.encode(body.to_string())))
}

/// What the site said about settling a payment, from `PAYMENT-RESPONSE` or
/// `X-PAYMENT-RESPONSE`.
fn settlement(headers: &HeaderMap) -> Option<Value> {
    let value = headers
        .get("payment-response")
        .or_else(|| headers.get("x-payment-response"))?;
    let json = STANDARD.decode(value.as_bytes().trim_ascii()).ok()?;
    serde_json::from_slice(&json).ok()
}

/// Pay for `plan` and ask for the page again with the payment: `method` to
/// `url` with `body`. The ledger holds the payment first, so the caps are
/// checked with the lock held; what the site answers after paying is the
/// page, or the error when it refused.
pub(crate) fn pay_and_fetch(
    plan: &Plan,
    method: reqwest::Method,
    url: &reqwest::Url,
    body: Option<&str>,
    kind: Kind,
) -> Result<(reqwest::blocking::Response, Entry), String> {
    let wallet = super::load();
    let mut entry = Entry {
        id: super::new_id(),
        at: crate::chat::now() as i64,
        kind,
        status: Status::Pending,
        chain: plan.chain.name.clone(),
        token: plan.token.symbol.clone(),
        amount: super::amount_text(plan.amount, plan.token.decimals),
        usd: plan.usd,
        to: plan.pay_to.clone(),
        site: plan.site.clone(),
        wallet: plan.account.label.clone(),
        tx: String::new(),
        link: String::new(),
        note: String::new(),
    };
    policy::reserve(&wallet.policy, entry.clone())?;
    let header = match pay(plan) {
        Ok(header) => header,
        Err(why) => {
            policy::settle(&entry.id, Status::Failed, "", "", &why);
            return Err(why);
        }
    };
    let mut request = super::client()?
        .request(method, url.clone())
        .header(header.0, header.1);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_owned());
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(e) => {
            let why = format!("The payment was signed but the site did not answer: {e}");
            policy::settle(&entry.id, Status::Unsettled, "", "", &why);
            return Err(why);
        }
    };
    let settled = settlement(response.headers());
    let tx = settled
        .as_ref()
        .and_then(|s| s["transaction"].as_str())
        .unwrap_or_default()
        .to_owned();
    let link = plan.chain.link(&tx);
    if response.status().as_u16() == 402 {
        let headers = response.headers().clone();
        let said = read(&headers, &response.bytes().unwrap_or_default())
            .map(|quote| quote.error)
            .filter(|error| !error.is_empty())
            .or_else(|| {
                settled
                    .as_ref()
                    .and_then(|s| s["errorReason"].as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| "no reason given".into());
        let why = format!("The site did not take the payment ({said}).");
        // The signature left this machine, so the site could still use it
        // until it expires: it keeps counting.
        policy::settle(&entry.id, Status::Unsettled, &tx, &link, &why);
        return Err(why);
    }
    let status = if response.status().is_success() {
        Status::Paid
    } else {
        Status::Unsettled
    };
    policy::settle(&entry.id, status, &tx, &link, "");
    entry.status = status;
    entry.tx = tx;
    entry.link = link;
    Ok((response, entry))
}

/// What `web_fetch` does with a 402 from `url`: `None` when it is not an x402
/// one, else the page, paid for, when the wallet pays this one on its own,
/// or why not, with the price, for the model to ask the user through
/// `paid_fetch`.
pub(crate) fn on_402(
    headers: &HeaderMap,
    body: &[u8],
    url: &reqwest::Url,
) -> Option<Result<reqwest::blocking::Response, String>> {
    let quote = read(headers, body)?;
    let wallet = super::load();
    let site = url.host_str().unwrap_or_default().to_owned();
    let lead = "The page answered HTTP 402: it asks to be paid (x402).";
    if wallet.accounts.is_empty() {
        return Some(Err(format!(
            "{lead} There is no wallet in Letronna yet; the user can add one with `letronna wallet new` or on the Wallet page."
        )));
    }
    let plan = match choose(&quote, &wallet, &site, true) {
        Ok(plan) => plan,
        Err(why) => return Some(Err(format!("{lead} {why}"))),
    };
    remember(url.as_str(), &plan);
    let price = plan.price();
    if wallet.policy.paused || wallet.policy.x402 == X402::Off {
        return Some(Err(format!(
            "{lead} It costs {price}, but the user has the wallet's payments switched off."
        )));
    }
    if let Err(why) = policy::site_allowed(&wallet.policy, &site) {
        return Some(Err(format!("{lead} It costs {price}, but {why}")));
    }
    if !policy::automatic(&wallet.policy, plan.usd, &site) {
        return Some(Err(format!(
            "{lead} It costs {price}. To pay, call paid_fetch with this url; the user sees the price and decides."
        )));
    }
    Some(pay_and_fetch(&plan, reqwest::Method::GET, url, None, Kind::X402).map(|(page, _)| page))
}

/// A page's price, kept for its card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Remembered {
    pub price: String,
    pub usd: Option<u64>,
    pub site: String,
}

static QUOTES: Mutex<Vec<(String, Instant, Remembered)>> = Mutex::new(Vec::new());

/// Keep what `url` costs, for its card.
pub(crate) fn remember(url: &str, plan: &Plan) {
    let Ok(mut quotes) = QUOTES.lock() else {
        return;
    };
    quotes.retain(|(known, at, _)| known != url && at.elapsed() < QUOTE_TIME);
    quotes.push((
        url.to_owned(),
        Instant::now(),
        Remembered {
            price: plan.price(),
            usd: plan.usd,
            site: plan.site.clone(),
        },
    ));
}

/// What `url` was last seen to cost, when that was lately.
pub(crate) fn remembered(url: &str) -> Option<Remembered> {
    let quotes = QUOTES.lock().ok()?;
    quotes
        .iter()
        .find(|(known, at, _)| known == url && at.elapsed() < QUOTE_TIME)
        .map(|(_, _, remembered)| remembered.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{DATA_DIR, Scratch};
    use reqwest::header::HeaderValue;
    use std::io::{BufRead, BufReader, Read, Write};

    /// A v2 offer of a cent of Base Sepolia USDC.
    fn offer() -> Value {
        json!({
            "scheme": "exact",
            "network": "eip155:84532",
            "amount": "10000",
            "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
            "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
            "maxTimeoutSeconds": 60,
            "extra": {"name": "USDC", "version": "2"}
        })
    }

    /// A wallet with the test phrase, test networks on.
    fn wallet() -> Wallet {
        let mut wallet = Wallet::default();
        wallet.policy.testnets = true;
        wallet.accounts.push(Account {
            id: "w1".into(),
            label: "Main".into(),
            holds: super::super::Holds::Phrase,
            evm: "0x9858EfFD232B4033E47d90003D41EC34EcaEda94".into(),
            solana: "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk".into(),
            index: 0,
        });
        wallet.default = "w1".into();
        wallet
    }

    #[test]
    fn a_version_two_header_and_a_version_one_body_are_both_read() {
        let mut headers = HeaderMap::new();
        let required =
            json!({"x402Version": 2, "resource": {"url": "https://x.dev/a"}, "accepts": [offer()]});
        let encoded = STANDARD.encode(required.to_string());
        headers.insert("payment-required", HeaderValue::from_str(&encoded).unwrap());
        let quote = read(&headers, b"").unwrap();
        assert_eq!((quote.version, quote.accepts.len()), (2, 1));
        assert_eq!(quote.resource["url"], "https://x.dev/a");

        let v1 = json!({"x402Version": 1, "error": "X-PAYMENT header is required", "accepts": [{
            "scheme": "exact", "network": "base-sepolia", "maxAmountRequired": "10000",
            "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e", "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
            "resource": "https://x.dev/a", "description": "", "maxTimeoutSeconds": 60, "extra": {"name": "USDC", "version": "2"}
        }]});
        let quote = read(&HeaderMap::new(), v1.to_string().as_bytes()).unwrap();
        assert_eq!(quote.version, 1);
        assert_eq!(quote.error, "X-PAYMENT header is required");
        assert!(read(&HeaderMap::new(), b"{\"error\": \"pay up\"}").is_none());
        assert!(read(&HeaderMap::new(), b"<html>").is_none());
    }

    #[test]
    fn the_offer_on_a_preferred_network_the_wallet_can_pay_is_chosen() {
        let solana = json!({
            "scheme": "exact", "network": chains::SOLANA_DEVNET, "amount": "5000",
            "asset": "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU", "payTo": "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4",
            "maxTimeoutSeconds": 60, "extra": {"feePayer": "EwWqGE4ZFKLofuestmU4LDdK7XM1N4ALgdZccwYugwGd"}
        });
        let upto = json!({"scheme": "upto", "network": "eip155:84532", "amount": "1"});
        let quote = Quote {
            version: 2,
            resource: Value::Null,
            accepts: vec![upto, offer(), solana],
            error: String::new(),
        };
        let mut wallet = wallet();
        let plan = choose(&quote, &wallet, "x.dev", false).unwrap();
        assert_eq!(plan.chain.id, "eip155:84532");
        assert_eq!(plan.price(), "0.01 USDC ($0.01) on Base Sepolia");
        wallet.policy.prefer = vec![chains::SOLANA_DEVNET.into()];
        assert_eq!(
            choose(&quote, &wallet, "x.dev", false).unwrap().chain.id,
            chains::SOLANA_DEVNET
        );
        // With Solana's address gone, Base Sepolia it is again.
        wallet.accounts[0].solana.clear();
        assert_eq!(
            choose(&quote, &wallet, "x.dev", false).unwrap().chain.id,
            "eip155:84532"
        );
        // Test networks off: nothing can be paid, and the reasons say so.
        wallet.policy.testnets = false;
        let why = choose(&quote, &wallet, "x.dev", false).unwrap_err();
        assert!(
            why.contains("switched off") && why.contains("`upto`"),
            "{why}"
        );
    }

    /// A one-request-at-a-time HTTP server on this machine: the first
    /// request is answered 402 with `required`, and every later one is
    /// handed to `paid`, which gives the status and headers to answer with.
    fn stub(
        required: (Vec<(String, String)>, String),
        paid: impl Fn(&str) -> (u16, Vec<(String, String)>) + Send + 'static,
    ) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/data", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut headers = Vec::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    headers.push(line);
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let payment = headers
                    .iter()
                    .find_map(|h| {
                        let lower = h.to_lowercase();
                        (lower.starts_with("payment-signature:") || lower.starts_with("x-payment:"))
                            .then(|| h.split_once(':').unwrap().1.trim().to_owned())
                    })
                    .unwrap_or_default();
                let (status, extra, body) = if n == 0 {
                    (402, required.0.clone(), required.1.clone())
                } else {
                    let (status, extra) = paid(&payment);
                    (status, extra, "{\"price\": 42}".to_owned())
                };
                let mut reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in extra {
                    reply.push_str(&format!("{name}: {value}\r\n"));
                }
                reply.push_str("\r\n");
                reply.push_str(&body);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        address
    }

    /// The signer of a version 2 EVM payment, checked as a facilitator
    /// would: the signature recovers to `from` over the authorization.
    fn signer(payment: &str) -> Option<String> {
        let body: Value = serde_json::from_slice(&STANDARD.decode(payment).ok()?).ok()?;
        let payload = &body["payload"];
        let auth = &payload["authorization"];
        let bytes = super::super::keys::from_hex(payload["signature"].as_str()?).ok()?;
        let nonce: [u8; 32] = super::super::keys::from_hex(auth["nonce"].as_str()?)
            .ok()?
            .try_into()
            .ok()?;
        let authorization = super::super::evm::Authorization {
            from: super::super::keys::evm_bytes(auth["from"].as_str()?).ok()?,
            to: super::super::keys::evm_bytes(auth["to"].as_str()?).ok()?,
            value: auth["value"].as_str()?.parse().ok()?,
            valid_after: auth["validAfter"].as_str()?.parse().ok()?,
            valid_before: auth["validBefore"].as_str()?.parse().ok()?,
            nonce,
        };
        let digest = super::super::evm::test_digest(
            "USDC",
            "2",
            84532,
            "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
            &authorization,
        );
        let signature = k256::ecdsa::Signature::from_slice(&bytes[..64]).ok()?;
        let id = k256::ecdsa::RecoveryId::from_byte(bytes[64] - 27)?;
        let key = k256::ecdsa::VerifyingKey::recover_from_prehash(&digest, &signature, id).ok()?;
        (super::super::keys::address_of(&key) == auth["from"].as_str()?)
            .then(|| auth["from"].as_str().unwrap().to_owned())
    }

    #[test]
    fn a_402_is_paid_on_its_own_within_the_rules_and_the_page_comes_back() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new("wallet-x402");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        let made = super::super::import("Main", "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about", 0).unwrap();
        super::super::change(|wallet| {
            wallet.policy.testnets = true;
            wallet.policy.x402 = X402::Auto;
            Ok(())
        })
        .unwrap();
        for version in [2, 1] {
            let required = if version == 2 {
                let body =
                    json!({"x402Version": 2, "resource": {"url": "x"}, "accepts": [offer()]});
                (
                    vec![(
                        "PAYMENT-REQUIRED".to_owned(),
                        STANDARD.encode(body.to_string()),
                    )],
                    String::new(),
                )
            } else {
                let mut v1 = offer();
                v1["network"] = "base-sepolia".into();
                v1["maxAmountRequired"] = v1["amount"].take();
                (
                    Vec::new(),
                    json!({"x402Version": 1, "accepts": [v1]}).to_string(),
                )
            };
            let url = stub(required, move |payment| match signer(payment) {
                Some(_) => {
                    let settled =
                        json!({"success": true, "transaction": "0xabc", "network": "eip155:84532"});
                    (
                        200,
                        vec![(
                            "PAYMENT-RESPONSE".to_owned(),
                            STANDARD.encode(settled.to_string()),
                        )],
                    )
                }
                None => (402, Vec::new()),
            });
            let url = reqwest::Url::parse(&url).unwrap();
            let first = reqwest::blocking::get(url.clone()).unwrap();
            let headers = first.headers().clone();
            let body = first.bytes().unwrap();
            // Not funded on the chain, so the plan is made without asking it.
            let quote = read(&headers, &body).unwrap();
            let plan = choose(&quote, &super::super::load(), "127.0.0.1", false).unwrap();
            assert_eq!(plan.account.evm, made.evm);
            let (page, entry) =
                pay_and_fetch(&plan, reqwest::Method::GET, &url, None, Kind::X402).unwrap();
            assert_eq!(page.text().unwrap(), "{\"price\": 42}");
            assert_eq!((entry.status, entry.tx.as_str()), (Status::Paid, "0xabc"));
            assert!(entry.link.ends_with("/tx/0xabc"));
        }
        let paid = policy::ledger();
        assert_eq!(paid.len(), 2);
        assert_eq!(policy::spent(&paid, crate::chat::now() as i64).day, 20_000);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_payment_the_site_refuses_keeps_counting() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new("wallet-x402-refused");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        super::super::import("Main", "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about", 0).unwrap();
        super::super::change(|wallet| {
            wallet.policy.testnets = true;
            Ok(())
        })
        .unwrap();
        let body = json!({"x402Version": 2, "accepts": [offer()]});
        let required = (
            vec![(
                "PAYMENT-REQUIRED".to_owned(),
                STANDARD.encode(body.to_string()),
            )],
            String::new(),
        );
        let again = STANDARD.encode(
            json!({"x402Version": 2, "error": "insufficient_funds", "accepts": [offer()]})
                .to_string(),
        );
        let url = stub(required, move |_| {
            (402, vec![("PAYMENT-REQUIRED".to_owned(), again.clone())])
        });
        let url = reqwest::Url::parse(&url).unwrap();
        let first = reqwest::blocking::get(url.clone()).unwrap();
        let quote = read(&first.headers().clone(), &first.bytes().unwrap()).unwrap();
        let plan = choose(&quote, &super::super::load(), "127.0.0.1", false).unwrap();
        let why = pay_and_fetch(&plan, reqwest::Method::GET, &url, None, Kind::X402).unwrap_err();
        assert!(why.contains("insufficient_funds"), "{why}");
        let entry = policy::ledger().pop().unwrap();
        assert_eq!(entry.status, Status::Unsettled);
        assert_eq!(
            policy::spent(&[entry], crate::chat::now() as i64).day,
            10_000
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// A JSON-RPC server on this machine that answers every call with
    /// `result`, as a chain asked for a balance would.
    fn rpc_stub(result: &str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/", listener.local_addr().unwrap());
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": result}).to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut request = vec![0; length];
                let _ = reader.read_exact(&mut request);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        address
    }

    #[test]
    fn web_fetch_pays_a_small_price_on_its_own_and_hands_a_larger_one_to_the_user() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new("wallet-web-fetch");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        super::super::import("Main", "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about", 0).unwrap();
        // The chain says the wallet holds a dollar of USDC.
        let chain = rpc_stub(&format!("0x{:064x}", 1_000_000));
        super::super::change(move |wallet| {
            wallet.policy.testnets = true;
            wallet.policy.x402 = X402::Auto;
            wallet.networks.insert(
                "eip155:84532".into(),
                chains::Network {
                    on: None,
                    rpc: chain,
                },
            );
            Ok(())
        })
        .unwrap();
        let site = |amount: &str| {
            let mut priced = offer();
            priced["amount"] = amount.into();
            let body = json!({"x402Version": 2, "accepts": [priced]});
            let required = (
                vec![(
                    "PAYMENT-REQUIRED".to_owned(),
                    STANDARD.encode(body.to_string()),
                )],
                String::new(),
            );
            stub(required, |payment| match signer(payment) {
                Some(_) => (200, Vec::new()),
                None => (402, Vec::new()),
            })
        };
        let fetch = |url: &str| {
            crate::tools::run(
                "web_fetch",
                &json!({"url": url}).to_string(),
                &crate::tools::ToolContext::default(),
            )
        };
        // A cent, under the ten cents paid unasked: the page comes back.
        let page = fetch(&site("10000")).unwrap();
        assert!(page.contains("{\"price\": 42}"), "{page}");
        assert_eq!(policy::ledger().len(), 1);
        // Fifty cents: the model is told the price and how to ask.
        let dear = site("500000");
        let why = fetch(&dear).unwrap_err();
        assert!(
            why.contains("0.5 USDC ($0.50) on Base Sepolia") && why.contains("paid_fetch"),
            "{why}"
        );
        assert_eq!(remembered(&dear).unwrap().usd, Some(500_000));
        assert_eq!(policy::ledger().len(), 1);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }
}
