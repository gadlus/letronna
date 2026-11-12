//! Signing in to MCP servers reached by URL, the OAuth 2.1 way the MCP spec
//! has it, for the hosted servers that take no key in a header.
//!
//! A server that answers 401 names where its sign-in is described (RFC
//! 9728), which names the authorization server, whose own description (RFC
//! 8414, or OpenID's) says where to send the browser and where to trade for
//! a token; the issuer it gives must be the one asked. Letronna registers
//! itself there (RFC 7591) unless `oauth.clientId` in mcp.json gives an id,
//! and keeps the app to use again. The browser comes back to a port on
//! 127.0.0.1 that only this machine can reach (`localhost` at the fixed
//! `callbackPort` an id registered by hand names, heard on both loopbacks),
//! once, on an exact path, with a code bound to a PKCE secret that never
//! leaves here and a state checked on return. A browser elsewhere, as over
//! SSH, shows the address it ended on, which can be pasted instead.
//! Letronna's own sign-in in `auth.rs` is left as it is: its page speaks
//! another shape.
//!
//! Every token is asked for this one server (RFC 8707) and saved 0600 under
//! `mcp-tokens`, named by a hash of the server's URL, so a server renamed in
//! mcp.json cannot pick up another's. Tokens never reach the model, a
//! session file or the log. One about to lapse, or refused with a 401, is
//! refreshed first, once however many requests found it stale. A refresh
//! the server refuses asks for a sign-in again, unless the other frontend
//! turned the token over meanwhile; one that cannot reach it keeps the
//! token, and the next request tries again. A 403 for want of a scope asks
//! the user to sign in again for it along with what was granted before.
//!
//! A server that needs a sign-in offers one tool in place of its own,
//! `mcp__<server>__authenticate`: a call asks the user on its card, opens
//! the page once they say yes, and tells the model to wait. Only a yes on
//! the card starts it, asked every time; a call let through by a setting
//! starts nothing. The page's address carries the state, so it goes to the
//! user alone, and a failure is kept for /mcp, the window and the next call.
//!
//! A server elsewhere names every address fetched here, so none of them may
//! lead where the server itself is not: https unless it runs on this
//! machine, no redirects, a time and size cap on each answer, the
//! description at the server's own origin, and no address on this machine
//! or its network unless the server is there too. Where the server is, is
//! the outermost of the address it answered from and every one its name
//! has, so no second answer or extra record widens it; each fetch connects
//! to an address it checked, directly, as a proxy would look the name up
//! again.

use crate::providers::auth::{form_encoded, listen, pasted, pkce, random, reply, returned};
use crate::tools::{Tool, ToolContext};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long the port waits for the browser: long enough to type a password
/// and a second factor, short enough that a forgotten tab closes it.
const PATIENCE: Duration = Duration::from_secs(5 * 60);

/// How long one fetch of a description, a registration or a token may take.
const FETCH_TIME: Duration = Duration::from_secs(15);

/// The most of an answer read: descriptions and tokens are a few KB.
const ANSWER_CAP: u64 = 64 * 1024;

/// How many seconds before it lapses a token is refreshed, so it does not
/// lapse on the way.
const EARLY: u64 = 60;

/// The one path the browser may come back to.
const CALLBACK: &str = "/callback";

/// What a sign-in stopped by the user ends with, which is no failure.
pub const CANCELLED: &str = "Sign-in cancelled.";

/// Whether the user has stopped a sign-in that is still finding its way.
type Stop<'a> = &'a dyn Fn() -> bool;

/// `oauth` in a server's mcp.json entry, in Claude Code's words.
#[derive(Deserialize, Clone, Default, PartialEq)]
pub(crate) struct OAuthConfig {
    /// An id the server gave out ahead, for one that registers no clients.
    #[serde(default, rename = "clientId")]
    pub client_id: Option<String>,
    /// Its secret, for a server that wants one with the id.
    #[serde(default, rename = "clientSecret")]
    pub client_secret: Option<String>,
    /// A fixed port for the browser to come back to, for an id registered
    /// with one.
    #[serde(default, rename = "callbackPort")]
    pub callback_port: Option<u16>,
}

/// A saved sign-in to one server.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
struct Token {
    /// The server's URL, checked on reading, so a file never serves another.
    url: String,
    issuer: String,
    token_endpoint: String,
    /// What the token was asked for (RFC 8707), asked again on refresh.
    resource: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// Unix seconds; None when the server did not say.
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    /// Whether the secret goes in a Basic header, the one way its token
    /// endpoint takes it.
    #[serde(default)]
    basic: bool,
}

/// Printed with its secret hidden, so no log line can carry it.
impl std::fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[hidden]"),
            )
            .field("callback_port", &self.callback_port)
            .finish()
    }
}

/// Printed with its tokens and secret hidden, so no log line can carry them.
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let hidden = |held: &Option<String>| held.as_ref().map(|_| "[hidden]");
        f.debug_struct("Token")
            .field("url", &self.url)
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &hidden(&self.client_secret))
            .field("access_token", &"[hidden]")
            .field("refresh_token", &hidden(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// What a 401 or 403 said of signing in, in its `WWW-Authenticate`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Challenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
    /// Whether it names the Bearer scheme, as a server that signs in does.
    pub bearer: bool,
    /// The address the request it answered was sent to, which is where the
    /// server really is.
    pub peer: Option<IpAddr>,
}

impl Challenge {
    /// Whether it asks for an OAuth sign-in, rather than being a key's plain
    /// refusal.
    pub(crate) fn wants_sign_in(&self) -> bool {
        self.bearer || self.resource_metadata.is_some()
    }
}

/// The parameters of a `WWW-Authenticate` header that matter here, quoted
/// or not, and whether one of its schemes is Bearer.
pub(crate) fn challenge(header: &str) -> Challenge {
    let mut found = Challenge {
        bearer: header.split(',').any(|part| {
            let first = part.split_whitespace().next().unwrap_or_default();
            first.eq_ignore_ascii_case("bearer")
        }),
        ..Challenge::default()
    };
    let mut rest = header;
    while let Some(at) = rest.find('=') {
        let key = rest[..at].rsplit([' ', ',']).next().unwrap_or("").trim();
        let after = &rest[at + 1..];
        let (value, next) = match after.strip_prefix('"') {
            Some(quoted) => {
                let end = quoted.find('"').unwrap_or(quoted.len());
                (&quoted[..end], quoted.get(end + 1..).unwrap_or(""))
            }
            None => {
                let end = after.find(',').unwrap_or(after.len());
                (after[..end].trim(), &after[end..])
            }
        };
        let value = Some(value.to_owned()).filter(|v| !v.is_empty());
        match key.to_ascii_lowercase().as_str() {
            "resource_metadata" => found.resource_metadata = value,
            "scope" => found.scope = value,
            "error" => found.error = value,
            _ => {}
        }
        rest = next;
    }
    found
}

/// How far inside a network an address is, from anywhere to this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    /// Reachable from anywhere.
    Public,
    /// A home, office or carrier network: RFC 1918, shared (CGNAT) and IPv6
    /// unique local addresses.
    Private,
    /// This machine, or the cloud host under it: loopback, link-local (where
    /// cloud metadata lives), unspecified, broadcast and multicast.
    Local,
}

/// Where `ip` is, an IPv4 address carried in an IPv6 one read as the IPv4
/// one: mapped, compatible (`::a.b.c.d`), NAT64 and 6to4.
fn place(ip: IpAddr) -> Place {
    let ip = match ip {
        IpAddr::V6(v6) if !v6.is_loopback() && !v6.is_unspecified() => {
            let [a, b, c, _, _, _, g, h] = v6.segments();
            let v4 =
                |hi: u16, lo: u16| IpAddr::V4(Ipv4Addr::from(u32::from(hi) << 16 | u32::from(lo)));
            match v6.to_ipv4() {
                Some(inside) => IpAddr::V4(inside),
                None if [a, b] == [0x64, 0xff9b] => v4(g, h),
                None if a == 0x2002 => v4(b, c),
                None => ip,
            }
        }
        other => other,
    };
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if a == 0
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
            {
                Place::Local
            } else if v4.is_private() || (a == 100 && (64..128).contains(&b)) {
                Place::Private
            } else {
                Place::Public
            }
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || first & 0xffc0 == 0xfe80
            {
                Place::Local
            } else if first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfec0 {
                Place::Private
            } else {
                Place::Public
            }
        }
    }
}

/// Where the fetches for one server's sign-in may go: no further in than the
/// server itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reach(Place);

impl Reach {
    /// The reach of the server at `url`, which `peer` answered as: the
    /// outermost of that and every address its name resolves to now, so no
    /// second answer or extra record can widen it.
    fn of(url: &Url, peer: Option<IpAddr>) -> Result<Self, String> {
        let found = resolve(url)?;
        Ok(Self::outermost(
            found.iter().map(SocketAddr::ip).chain(peer),
        ))
    }

    /// The reach of a server at all of `addresses`: as far out as the
    /// furthest, since any one of them may be the one really answering.
    fn outermost(addresses: impl Iterator<Item = IpAddr>) -> Self {
        Self(addresses.map(place).min().unwrap_or(Place::Public))
    }
}

/// `text` as an address this sign-in may use: https, or http as well for a
/// server on this machine, with a host and no password in it.
fn checked(text: &str, reach: Reach) -> Result<Url, String> {
    // The server's words, so not repeated: they could be anything.
    let url = Url::parse(text.trim()).map_err(|_| "An address it gave is not one.".to_owned())?;
    let plain = url.scheme() == "http" && reach.0 == Place::Local;
    if url.scheme() != "https" && !plain {
        return Err(format!("{url} is not https, which a sign-in needs."));
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{url} is not an address a sign-in can use."));
    }
    Ok(url)
}

/// The addresses `url`'s host resolves to.
fn resolve(url: &Url) -> Result<Vec<SocketAddr>, String> {
    let host = url.host_str().unwrap_or_default();
    let port = url.port_or_known_default().unwrap_or(443);
    // An IPv6 host keeps its brackets in a URL, which a lookup does not take.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    (bare, port)
        .to_socket_addrs()
        .map(Iterator::collect)
        .map_err(|e| format!("Cannot find {host}: {e}"))
}

/// Send what `build` makes of `url` no further in than `reach`, following no
/// redirect, and read its JSON answer up to `ANSWER_CAP`: the status, and
/// the answer or null.
fn fetch(
    url: &Url,
    reach: Reach,
    build: impl FnOnce(&reqwest::blocking::Client, Url) -> reqwest::blocking::RequestBuilder,
) -> Result<(u16, Value), String> {
    let host = url.host_str().unwrap_or_default();
    let kept: Vec<SocketAddr> = resolve(url)?
        .into_iter()
        .filter(|addr| place(addr.ip()) <= reach.0)
        .collect();
    if kept.is_empty() {
        return Err(format!(
            "{host} is on this machine or its network, where a server elsewhere may not send Letronna."
        ));
    }
    // The addresses checked are the ones connected to, and no proxy stands
    // between to look the name up again.
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(FETCH_TIME)
        .resolve_to_addrs(host, &kept)
        .build()
        .map_err(|e| e.to_string())?;
    let reply = build(&client, url.clone())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("Could not reach {url}: {e}"))?;
    let status = reply.status().as_u16();
    let mut body = Vec::new();
    reply
        .take(ANSWER_CAP + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("{url} stopped answering: {e}"))?;
    if body.len() as u64 > ANSWER_CAP {
        return Err(format!("{url} answered with more than 64 KB."));
    }
    Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

/// Where and how to sign in to one server, as its descriptions say.
#[derive(Clone, Debug, PartialEq)]
struct Found {
    /// What a token is asked for: the server's own word for itself, else its
    /// URL.
    resource: String,
    issuer: String,
    authorize: Url,
    token: Url,
    register: Option<Url>,
    /// The scopes the server lists, asked for when no challenge names any.
    scopes: Option<String>,
    /// Whether an app's secret goes in a Basic header rather than the form.
    basic: bool,
}

/// `url` as the one name of its server, which its token is saved under and,
/// with nothing better, asked for: no fragment and no trailing slash.
fn canonical(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.as_str().trim_end_matches('/').to_owned()
}

/// What a token for the server at `url` is asked for when it does not say:
/// its URL without a query (RFC 8707), which may hold a key.
fn resource_of(url: &Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    canonical(&url)
}

fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Where a server's description may be, best first (RFC 9728): where its
/// challenge said, when that is at its own origin, then the well-known
/// address with its path, then without.
fn resource_addresses(server: &Url, named: Option<&str>) -> Vec<String> {
    let base = origin(server);
    let mut all: Vec<String> = named
        .and_then(|named| Url::parse(named).ok())
        .filter(|named| origin(named) == base)
        .map(String::from)
        .into_iter()
        .collect();
    let path = server.path().trim_end_matches('/');
    if !path.is_empty() {
        all.push(format!("{base}/.well-known/oauth-protected-resource{path}"));
    }
    all.push(format!("{base}/.well-known/oauth-protected-resource"));
    all
}

/// Where an authorization server's description may be, in the order the
/// MCP spec tries them: RFC 8414's, then OpenID's both ways round.
fn issuer_addresses(issuer: &Url) -> Vec<String> {
    let base = origin(issuer);
    let path = issuer.path().trim_end_matches('/');
    if path.is_empty() {
        return vec![
            format!("{base}/.well-known/oauth-authorization-server"),
            format!("{base}/.well-known/openid-configuration"),
        ];
    }
    vec![
        format!("{base}/.well-known/oauth-authorization-server{path}"),
        format!("{base}/.well-known/openid-configuration{path}"),
        format!("{base}{path}/.well-known/openid-configuration"),
    ]
}

/// Whether a description of `resource` speaks for the server at `server`:
/// the same origin, and its path at or above the server's.
fn covers(resource: &str, server: &Url) -> bool {
    let Ok(resource) = Url::parse(resource) else {
        return false;
    };
    let above = resource.path().trim_end_matches('/');
    let path = server.path().trim_end_matches('/');
    origin(&resource) == origin(server)
        && path
            .strip_prefix(above)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The first of `addresses` that answers with a JSON object, or why none
/// did; asking the next stops once `stop` says so.
fn first_described(addresses: &[String], reach: Reach, stop: Stop) -> Result<Value, String> {
    let mut why = String::from("nothing described it");
    for at in addresses {
        if stop() {
            return Err(CANCELLED.into());
        }
        let answered = checked(at, reach).and_then(|url| fetch(&url, reach, |c, url| c.get(url)));
        match answered {
            Ok((200, doc)) if doc.is_object() => return Ok(doc),
            Ok((status, _)) => why = format!("{at} answered HTTP {status}"),
            Err(error) => why = error,
        }
    }
    Err(why)
}

/// Find where to sign in to the server at `server`, from what its challenge
/// said.
fn discover(server: &Url, heard: &Challenge, reach: Reach, stop: Stop) -> Result<Found, String> {
    let described = first_described(
        &resource_addresses(server, heard.resource_metadata.as_deref()),
        reach,
        stop,
    );
    if stop() {
        return Err(CANCELLED.into());
    }
    let (resource, issuer, scopes) = match described {
        Ok(doc) => {
            let resource = doc["resource"].as_str().map(str::to_owned);
            if let Some(resource) = &resource
                && !covers(resource, server)
            {
                return Err(format!(
                    "its description is of another server than {server}."
                ));
            }
            let Some(issuer) = doc["authorization_servers"][0].as_str() else {
                return Err("its description names no authorization server.".into());
            };
            let scopes = doc["scopes_supported"].as_array().map(|all| {
                all.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            let resource = resource.unwrap_or_else(|| resource_of(server));
            (
                resource,
                issuer.to_owned(),
                scopes.filter(|s| !s.is_empty()),
            )
        }
        // A server from before RFC 9728 was in the spec signs in at its own
        // origin.
        Err(_) => (resource_of(server), origin(server), None),
    };
    let issuer_url = checked(&issuer, reach)?;
    let meta = first_described(&issuer_addresses(&issuer_url), reach, stop)
        .map_err(|why| format!("its authorization server {issuer} is not described: {why}"))?;
    let said = meta["issuer"].as_str().unwrap_or_default();
    if said.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(format!("{issuer} describes itself as another issuer."));
    }
    let methods = meta["code_challenge_methods_supported"].as_array();
    if methods.is_some_and(|all| !all.iter().any(|m| m == "S256")) {
        return Err(format!("{issuer} does not take PKCE with S256."));
    }
    let endpoint = |key: &str| {
        let text = meta[key]
            .as_str()
            .ok_or_else(|| format!("{issuer} gives no {key}."))?;
        checked(text, reach)
    };
    let (authorize, token) = (
        endpoint("authorization_endpoint")?,
        endpoint("token_endpoint")?,
    );
    let register = meta["registration_endpoint"]
        .as_str()
        .map(|text| checked(text, reach))
        .transpose()?;
    // client_secret_post is the default (RFC 8414), so Basic only where it
    // is the one way listed.
    let auth_methods = meta["token_endpoint_auth_methods_supported"].as_array();
    let basic = auth_methods.is_some_and(|all| {
        all.iter().any(|m| m == "client_secret_basic")
            && !all.iter().any(|m| m == "client_secret_post")
    });
    Ok(Found {
        resource,
        issuer,
        authorize,
        token,
        register,
        scopes,
        basic,
    })
}

/// A sign-in under way: the page to send the browser to, and the port it
/// comes back to.
pub struct SignIn {
    /// The server's name in mcp.json.
    pub server: String,
    /// Where to send the browser.
    pub url: String,
    /// Taken by the wait, and closed as it ends: one on 127.0.0.1, and one
    /// on [::1] too where `localhost` is the address given.
    listeners: Mutex<Vec<TcpListener>>,
    redirect: String,
    state: String,
    verifier: String,
    found: Found,
    client_id: String,
    client_secret: Option<String>,
    /// Where the app registered for this sign-in is kept, forgotten if the
    /// authorization server refuses it.
    client_file: Option<PathBuf>,
    scope: Option<String>,
    reach: Reach,
    /// The server's URL, which the token is saved under.
    canonical: String,
    file: PathBuf,
    /// A code the user pasted, for the wait to take.
    pasted: Mutex<Option<String>>,
    answered: AtomicBool,
    stop: AtomicBool,
}

/// Start signing in to the server `name` in mcp.json: find where, register
/// if need be, and open the port the browser comes back to, unless `stop`
/// says to give up first. Nothing is opened in a browser here.
pub fn begin(name: &str, stop: Stop) -> Result<Arc<SignIn>, String> {
    let (url, oauth) = crate::mcp::remote(name)?;
    let sign_in = Arc::new(start(name, &url, &oauth, &tokens_dir(), stop)?);
    // One sign-in per server at a time: an older one still waiting stops.
    if let Ok(mut all) = flows().lock()
        && let Some(older) = all.insert(name.to_owned(), Arc::downgrade(&sign_in))
        && let Some(older) = older.upgrade()
    {
        older.cancel();
    }
    Ok(sign_in)
}

/// The sign-in to the server `name` still waiting, whoever started it.
pub fn pending(name: &str) -> Option<Arc<SignIn>> {
    let all = flows().lock().ok()?;
    let sign_in = all.get(name)?.upgrade()?;
    let ended = sign_in.stop.load(Ordering::Relaxed) || sign_in.answered();
    (!ended).then_some(sign_in)
}

/// Why the last sign-in to the server `name` failed, if it did.
fn last_failure(name: &str) -> Option<String> {
    failures().lock().ok()?.get(name).cloned()
}

/// Why each server's last sign-in failed, by name.
fn failures() -> &'static Mutex<HashMap<String, String>> {
    static FAILURES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    FAILURES.get_or_init(Default::default)
}

/// The sign-ins under way, by server name.
fn flows() -> &'static Mutex<HashMap<String, std::sync::Weak<SignIn>>> {
    static FLOWS: OnceLock<Mutex<HashMap<String, std::sync::Weak<SignIn>>>> = OnceLock::new();
    FLOWS.get_or_init(Default::default)
}

/// A sign-in to the server `name` at `url`, its token to be saved in `dir`,
/// given up once `stop` says so.
fn start(
    name: &str,
    url: &str,
    oauth: &OAuthConfig,
    dir: &Path,
    stop: Stop,
) -> Result<SignIn, String> {
    let server =
        Url::parse(url.trim()).map_err(|_| format!("{name}: `{url}` is not an address."))?;
    let canonical = canonical(&server);
    let heard = heard(&canonical);
    let reach = Reach::of(&server, heard.peer).map_err(|e| format!("{name}: {e}"))?;
    let found = discover(&server, &heard, reach, stop).map_err(|e| match stop() {
        true => CANCELLED.to_owned(),
        false => format!("{name}: {e}"),
    })?;
    let app_file = token_file(dir, &format!("app {}", found.issuer));
    let kept = (oauth.client_id.is_none())
        .then(|| load_app(&app_file, &found.issuer))
        .flatten();
    let (listeners, redirect) = match oauth.callback_port {
        // As Claude Code sends it, so an id registered for it by hand fits.
        Some(port) => (
            listen(port, true)?.0,
            format!("http://localhost:{port}{CALLBACK}"),
        ),
        None => {
            // The kept app's port if it is free, so that app serves again.
            let again = kept
                .as_ref()
                .and_then(|app| Url::parse(&app.redirect).ok()?.port());
            let (listeners, port) = match again.and_then(|port| listen(port, false).ok()) {
                Some(listening) => listening,
                None => listen(0, false)?,
            };
            (listeners, format!("http://127.0.0.1:{port}{CALLBACK}"))
        }
    };
    let kept = kept.filter(|app| app.redirect == redirect);
    let (client_id, client_secret, client_file) = match (&oauth.client_id, kept, &found.register) {
        (Some(id), _, _) => (id.clone(), oauth.client_secret.clone(), None),
        (None, Some(app), _) => (app.client_id, app.client_secret, Some(app_file)),
        (None, None, Some(register)) if !stop() => {
            let (id, secret) =
                registered(register, &redirect, reach).map_err(|e| format!("{name}: {e}"))?;
            let app = App {
                issuer: found.issuer.clone(),
                redirect: redirect.clone(),
                client_id: id.clone(),
                client_secret: secret.clone(),
            };
            // One not kept is only registered again next time.
            let _ = save(&app_file, &app);
            (id, secret, Some(app_file))
        }
        (None, None, Some(_)) => return Err(CANCELLED.into()),
        (None, None, None) => {
            return Err(format!(
                "{name}'s sign-in takes no new apps: give it the id it gave you as \
                 oauth.clientId in mcp.json."
            ));
        }
    };
    let file = token_file(dir, &canonical);
    // A step up asks for what was granted before as well as what is missing.
    let before = load(&file, &canonical).and_then(|token| token.scope);
    let scope = joined(
        before.as_deref(),
        heard.scope.as_deref().or(found.scopes.as_deref()),
    );
    let (verifier, state) = (random(32)?, random(16)?);
    let asked = Asked {
        client_id: &client_id,
        redirect: &redirect,
        state: &state,
        challenge: &pkce(&verifier),
        scope: scope.as_deref(),
    };
    Ok(SignIn {
        server: name.to_owned(),
        url: authorize_url(&found, &asked),
        listeners: Mutex::new(listeners),
        redirect,
        state,
        verifier,
        found,
        client_id,
        client_secret,
        client_file,
        scope,
        reach,
        canonical,
        file,
        pasted: Mutex::default(),
        answered: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    })
}

/// What the sign-in page is asked for, besides where it is.
struct Asked<'a> {
    client_id: &'a str,
    redirect: &'a str,
    state: &'a str,
    challenge: &'a str,
    scope: Option<&'a str>,
}

/// The address of the sign-in page, asking for a code for this server alone.
fn authorize_url(found: &Found, asked: &Asked) -> String {
    let mut url = found.authorize.clone();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", asked.client_id)
            .append_pair("redirect_uri", asked.redirect)
            .append_pair("state", asked.state)
            .append_pair("code_challenge", asked.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &found.resource);
        if let Some(scope) = asked.scope {
            query.append_pair("scope", scope);
        }
    }
    url.into()
}

/// The scopes in `before` and `more`, each once, in the order first named.
fn joined(before: Option<&str>, more: Option<&str>) -> Option<String> {
    let mut all: Vec<&str> = Vec::new();
    for scope in before
        .into_iter()
        .chain(more)
        .flat_map(str::split_whitespace)
    {
        if !all.contains(&scope) {
            all.push(scope);
        }
    }
    (!all.is_empty()).then(|| all.join(" "))
}

/// An app Letronna registered with an authorization server, kept beside the
/// tokens to sign in with again rather than registering anew each time.
#[derive(Serialize, Deserialize)]
struct App {
    issuer: String,
    /// The one address it was registered to come back to.
    redirect: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

/// The app kept in `file`, if it is the one registered with `issuer`.
fn load_app(file: &Path, issuer: &str) -> Option<App> {
    let text = std::fs::read_to_string(file).ok()?;
    serde_json::from_str::<App>(&text)
        .ok()
        .filter(|app| app.issuer == issuer)
}

/// Register Letronna with the authorization server (RFC 7591) as an app
/// with no secret that comes back to `redirect`: its id, and a secret if it
/// gave one anyway.
fn registered(at: &Url, redirect: &str, reach: Reach) -> Result<(String, Option<String>), String> {
    let asked = json!({
        "client_name": "Letronna",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let (status, reply) = fetch(at, reach, |client, url| client.post(url).json(&asked))?;
    let id = reply["client_id"]
        .as_str()
        .filter(|_| (200..300).contains(&status));
    let Some(id) = id else {
        return Err(format!("{at} would not register Letronna (HTTP {status})."));
    };
    Ok((
        id.to_owned(),
        reply["client_secret"].as_str().map(str::to_owned),
    ))
}

impl SignIn {
    /// Take the address the browser ended on, or the code on it, pasted by
    /// the user for a browser that cannot reach this machine.
    pub fn paste(&self, text: &str) -> Result<(), String> {
        let code = pasted(text, &self.state)?;
        if let Ok(mut pasted) = self.pasted.lock() {
            *pasted = Some(code);
        }
        self.answered.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// End the wait at once, for a Cancel button or escape.
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether a code has come, from the browser or pasted, so that all
    /// `finish` has left is trading it for a token.
    pub fn answered(&self) -> bool {
        self.answered.load(Ordering::Relaxed)
    }

    /// Wait for the code, trade it for a token, save that, and start the
    /// server again with it. A failure is kept for /mcp, the window and the
    /// model's next call, as one the model started has nobody waiting on it.
    pub fn finish(&self) -> Result<(), String> {
        let outcome = self.complete();
        let Ok(mut all) = failures().lock() else {
            return outcome;
        };
        match &outcome {
            Ok(()) => drop(all.remove(&self.server)),
            Err(why) if why == CANCELLED => {}
            Err(why) => {
                all.insert(self.server.clone(), why.clone());
                drop(all);
                crate::mcp::sign_in_failed(&self.server, why);
            }
        }
        outcome
    }

    /// `finish`, but for keeping how it went.
    fn complete(&self) -> Result<(), String> {
        let token = self.traded().map_err(|failed| {
            // An app the server may have forgotten, as one that shows no page
            // for it would, is registered again next time; one that could not
            // be reached is kept.
            if failed.refused
                && let Some(file) = &self.client_file
            {
                let _ = std::fs::remove_file(file);
            }
            failed.why
        })?;
        save(&self.file, &token)?;
        if let Ok(mut heard) = heard_map().lock() {
            heard.remove(&self.canonical);
        }
        crate::mcp::signed_in(&self.server);
        Ok(())
    }

    /// Wait for the code and trade it for a token.
    fn traded(&self) -> Result<Token, Ungranted> {
        let code = self
            .wait()
            .map_err(|why| Ungranted { refused: true, why })?;
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", self.redirect.as_str()),
            ("code_verifier", self.verifier.as_str()),
            ("resource", self.found.resource.as_str()),
        ];
        let app = Credentials {
            id: &self.client_id,
            secret: self.client_secret.as_deref(),
            basic: self.found.basic,
        };
        let granted =
            grant(&self.found.token, self.reach, &form, &app).map_err(|failed| Ungranted {
                why: format!("{}: {}", self.server, failed.why),
                ..failed
            })?;
        Ok(Token {
            url: self.canonical.clone(),
            issuer: self.found.issuer.clone(),
            token_endpoint: self.found.token.to_string(),
            resource: self.found.resource.clone(),
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            access_token: granted.access,
            refresh_token: granted.refresh,
            expires_at: granted.expires_at,
            scope: granted.scope.or_else(|| self.scope.clone()),
            basic: self.found.basic,
        })
    }

    /// Take knocks on the port until one brings the code, or one is pasted.
    fn wait(&self) -> Result<String, String> {
        let listeners = self
            .listeners
            .lock()
            .map(|mut held| std::mem::take(&mut *held));
        let listeners = listeners.unwrap_or_default();
        if listeners.is_empty() {
            return Err("This sign-in has already ended; start it again.".into());
        }
        // Blocking in `accept` would never look at the time, the switch or
        // a paste, and there may be two ports. Five looks a second, and only
        // while a sign-in waits.
        for listener in &listeners {
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        }
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if self.stop.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            if let Some(code) = self.pasted.lock().ok().and_then(|mut code| code.take()) {
                return Ok(code);
            }
            let knock = listeners.iter().find_map(|l| l.accept().ok());
            let Some((stream, _)) = knock else {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            if let Some(came) = self.callback(stream) {
                self.answered.store(true, Ordering::Relaxed);
                return came;
            }
        }
        // Over: no longer waiting, for whoever looks.
        self.stop.store(true, Ordering::Relaxed);
        Err(format!(
            "The sign-in to {} was not finished in 5 minutes; start it again.",
            self.server
        ))
    }

    /// Read one request, answer the browser with a plain page, and take what
    /// it brought: None for a knock that was not this sign-in coming back.
    fn callback(&self, mut stream: TcpStream) -> Option<Result<String, String>> {
        // Accepted sockets can come out non-blocking like the listener, and a
        // browser's spare connection that never writes must not hold it up.
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
        let mut request = BufReader::new((&stream).take(8 * 1024));
        let mut line = String::new();
        request.read_line(&mut line).ok()?;
        // The rest of the request is read too: a socket closed with some of
        // it unread is reset, and the browser shows that, not the page.
        let mut header = String::new();
        while request.read_line(&mut header).is_ok_and(|n| n > 2) {
            header.clear();
        }
        let target = line.split_whitespace().nth(1)?;
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != CALLBACK {
            reply(&mut stream, "404 Not Found", "Nothing is here.");
            return None;
        }
        // The page says nothing the query said: it is not Letronna's words.
        let came = returned(query, &self.state);
        let words = match &came {
            Some(Ok(_)) => format!(
                "Signed in to {}. You can close this tab and go back to Letronna.",
                self.server
            ),
            Some(Err(_)) => "The sign-in was refused. Go back to Letronna to try again.".into(),
            None => "That did not work: this is not the sign-in Letronna started.".into(),
        };
        reply(&mut stream, "200 OK", &words);
        came
    }
}

/// What a token endpoint gave.
struct Granted {
    access: String,
    refresh: Option<String>,
    expires_at: Option<u64>,
    scope: Option<String>,
}

/// Why a token endpoint gave no token.
#[derive(Debug)]
struct Ungranted {
    /// It said no, rather than failing to answer: asking again with the same
    /// grant will not do.
    refused: bool,
    why: String,
}

/// The app asking a token endpoint for a token.
struct Credentials<'a> {
    id: &'a str,
    secret: Option<&'a str>,
    /// The secret goes in a Basic header, not the form.
    basic: bool,
}

/// POST `form` from `app` to the token endpoint at `at` and take the token
/// it gives.
fn grant(
    at: &Url,
    reach: Reach,
    form: &[(&str, &str)],
    app: &Credentials,
) -> Result<Granted, Ungranted> {
    let mut form = form.to_vec();
    let basic = app.secret.filter(|_| app.basic);
    if basic.is_none() {
        form.push(("client_id", app.id));
        form.extend(app.secret.map(|secret| ("client_secret", secret)));
    }
    let asked = fetch(at, reach, |client, url| {
        let request = client.post(url).form(&form);
        match basic {
            Some(secret) => request.basic_auth(form_encoded(app.id), Some(form_encoded(secret))),
            None => request,
        }
    });
    let (status, reply) = asked.map_err(|why| Ungranted {
        refused: false,
        why,
    })?;
    let access = reply["access_token"]
        .as_str()
        .filter(|t| status == 200 && !t.is_empty());
    let Some(access) = access else {
        let error = reply["error"].as_str().unwrap_or_default();
        let plain =
            !error.is_empty() && error.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        let why = if plain {
            format!(", {error}")
        } else {
            String::new()
        };
        return Err(Ungranted {
            // RFC 6749 answers a grant it refuses with 400 or 401, and some
            // servers with 403; a 5xx, a 429 or a garbled 200 may pass.
            refused: matches!(status, 400 | 401 | 403),
            why: format!("no token was given (HTTP {status}{why})."),
        });
    };
    if let Some(kind) = reply["token_type"].as_str()
        && !kind.eq_ignore_ascii_case("bearer")
    {
        return Err(Ungranted {
            refused: true,
            why: "the token given is not a bearer token.".into(),
        });
    }
    let text = |key: &str| {
        reply[key]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    Ok(Granted {
        access: access.to_owned(),
        refresh: text("refresh_token"),
        expires_at: reply["expires_in"]
            .as_u64()
            .map(|secs| now().saturating_add(secs)),
        scope: text("scope"),
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
thread_local! {
    /// Where this test's thread saves tokens, so no test reaches the user's.
    static TOKENS: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Where the tokens are saved.
#[cfg(not(test))]
fn tokens_dir() -> PathBuf {
    crate::chat::data_dir().join("mcp-tokens")
}

/// Where this test's thread saves tokens; see `TOKENS`.
#[cfg(test)]
fn tokens_dir() -> PathBuf {
    TOKENS
        .with_borrow(Clone::clone)
        .unwrap_or_else(|| std::env::temp_dir().join("letronna-mcp-tokens-unset"))
}

/// The file of the token for the server at `canonical`: a hash, so that no
/// name or URL can pick the path.
fn token_file(dir: &Path, canonical: &str) -> PathBuf {
    let hash = ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes());
    let hex: String = hash.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    dir.join(format!("{hex}.json"))
}

/// The token saved in `file`, if it is the one for the server at `canonical`.
fn load(file: &Path, canonical: &str) -> Option<Token> {
    let text = std::fs::read_to_string(file).ok()?;
    serde_json::from_str::<Token>(&text)
        .ok()
        .filter(|token| token.url == canonical)
}

/// Save `saved` where only this account can read it, whole: written beside
/// it first and moved over it, so the other frontend never reads half a
/// file and a crash midway leaves the old one.
fn save(file: &Path, saved: &impl Serialize) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("Cannot save the sign-in: {e}");
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let text = serde_json::to_string_pretty(saved).map_err(|e| e.to_string())?;
    let temp = file.with_extension(format!("{}.tmp", random(6)?));
    let written = crate::store::write_private(&temp, text.as_bytes())
        .and_then(|()| std::fs::rename(&temp, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.map_err(failed)
}

/// The saved token's file for the server at `url`, if `url` is an address.
fn file_for(url: &str) -> Option<(PathBuf, String)> {
    let canonical = canonical(&Url::parse(url.trim()).ok()?);
    Some((token_file(&tokens_dir(), &canonical), canonical))
}

/// Whether a sign-in to the server at `url` is saved.
pub(crate) fn saved(url: &str) -> bool {
    file_for(url).is_some_and(|(file, _)| file.exists())
}

/// Forget the sign-in to the server `name`, and start it again without it.
/// The token is only forgotten here: the server may keep it until it lapses.
pub fn sign_out(name: &str) -> Result<(), String> {
    let (url, _) = crate::mcp::remote(name)?;
    let Some((file, _)) = file_for(&url) else {
        return Err(format!("{name} is not signed in."));
    };
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{name} is not signed in."));
        }
        Err(e) => return Err(format!("Cannot forget the sign-in to {name}: {e}")),
    }
    crate::mcp::signed_in(name);
    Ok(())
}

/// Every saved token and secret, for hiding wherever one turns up.
pub(crate) fn secrets() -> Vec<String> {
    let Ok(files) = std::fs::read_dir(tokens_dir()) else {
        return Vec::new();
    };
    files
        .flatten()
        .filter_map(|file| std::fs::read_to_string(file.path()).ok())
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        // Tokens and the apps kept beside them alike.
        .flat_map(|saved| {
            ["access_token", "refresh_token", "client_secret"]
                .map(|key| saved[key].as_str().map(str::to_owned))
        })
        .flatten()
        .collect()
}

/// What each server's last 401 or 403 said, by its URL, for the sign-in
/// that follows.
fn heard_map() -> &'static Mutex<HashMap<String, Challenge>> {
    static HEARD: OnceLock<Mutex<HashMap<String, Challenge>>> = OnceLock::new();
    HEARD.get_or_init(Default::default)
}

/// Keep what the server at `url` said of signing in; where it said less
/// than before, what it said before stands.
pub(crate) fn hear(url: &str, said: Challenge) {
    let Some(canonical) = Url::parse(url.trim()).ok().map(|url| canonical(&url)) else {
        return;
    };
    if let Ok(mut heard) = heard_map().lock() {
        let kept = heard.entry(canonical).or_default();
        kept.resource_metadata = said.resource_metadata.or(kept.resource_metadata.take());
        kept.scope = joined(kept.scope.as_deref(), said.scope.as_deref());
        kept.error = said.error;
        kept.bearer |= said.bearer;
        // The outermost of the addresses it answered from, as for a name.
        kept.peer = match (kept.peer, said.peer) {
            (Some(was), Some(now)) if place(was) <= place(now) => Some(was),
            (was, now) => now.or(was),
        };
    }
}

fn heard(canonical: &str) -> Challenge {
    heard_map()
        .lock()
        .ok()
        .and_then(|heard| heard.get(canonical).cloned())
        .unwrap_or_default()
}

/// A server's sign-in as its requests use it: the saved token, read again
/// when its file changes, as when the other frontend refreshed it.
pub(crate) struct Auth {
    canonical: String,
    file: PathBuf,
    held: Mutex<Held>,
    /// Held while refreshing, with the token the last refresh was of and how
    /// it went, so requests that all found the token stale refresh it once
    /// between them and share its outcome; a refresh token may be good only
    /// once.
    refreshing: Mutex<Option<(String, Result<String, Lapse>)>>,
    /// How many refreshes have ended, so a request that waited for one knows
    /// it has.
    refreshed: AtomicU64,
}

/// Why a token could not be renewed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Lapse {
    /// The authorization server could not be asked, or failed: the next
    /// request tries again.
    Unreachable(String),
    /// The refresh was refused, or there is none: only a sign-in will do.
    Gone,
}

/// The token as last read, and when its file was changed then.
#[derive(Default)]
struct Held {
    token: Option<Token>,
    changed: Option<SystemTime>,
}

impl Auth {
    /// The sign-in for the server at `url`, saved or not yet.
    pub(crate) fn new(url: &str) -> Option<Self> {
        let (file, canonical) = file_for(url)?;
        Some(Self {
            canonical,
            file,
            held: Mutex::default(),
            refreshing: Mutex::default(),
            refreshed: AtomicU64::new(0),
        })
    }

    /// The token to send now, refreshed first when it is about to lapse;
    /// None when there is none. One that could not be refreshed is sent as
    /// it is, as it may still be good.
    pub(crate) fn bearer(&self) -> Option<String> {
        let token = self.current()?;
        let by = |secs: u64| {
            token
                .expires_at
                .is_some_and(|at| at <= now().saturating_add(secs))
        };
        // One lapsed already is left to its 401 to refresh, so a request
        // tries once, not twice, when the server cannot be reached.
        if !by(EARLY) || by(0) {
            return Some(token.access_token);
        }
        Some(
            self.renewed(&token.access_token)
                .unwrap_or(token.access_token),
        )
    }

    /// The token to send as it stands, lapsed or not.
    pub(crate) fn token(&self) -> Option<String> {
        self.current().map(|token| token.access_token)
    }

    /// The saved token, read again if its file changed since; one that will
    /// not read keeps the token read before.
    fn current(&self) -> Option<Token> {
        let changed = modified(&self.file);
        let mut held = self.held.lock().ok()?;
        if changed.is_none() || changed != held.changed {
            let read = changed.and_then(|_| load(&self.file, &self.canonical));
            if read.is_some() || changed.is_none() {
                held.token = read;
            }
            held.changed = changed;
        }
        held.token.clone()
    }

    /// The saved token read again whatever its file's time says, and kept.
    fn reread(&self) -> Option<Token> {
        let token = load(&self.file, &self.canonical);
        if let Ok(mut held) = self.held.lock() {
            *held = Held {
                token: token.clone(),
                changed: modified(&self.file),
            };
        }
        token
    }

    /// A token in place of `stale`, one a request was refused or about to be
    /// refused with: one another request or frontend refreshed meanwhile,
    /// else a refresh of it. Requests that wait on one refresh take its
    /// outcome rather than trying again.
    pub(crate) fn renewed(&self, stale: &str) -> Result<String, Lapse> {
        let seen = self.refreshed.load(Ordering::Acquire);
        let mut last = self
            .refreshing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.refreshed.load(Ordering::Acquire) != seen
            && let Some((was, outcome)) = last.as_ref()
            && was == stale
        {
            return outcome.clone();
        }
        let outcome = self.refresh(stale);
        *last = Some((stale.to_owned(), outcome.clone()));
        self.refreshed.fetch_add(1, Ordering::Release);
        outcome
    }

    /// Refresh `stale`, with the refreshing lock held.
    fn refresh(&self, stale: &str) -> Result<String, Lapse> {
        let token = self.current().ok_or(Lapse::Gone)?;
        if token.access_token != stale {
            return Ok(token.access_token);
        }
        let refresh = token.refresh_token.clone().ok_or(Lapse::Gone)?;
        let url = Url::parse(&self.canonical).map_err(|_| Lapse::Gone)?;
        let reach = Reach::of(&url, heard(&self.canonical).peer).map_err(Lapse::Unreachable)?;
        let at = checked(&token.token_endpoint, reach).map_err(|_| Lapse::Gone)?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("resource", token.resource.as_str()),
        ];
        let app = Credentials {
            id: &token.client_id,
            secret: token.client_secret.as_deref(),
            basic: token.basic,
        };
        let granted = match grant(&at, reach, &form, &app) {
            Ok(granted) => granted,
            // Refused, perhaps because the other frontend turned the refresh
            // token over meanwhile and saved the new one.
            Err(failed) if failed.refused => {
                let now = self.reread().filter(|now| now.access_token != stale);
                return now.map(|now| now.access_token).ok_or(Lapse::Gone);
            }
            Err(failed) => return Err(Lapse::Unreachable(failed.why)),
        };
        let fresh = Token {
            access_token: granted.access,
            // A server that does not turn refresh tokens over keeps the one
            // it gave.
            refresh_token: granted.refresh.or_else(|| token.refresh_token.clone()),
            expires_at: granted.expires_at,
            scope: granted.scope.or_else(|| token.scope.clone()),
            ..token
        };
        // Used from memory even when it cannot be saved: the old one may be
        // spent.
        let _ = save(&self.file, &fresh);
        if let Ok(mut held) = self.held.lock() {
            *held = Held {
                token: Some(fresh.clone()),
                changed: modified(&self.file),
            };
        }
        Ok(fresh.access_token)
    }
}

/// When `file` was last changed, if it is there.
fn modified(file: &Path) -> Option<SystemTime> {
    std::fs::metadata(file).and_then(|m| m.modified()).ok()
}

/// The one tool of a server that needs a sign-in, in place of its own:
/// calling it asks the user on its card, and only their yes starts it.
pub(crate) struct Authenticate {
    /// The server's name in mcp.json.
    pub server: String,
    pub name: &'static str,
    pub description: &'static str,
}

/// What the model is told of a server that needs a sign-in.
pub(crate) fn described(server: &str) -> String {
    format!(
        "The {server} MCP server needs the user to sign in before its tools can be used. \
         Call this to ask the user; it opens the sign-in page in their browser once they \
         agree, and the server's own tools take its place when they have signed in."
    )
}

impl Tool for Authenticate {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        false
    }
    fn asked_each_time(&self) -> bool {
        true
    }
    fn preview(&self, _: &Value, _: &ToolContext) -> String {
        format!(
            "Sign in to the {} MCP server: opens its sign-in page in your browser.",
            self.server
        )
    }
    fn run(&self, _: &Value, ctx: &ToolContext) -> Result<String, String> {
        let server = &self.server;
        let how = format!(
            "`/mcp login {server}` in the terminal or Sign in on its row under Capabilities › MCP \
             in the window"
        );
        // What went wrong may quote the server, so it is fenced as its words.
        let before = last_failure(server)
            .map(|why| {
                let lead = format!("The last sign-in to {server} failed:");
                format!("{}\n", crate::untrusted::quoted(&lead, &why))
            })
            .unwrap_or_default();
        // Allowed by a setting, a grant or the judge, nobody said yes to a
        // browser opening.
        if !ctx.approved {
            return Ok(format!(
                "{before}Nothing was started: signing in to {server} needs the user's yes on \
                 this call's card, and it ran without one. Ask the user to sign in with {how}, \
                 and wait for them."
            ));
        }
        let failed = format!("Signing in to {server} could not start:");
        let stop = || ctx.cancel.is_set();
        let sign_in =
            begin(server, &stop).map_err(|why| crate::untrusted::quoted(&failed, &why))?;
        let opened = open_browser(&sign_in.url);
        let waiting = sign_in.clone();
        // How it ends is kept by `finish`, for /mcp, the window and the next call.
        std::thread::spawn(move || waiting.finish());
        // The page's address carries the sign-in's state, so it goes to the
        // user alone, in the window or with /mcp login.
        let page = match opened {
            true => "is open in the user's browser",
            false => "could not be opened in a browser",
        };
        Ok(format!(
            "{before}The sign-in page for {server} {page}. Wait for the user to finish signing \
             in there; the server's tools take this tool's place once they have. Do not call \
             this again meanwhile. If the page did not open or their browser cannot reach this \
             computer, they can sign in with {how}."
        ))
    }
}

/// Open `url` in the user's browser: the one `BROWSER` names when it is
/// set, as on Linux, else the system's own opener. Whether one started; an
/// address that is not http(s) is never opened.
pub fn open_browser(url: &str) -> bool {
    let web = Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"));
    let named = std::env::var("BROWSER").ok();
    let words = opener(
        named.as_deref(),
        url,
        cfg!(windows),
        cfg!(target_os = "macos"),
    );
    let Some((program, args)) = words.split_first().filter(|_| web) else {
        return false;
    };
    let started = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = started else {
        return false;
    };
    // Reaped once it exits, so it is not left a zombie.
    std::thread::spawn(move || child.wait());
    true
}

/// The command line that opens `url`: the first browser `BROWSER` lists,
/// split as the system splits a list of paths, with the address in place
/// of its `%s` or else after it; else the system's own opener. An entry
/// with no `%s` is one program, spaces in its path and all.
fn opener(browser: Option<&str>, url: &str, windows: bool, mac: bool) -> Vec<String> {
    let first = browser.and_then(|all| {
        all.split(if windows { ';' } else { ':' })
            .map(str::trim)
            .find(|one| !one.is_empty())
    });
    let words: Vec<&str> = match first {
        Some(one) if one.contains("%s") => one.split_whitespace().collect(),
        Some(one) => vec![one],
        None if windows => vec!["rundll32", "url.dll,FileProtocolHandler"],
        None if mac => vec!["open"],
        None => vec!["xdg-open"],
    };
    let mut words: Vec<String> = words.iter().map(|word| word.replace("%s", url)).collect();
    if !first.is_some_and(|one| one.contains("%s")) {
        words.push(url.to_owned());
    }
    words
}

/// Keep this test thread's tokens in `dir`.
#[cfg(test)]
pub(crate) fn keep_tokens_in(dir: &Path) {
    TOKENS.set(Some(dir.to_owned()));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::providers::auth::{base64url, same};
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;

    /// A request as the fakes below hear it.
    pub(crate) struct Request {
        pub method: String,
        pub target: String,
        pub headers: HashMap<String, String>,
        pub body: String,
    }

    /// What a fake answers: status, headers and body.
    pub(crate) type Answer = (u16, Vec<(&'static str, String)>, String);

    /// A server on 127.0.0.1 answering each request with what `answer` makes
    /// of it; its address, with no trailing slash.
    pub(crate) fn serve(answer: impl Fn(&Request) -> Answer + Send + Sync + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let answer = answer.clone();
                std::thread::spawn(move || {
                    let mut stream = stream.unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap();
                    let mut headers = HashMap::new();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        let Some((name, value)) = line.trim_end().split_once(':') else {
                            break;
                        };
                        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                    }
                    let length = headers
                        .get("content-length")
                        .map_or(0, |n| n.parse().unwrap());
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let mut words = first.split_whitespace();
                    let request = Request {
                        method: words.next().unwrap_or_default().to_owned(),
                        target: words.next().unwrap_or_default().to_owned(),
                        headers,
                        body: String::from_utf8(body).unwrap(),
                    };
                    let (status, headers, body) = answer(&request);
                    let mut head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
                    for (name, value) in headers {
                        head.push_str(&format!("{name}: {value}\r\n"));
                    }
                    let whole = format!("{head}Content-Length: {}\r\n\r\n{body}", body.len());
                    let _ = stream.write_all(whole.as_bytes());
                });
            }
        });
        base
    }

    /// What a fake authorization and MCP server keeps between requests.
    #[derive(Default)]
    pub(crate) struct Provider {
        pub base: OnceLock<String>,
        /// The PKCE challenge the sign-in page was given.
        pub challenge: Mutex<String>,
        /// The issuer its description gives, when not its own.
        pub issuer: Option<String>,
        /// The tokens its MCP endpoint takes.
        pub valid: Mutex<Vec<String>>,
        pub refreshes: AtomicUsize,
        pub registrations: AtomicUsize,
    }

    fn json_answer(status: u16, body: Value) -> Answer {
        (
            status,
            vec![("Content-Type", "application/json".into())],
            body.to_string(),
        )
    }

    /// The form in a request's body, decoded.
    fn form(body: &str) -> HashMap<String, String> {
        let url = Url::parse(&format!("http://x/?{body}")).unwrap();
        url.query_pairs().into_owned().collect()
    }

    /// A fake hosted MCP server with its own authorization server at one
    /// address: `/mcp` wants a token, the rest signs in. Its address.
    pub(crate) fn provider(issuer: Option<String>) -> (Arc<Provider>, String) {
        let fake = Arc::new(Provider {
            issuer,
            ..Provider::default()
        });
        let kept = fake.clone();
        let base = serve(move |request| answer(&kept, request));
        fake.base.set(base.clone()).unwrap();
        (fake, base)
    }

    fn answer(fake: &Provider, request: &Request) -> Answer {
        let base = fake.base.get().cloned().unwrap_or_default();
        match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/.well-known/oauth-protected-resource/mcp") => json_answer(
                200,
                json!({
                    "resource": format!("{base}/mcp"),
                    "authorization_servers": [base],
                    "scopes_supported": ["read"],
                }),
            ),
            ("GET", "/.well-known/oauth-authorization-server") => json_answer(
                200,
                json!({
                    "issuer": fake.issuer.clone().unwrap_or(base.clone()),
                    "authorization_endpoint": format!("{base}/authorize"),
                    "token_endpoint": format!("{base}/token"),
                    "registration_endpoint": format!("{base}/register"),
                    "code_challenge_methods_supported": ["S256"],
                }),
            ),
            ("POST", "/register") => {
                fake.registrations.fetch_add(1, Ordering::SeqCst);
                json_answer(201, json!({"client_id": "c-1"}))
            }
            ("POST", "/token") => token(fake, &form(&request.body), &base),
            ("POST", "/mcp") => mcp(fake, request, &base),
            _ => (404, Vec::new(), String::new()),
        }
    }

    /// The token endpoint: a code only with the verifier of the challenge the
    /// page was given, and a refresh taken slowly, so that two at once would
    /// overlap.
    fn token(fake: &Provider, form: &HashMap<String, String>, base: &str) -> Answer {
        let field = |key: &str| form.get(key).map(String::as_str).unwrap_or_default();
        let bound = field("resource") == format!("{base}/mcp") && field("client_id") == "c-1";
        let access = match field("grant_type") {
            "authorization_code"
                if bound
                    && field("code") == "code-1"
                    && pkce(field("code_verifier")) == *fake.challenge.lock().unwrap() =>
            {
                "a-1".to_owned()
            }
            "refresh_token" if bound && field("refresh_token") == "r-1" => {
                let count = fake.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
                std::thread::sleep(Duration::from_millis(300));
                format!("a-{}", count + 1)
            }
            _ => return json_answer(400, json!({"error": "invalid_grant"})),
        };
        fake.valid.lock().unwrap().push(access.clone());
        json_answer(
            200,
            json!({
                "access_token": access,
                "refresh_token": "r-1",
                "expires_in": 3600,
                "token_type": "Bearer",
                "scope": "read",
            }),
        )
    }

    /// The MCP endpoint: refused without a token it gave, and wanting more
    /// scope for a call that says `more`.
    fn mcp(fake: &Provider, request: &Request, base: &str) -> Answer {
        let sent = request
            .headers
            .get("authorization")
            .cloned()
            .unwrap_or_default();
        let good = sent
            .strip_prefix("Bearer ")
            .is_some_and(|token| fake.valid.lock().unwrap().iter().any(|t| t == token));
        if !good {
            let why = format!(
                "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""
            );
            return (401, vec![("WWW-Authenticate", why)], String::new());
        }
        let message: Value = serde_json::from_str(&request.body).unwrap_or_default();
        let id = message["id"].clone();
        let result = match message["method"].as_str().unwrap_or_default() {
            "initialize" => json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}}),
            "tools/list" => json!({"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]}),
            "tools/call" if message["params"]["arguments"]["text"] == "more" => {
                let why = "Bearer error=\"insufficient_scope\", scope=\"write\"".to_owned();
                return (403, vec![("WWW-Authenticate", why)], String::new());
            }
            "tools/call" => {
                let said = message["params"]["arguments"]["text"].clone();
                json!({"content": [{"type": "text", "text": said}]})
            }
            _ => return (202, Vec::new(), String::new()),
        };
        json_answer(200, json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-mcp-auth-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Knock on the sign-in's port as a browser coming back to `target`
    /// would, and read the page it is given.
    fn knock(sign_in: &SignIn, target: &str) -> std::thread::JoinHandle<String> {
        let port = Url::parse(&sign_in.redirect).unwrap().port().unwrap();
        let target = target.to_owned();
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut page = String::new();
            let _ = stream.read_to_string(&mut page);
            page
        })
    }

    /// The query of the sign-in page's address.
    fn asked(sign_in: &SignIn) -> HashMap<String, String> {
        Url::parse(&sign_in.url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }

    #[test]
    fn a_sign_in_finds_the_server_registers_and_saves_a_token_only_its_owner_reads() {
        let (fake, base) = provider(None);
        let dir = scratch("sign-in");
        let url = format!("{base}/mcp");
        let sign_in = start("oauth-a", &url, &OAuthConfig::default(), &dir, &|| false).unwrap();
        let page = Url::parse(&sign_in.url).unwrap();
        assert_eq!(page.path(), "/authorize");
        let asked = asked(&sign_in);
        assert_eq!(asked["response_type"], "code");
        assert_eq!(asked["client_id"], "c-1");
        assert_eq!(asked["code_challenge_method"], "S256");
        assert_eq!(asked["resource"], url);
        assert_eq!(asked["scope"], "read");
        assert_eq!(asked["redirect_uri"], sign_in.redirect);
        assert!(sign_in.redirect.starts_with("http://127.0.0.1:"));
        assert!(sign_in.redirect.ends_with("/callback"));
        assert_ne!(asked["code_challenge"], sign_in.verifier);
        *fake.challenge.lock().unwrap() = asked["code_challenge"].clone();

        let browser = knock(
            &sign_in,
            &format!("/callback?code=code-1&state={}", asked["state"]),
        );
        sign_in.finish().unwrap();
        let page = browser.join().unwrap();
        assert!(
            page.contains("text/plain") && page.contains("Signed in to oauth-a"),
            "{page}"
        );
        assert!(!page.contains("code-1"), "{page}");

        let file = token_file(&dir, &url);
        let token = load(&file, &url).unwrap();
        assert_eq!(token.access_token, "a-1");
        assert_eq!(token.refresh_token.as_deref(), Some("r-1"));
        assert_eq!(token.resource, url);
        assert!(token.expires_at.is_some_and(|at| at > now()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_authorization_server_that_names_another_issuer_is_refused() {
        let (_fake, base) = provider(Some("https://elsewhere.example".into()));
        let dir = scratch("issuer");
        let error = start(
            "oauth-b",
            &format!("{base}/mcp"),
            &OAuthConfig::default(),
            &dir,
            &|| false,
        )
        .err()
        .unwrap();
        assert!(error.contains("another issuer"), "{error}");
    }

    #[test]
    fn a_callback_with_the_wrong_state_or_path_is_refused_and_waiting_goes_on() {
        let (fake, base) = provider(None);
        let dir = scratch("state");
        let sign_in = start(
            "oauth-c",
            &format!("{base}/mcp"),
            &OAuthConfig::default(),
            &dir,
            &|| false,
        )
        .unwrap();
        let asked = asked(&sign_in);
        *fake.challenge.lock().unwrap() = asked["code_challenge"].clone();
        let sign_in = Arc::new(sign_in);
        let waiting = sign_in.clone();
        let finished = std::thread::spawn(move || waiting.finish());

        let forged = knock(&sign_in, "/callback?code=code-1&state=forged")
            .join()
            .unwrap();
        assert!(
            forged.contains("not the sign-in Letronna started"),
            "{forged}"
        );
        assert!(!forged.contains("code-1") && !forged.contains("forged"));
        let elsewhere = knock(&sign_in, "/favicon.ico").join().unwrap();
        assert!(elsewhere.starts_with("HTTP/1.1 404"), "{elsewhere}");
        assert!(!sign_in.answered());

        let good = format!("/callback?code=code-1&state={}", asked["state"]);
        assert!(knock(&sign_in, &good).join().unwrap().contains("Signed in"));
        finished.join().unwrap().unwrap();
    }

    #[test]
    fn a_pasted_address_finishes_the_sign_in_in_place_of_the_browser() {
        let (fake, base) = provider(None);
        let dir = scratch("paste");
        let sign_in = start(
            "oauth-d",
            &format!("{base}/mcp"),
            &OAuthConfig::default(),
            &dir,
            &|| false,
        )
        .unwrap();
        let asked = asked(&sign_in);
        *fake.challenge.lock().unwrap() = asked["code_challenge"].clone();
        let ended_on = "http://127.0.0.1:1/callback?state=x&code=code-1";
        assert!(sign_in.paste(ended_on).is_err());
        assert!(!sign_in.answered());
        let ended_on = format!("{}?code=code-1&state={}", sign_in.redirect, asked["state"]);
        sign_in.paste(&format!("  {ended_on}\n")).unwrap();
        assert!(sign_in.answered());
        sign_in.finish().unwrap();
        let url = format!("{base}/mcp");
        assert_eq!(
            load(&token_file(&dir, &url), &url).unwrap().access_token,
            "a-1"
        );
    }

    #[test]
    fn a_cancelled_sign_in_ends_at_once_and_closes_its_port() {
        let (_fake, base) = provider(None);
        let dir = scratch("cancel");
        let sign_in = start(
            "oauth-e",
            &format!("{base}/mcp"),
            &OAuthConfig::default(),
            &dir,
            &|| false,
        )
        .unwrap();
        sign_in.cancel();
        assert_eq!(sign_in.finish().unwrap_err(), "Sign-in cancelled.");
        let port = Url::parse(&sign_in.redirect).unwrap().port().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    /// A token for the fake at `base` saved in `dir`, as a sign-in leaves it.
    pub(crate) fn signed_in_to(base: &str, dir: &Path, expires_at: Option<u64>) -> PathBuf {
        let url = format!("{base}/mcp");
        let file = token_file(dir, &url);
        let token = Token {
            url: url.clone(),
            issuer: base.to_owned(),
            token_endpoint: format!("{base}/token"),
            resource: url,
            client_id: "c-1".into(),
            access_token: "a-1".into(),
            refresh_token: Some("r-1".into()),
            expires_at,
            scope: Some("read".into()),
            ..Token::default()
        };
        save(&file, &token).unwrap();
        file
    }

    #[test]
    fn requests_that_all_find_the_token_refused_refresh_it_once() {
        let (fake, base) = provider(None);
        let dir = scratch("single-flight");
        keep_tokens_in(&dir);
        signed_in_to(&base, &dir, None);
        let auth = Arc::new(Auth::new(&format!("{base}/mcp")).unwrap());
        assert_eq!(auth.bearer().as_deref(), Some("a-1"));
        let both: Vec<_> = (0..2)
            .map(|_| {
                let auth = auth.clone();
                std::thread::spawn(move || auth.renewed("a-1"))
            })
            .collect();
        let got: Vec<_> = both.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(got, [Ok("a-2".to_owned()), Ok("a-2".to_owned())]);
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
        // Another Auth, as the other frontend holds, reads the new one.
        let other = Auth::new(&format!("{base}/mcp")).unwrap();
        assert_eq!(other.bearer().as_deref(), Some("a-2"));
    }

    #[test]
    fn a_token_about_to_lapse_is_refreshed_before_it_is_sent() {
        let (fake, base) = provider(None);
        let dir = scratch("lapse");
        keep_tokens_in(&dir);
        signed_in_to(&base, &dir, Some(now() + EARLY / 2));
        let auth = Auth::new(&format!("{base}/mcp")).unwrap();
        assert_eq!(auth.bearer().as_deref(), Some("a-2"));
        assert_eq!(auth.bearer().as_deref(), Some("a-2"));
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refused_refresh_leaves_only_a_sign_in() {
        let (fake, base) = provider(None);
        let dir = scratch("refresh-fails");
        keep_tokens_in(&dir);
        let file = signed_in_to(&base, &dir, Some(1));
        let mut token: Token =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        token.refresh_token = Some("r-revoked".into());
        save(&file, &token).unwrap();
        let auth = Auth::new(&format!("{base}/mcp")).unwrap();
        // Lapsed, it is sent as it is for its 401 to refresh.
        assert_eq!(auth.bearer().as_deref(), Some("a-1"));
        assert_eq!(auth.renewed("a-1"), Err(Lapse::Gone));
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_refresh_refused_as_the_other_frontend_turned_it_over_takes_the_new_token() {
        let dir = scratch("rotated");
        keep_tokens_in(&dir);
        let file: Arc<OnceLock<PathBuf>> = Arc::default();
        let other = file.clone();
        // The other frontend refreshes first and saves what it got, so this
        // one's refresh token is spent by the time it arrives.
        let base = serve(move |_| {
            let file = other.get().unwrap();
            let mut token: Token =
                serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
            token.access_token = "a-9".into();
            token.refresh_token = Some("r-9".into());
            save(file, &token).unwrap();
            json_answer(400, json!({"error": "invalid_grant"}))
        });
        file.set(signed_in_to(&base, &dir, None)).unwrap();
        let auth = Auth::new(&format!("{base}/mcp")).unwrap();
        assert_eq!(auth.bearer().as_deref(), Some("a-1"));
        assert_eq!(auth.renewed("a-1"), Ok("a-9".to_owned()));
        assert_eq!(auth.bearer().as_deref(), Some("a-9"));
    }

    #[test]
    fn a_refresh_that_cannot_reach_its_server_keeps_the_token_and_tries_again_next_time() {
        let dir = scratch("transient");
        keep_tokens_in(&dir);
        let (down, tries) = (
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (is_down, tried) = (down.clone(), tries.clone());
        let base = serve(move |_| {
            tried.fetch_add(1, Ordering::SeqCst);
            // Slow, so that requests waiting on this refresh wait together.
            std::thread::sleep(Duration::from_millis(500));
            match is_down.load(Ordering::SeqCst) {
                true => json_answer(503, json!({})),
                false => json_answer(200, json!({"access_token": "a-2", "expires_in": 3600})),
            }
        });
        signed_in_to(&base, &dir, Some(now() + EARLY / 2));
        let auth = Arc::new(Auth::new(&format!("{base}/mcp")).unwrap());
        // Still good for a while, so sent as it is.
        assert_eq!(auth.bearer().as_deref(), Some("a-1"));
        assert_eq!(tries.load(Ordering::SeqCst), 1);
        // Requests that waited on one refresh take how it went.
        let start = Arc::new(std::sync::Barrier::new(3));
        let both: Vec<_> = (0..3)
            .map(|_| {
                let (auth, start) = (auth.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    auth.renewed("a-1")
                })
            })
            .collect();
        for got in both {
            assert!(matches!(got.join().unwrap(), Err(Lapse::Unreachable(_))));
        }
        assert_eq!(tries.load(Ordering::SeqCst), 2);
        // The next request tries again, and a server that turns no refresh
        // token over leaves the one it gave.
        down.store(false, Ordering::SeqCst);
        assert_eq!(auth.bearer().as_deref(), Some("a-2"));
        let url = format!("{base}/mcp");
        let saved = load(&token_file(&dir, &url), &url).unwrap();
        assert_eq!(saved.refresh_token.as_deref(), Some("r-1"));
    }

    #[test]
    fn a_step_up_asks_for_the_scope_wanted_along_with_what_was_granted() {
        let (_fake, base) = provider(None);
        let dir = scratch("step-up");
        signed_in_to(&base, &dir, None);
        let url = format!("{base}/mcp");
        hear(
            &url,
            challenge(r#"Bearer error="insufficient_scope", scope="write read""#),
        );
        let sign_in = start("oauth-f", &url, &OAuthConfig::default(), &dir, &|| false).unwrap();
        assert_eq!(asked(&sign_in)["scope"], "read write");
    }

    #[test]
    fn a_token_is_kept_under_its_servers_url_and_serves_no_other() {
        let dir = scratch("keyed");
        let (one, two) = ("https://a.example/mcp", "https://b.example/mcp");
        assert_ne!(token_file(&dir, one), token_file(&dir, two));
        let token = Token {
            url: one.into(),
            access_token: "a-1".into(),
            ..Token::default()
        };
        // A file copied to where another server's is kept is not its token.
        save(&token_file(&dir, two), &token).unwrap();
        assert_eq!(load(&token_file(&dir, two), two), None);
        save(&token_file(&dir, one), &token).unwrap();
        assert_eq!(load(&token_file(&dir, one), one), Some(token));
        let name = token_file(&dir, one);
        let name = name.file_name().unwrap().to_str().unwrap();
        assert!(name.len() == 37 && name.ends_with(".json"), "{name}");
    }

    #[test]
    fn the_sign_in_tool_starts_nothing_without_a_yes_on_its_card() {
        let tool = Authenticate {
            server: "far".into(),
            name: "mcp__far__authenticate",
            description: "",
        };
        assert!(tool.needs_approval() && !tool.mutates());
        let said = tool.run(&json!({}), &ToolContext::default()).unwrap();
        assert!(said.starts_with("Nothing was started"), "{said}");
        assert!(said.contains("/mcp login far"), "{said}");
        assert!(
            tool.preview(&json!({}), &ToolContext::default())
                .contains("far")
        );
    }

    #[test]
    fn a_challenge_is_read_quoted_or_not() {
        let said = challenge(
            r#"Bearer realm="mcp", resource_metadata="https://a.example/.well-known/oauth-protected-resource", scope="read write""#,
        );
        assert_eq!(
            said.resource_metadata.as_deref(),
            Some("https://a.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(said.scope.as_deref(), Some("read write"));
        let bare = challenge("Bearer error=insufficient_scope, scope=admin");
        assert_eq!(bare.error.as_deref(), Some("insufficient_scope"));
        assert_eq!(bare.scope.as_deref(), Some("admin"));
        assert_eq!(challenge("Basic"), Challenge::default());
    }

    #[test]
    fn addresses_are_placed_by_how_far_inside_a_network_they_are() {
        let at = |text: &str| place(text.parse().unwrap());
        for local in [
            "127.0.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "0.1.2.3",
            "2002:7f00:1::",
            "64:ff9b::a9fe:a9fe",
        ] {
            assert_eq!(at(local), Place::Local, "{local}");
        }
        for private in [
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "fd00::1",
            "fc00::1",
            "fec0::1",
            "64:ff9b::a00:1",
            "::ffff:10.0.0.1",
        ] {
            assert_eq!(at(private), Place::Private, "{private}");
        }
        for public in [
            "8.8.8.8",
            "100.128.0.1",
            "2001:4860::8888",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            assert_eq!(at(public), Place::Public, "{public}");
        }
    }

    #[test]
    fn a_server_is_reached_no_further_in_than_its_outermost_address() {
        let reach = |all: &[&str]| Reach::outermost(all.iter().map(|ip| ip.parse().unwrap())).0;
        // A second record, or a peer seen public, keeps every guard on.
        assert_eq!(reach(&["203.0.113.5", "127.0.0.1"]), Place::Public);
        assert_eq!(reach(&["127.0.0.1", "203.0.113.5"]), Place::Public);
        assert_eq!(reach(&["10.0.0.2", "127.0.0.1"]), Place::Private);
        assert_eq!(reach(&["127.0.0.1", "::1"]), Place::Local);
        assert_eq!(reach(&[]), Place::Public);
        // A name that resolves to this machine for a server that answered from
        // elsewhere is where it answered from.
        let url = Url::parse("http://127.0.0.1:9/mcp").unwrap();
        let peer = Some("203.0.113.5".parse().unwrap());
        assert_eq!(Reach::of(&url, peer).unwrap(), Reach(Place::Public));
        assert_eq!(Reach::of(&url, None).unwrap(), Reach(Place::Local));
    }

    #[test]
    fn a_sign_in_needs_https_unless_its_server_is_on_this_machine() {
        let public = Reach(Place::Public);
        let error = checked("http://auth.example.com/token", public).unwrap_err();
        assert!(error.contains("https"), "{error}");
        assert!(checked("https://auth.example.com/token", public).is_ok());
        assert!(checked("https://user:pw@auth.example.com/", public).is_err());
        assert!(checked("javascript:alert(1)", public).is_err());
        assert!(checked("http://127.0.0.1:9/token", Reach(Place::Local)).is_ok());
        assert!(checked("http://10.0.0.2/token", Reach(Place::Private)).is_err());
    }

    #[test]
    fn a_server_elsewhere_cannot_send_a_fetch_to_this_machine_or_its_network() {
        let get = |url: &str, place: Place| {
            fetch(&Url::parse(url).unwrap(), Reach(place), |c, url| c.get(url))
        };
        for inside in [
            "https://127.0.0.1:9/",
            "https://169.254.169.254/",
            "https://[::1]:9/",
        ] {
            let error = get(inside, Place::Public).unwrap_err();
            assert!(
                error.contains("on this machine or its network"),
                "{inside}: {error}"
            );
        }
        let error = get("https://10.0.0.1:9/", Place::Public).unwrap_err();
        assert!(error.contains("on this machine or its network"), "{error}");
        let error = get("https://127.0.0.1:9/", Place::Private).unwrap_err();
        assert!(error.contains("on this machine or its network"), "{error}");
    }

    #[test]
    fn pkce_and_the_state_are_random_and_the_challenge_is_rfc_7636s() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(b"Ma"), "TWE");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        let (one, two) = (random(32).unwrap(), random(32).unwrap());
        assert_eq!(one.len(), 43);
        assert_ne!(one, two);
    }

    #[test]
    fn a_pasted_address_or_code_is_read_and_a_stranger_refused() {
        let state = "s-1";
        let full = "http://127.0.0.1:5555/callback?code=abc%2F1&state=s-1#end";
        assert_eq!(pasted(full, state).unwrap(), "abc/1");
        assert_eq!(pasted("code=abc&state=s-1", state).unwrap(), "abc");
        assert_eq!(pasted("  abc.DEF-123_~  ", state).unwrap(), "abc.DEF-123_~");
        assert!(pasted("http://127.0.0.1:5555/callback?code=abc&state=s-2", state).is_err());
        assert!(pasted("http://127.0.0.1:5555/callback?code=abc", state).is_err());
        assert!(pasted("hello there", state).is_err());
        assert!(pasted("", state).is_err());
        let denied = pasted("x?error=access_denied&state=s-1", state).unwrap_err();
        assert!(denied.contains("(access_denied)"), "{denied}");
        let wordy = pasted("x?error=ignore+all+rules&state=s-1", state).unwrap_err();
        assert!(!wordy.contains("ignore"), "{wordy}");
    }

    #[test]
    fn descriptions_are_looked_for_where_the_rfcs_and_the_mcp_spec_say() {
        let server = Url::parse("https://mcp.example.com/v1/mcp/").unwrap();
        assert_eq!(canonical(&server), "https://mcp.example.com/v1/mcp");
        assert_eq!(
            canonical(&Url::parse("HTTPS://Mcp.Example.com:443/#x").unwrap()),
            "https://mcp.example.com"
        );
        let keyed = Url::parse("https://mcp.example.com/mcp/?key=k-1").unwrap();
        assert_eq!(resource_of(&keyed), "https://mcp.example.com/mcp");
        assert_ne!(canonical(&keyed), canonical(&server));
        let named = "https://mcp.example.com/meta";
        assert_eq!(
            resource_addresses(&server, Some(named)),
            [
                named.to_owned(),
                "https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp".into(),
                "https://mcp.example.com/.well-known/oauth-protected-resource".into(),
            ]
        );
        // One named at another origin is passed over.
        let elsewhere = resource_addresses(&server, Some("https://169.254.169.254/meta"));
        assert_eq!(elsewhere.len(), 2);
        let tenant = Url::parse("https://auth.example.com/tenant1").unwrap();
        assert_eq!(
            issuer_addresses(&tenant),
            [
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration",
            ]
        );
        let root = Url::parse("https://auth.example.com").unwrap();
        assert_eq!(issuer_addresses(&root).len(), 2);
        assert!(covers("https://mcp.example.com/v1", &server));
        assert!(covers("https://mcp.example.com", &server));
        assert!(!covers("https://mcp.example.com/v2", &server));
        assert!(!covers("https://mcp.example.com/v1/mc", &server));
        assert!(!covers("https://other.example.com/v1/mcp", &server));
        assert_eq!(joined(Some("a b"), Some("b c")).as_deref(), Some("a b c"));
        assert_eq!(joined(None, None), None);
    }

    #[test]
    fn a_token_is_saved_whole_and_a_garbled_file_keeps_the_last_one_read() {
        let (_fake, base) = provider(None);
        let dir = scratch("whole");
        keep_tokens_in(&dir);
        let file = signed_in_to(&base, &dir, None);
        signed_in_to(&base, &dir, None);
        // Moved into place: nothing is left beside it.
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let auth = Auth::new(&format!("{base}/mcp")).unwrap();
        assert_eq!(auth.bearer().as_deref(), Some("a-1"));
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&file, "{\"url\": ").unwrap();
        assert_eq!(auth.bearer().as_deref(), Some("a-1"));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(auth.bearer(), None);
    }

    #[test]
    fn a_browser_is_opened_as_browser_says_or_by_the_systems_opener() {
        let url = "https://auth.example.com/authorize?state=s";
        let line = |browser: Option<&str>, windows: bool, mac: bool| {
            opener(browser, url, windows, mac).join(" | ")
        };
        assert_eq!(line(None, false, true), format!("open | {url}"));
        assert_eq!(line(None, false, false), format!("xdg-open | {url}"));
        assert_eq!(
            line(None, true, false),
            format!("rundll32 | url.dll,FileProtocolHandler | {url}")
        );
        // Split as each system splits a list of paths.
        assert_eq!(
            line(Some(":firefox:chromium"), false, false),
            format!("firefox | {url}")
        );
        let windows = r"C:\Program Files\Browser\browser.exe;C:\other.exe";
        assert_eq!(
            line(Some(windows), true, false),
            format!(r"C:\Program Files\Browser\browser.exe | {url}")
        );
        // Its arguments, with the address where `%s` is.
        assert_eq!(
            line(Some("firefox --new-window %s"), false, false),
            format!("firefox | --new-window | {url}")
        );
        assert_eq!(line(Some("  "), false, true), format!("open | {url}"));
        assert!(!open_browser("file:///etc/passwd"));
        assert!(!open_browser("not an address"));
    }

    #[test]
    fn states_are_compared_whole() {
        assert!(same("s-1", "s-1"));
        assert!(!same("s-1", "s-2"));
        assert!(!same("s-1", "s-12"));
        assert!(!same("", "s"));
    }

    #[test]
    fn a_fixed_port_is_given_as_localhost_and_heard_on_either_loopback() {
        let (fake, base) = provider(None);
        let dir = scratch("fixed-port");
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let oauth = OAuthConfig {
            client_id: Some("c-1".into()),
            callback_port: Some(port),
            ..OAuthConfig::default()
        };
        let sign_in = start("oauth-g", &format!("{base}/mcp"), &oauth, &dir, &|| false).unwrap();
        assert_eq!(
            sign_in.redirect,
            format!("http://localhost:{port}/callback")
        );
        let asked = asked(&sign_in);
        assert_eq!(asked["redirect_uri"], sign_in.redirect);
        assert_eq!(fake.registrations.load(Ordering::SeqCst), 0);
        *fake.challenge.lock().unwrap() = asked["code_challenge"].clone();
        // Over IPv6 where this machine has it, as a browser may try it first.
        let host = match sign_in.listeners.lock().unwrap().len() {
            2 => "::1",
            _ => "127.0.0.1",
        };
        let target = format!("/callback?code=code-1&state={}", asked["state"]);
        let browser = std::thread::spawn(move || {
            let mut stream = TcpStream::connect((host, port)).unwrap();
            write!(stream, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut page = String::new();
            let _ = stream.read_to_string(&mut page);
            page
        });
        sign_in.finish().unwrap();
        assert!(browser.join().unwrap().contains("Signed in"));
    }

    #[test]
    fn an_app_registered_once_is_used_again_while_its_port_is_free() {
        let (fake, base) = provider(None);
        let dir = scratch("app-kept");
        let url = format!("{base}/mcp");
        let begun = || start("oauth-h", &url, &OAuthConfig::default(), &dir, &|| false).unwrap();
        let first = begun();
        let redirect = first.redirect.clone();
        drop(first);
        let again = begun();
        assert_eq!(again.redirect, redirect);
        assert_eq!(asked(&again)["client_id"], "c-1");
        assert_eq!(fake.registrations.load(Ordering::SeqCst), 1);
        // With its port taken, as by the sign-in still open, it registers anew.
        let other = begun();
        assert_ne!(other.redirect, redirect);
        assert_eq!(fake.registrations.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_secret_goes_in_a_basic_header_where_that_is_the_one_way_taken() {
        let seen = Arc::new(Mutex::new(None));
        let heard = seen.clone();
        let base = serve(move |request| {
            let header = request.headers.get("authorization").cloned();
            *heard.lock().unwrap() = Some((header, request.body.clone()));
            json_answer(200, json!({"access_token": "a-1"}))
        });
        let at = Url::parse(&format!("{base}/token")).unwrap();
        let form = [("grant_type", "refresh_token")];
        let app = Credentials {
            id: "c 1",
            secret: Some("s:1"),
            basic: true,
        };
        assert_eq!(
            grant(&at, Reach(Place::Local), &form, &app).unwrap().access,
            "a-1"
        );
        let (header, body) = seen.lock().unwrap().take().unwrap();
        assert_eq!(header.as_deref(), Some("Basic YysxOnMlM0Ex"));
        assert_eq!(body, "grant_type=refresh_token");
        let app = Credentials {
            basic: false,
            ..app
        };
        grant(&at, Reach(Place::Local), &form, &app).unwrap();
        let (header, body) = seen.lock().unwrap().take().unwrap();
        assert_eq!(header, None);
        assert_eq!(
            body,
            "grant_type=refresh_token&client_id=c+1&client_secret=s%3A1"
        );
    }

    #[test]
    fn a_sign_in_stopped_while_finding_its_way_goes_no_further() {
        let (fake, base) = provider(None);
        let dir = scratch("stopped");
        let url = format!("{base}/mcp");
        let error = start("oauth-i", &url, &OAuthConfig::default(), &dir, &|| true)
            .err()
            .unwrap();
        assert_eq!(error, CANCELLED);
        assert_eq!(fake.registrations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_sign_in_that_fails_is_told_on_the_next_call_and_its_app_forgotten() {
        let (_fake, base) = provider(None);
        let dir = scratch("failed");
        let url = format!("{base}/mcp");
        let sign_in = start("oauth-j", &url, &OAuthConfig::default(), &dir, &|| false).unwrap();
        let app = token_file(&dir, &format!("app {base}"));
        assert!(app.exists());
        // A code the token endpoint does not take.
        let state = asked(&sign_in)["state"].clone();
        sign_in
            .paste(&format!("{}?code=code-x&state={state}", sign_in.redirect))
            .unwrap();
        let error = sign_in.finish().unwrap_err();
        assert!(error.contains("invalid_grant"), "{error}");
        assert!(!app.exists());
        let tool = Authenticate {
            server: "oauth-j".into(),
            name: "mcp__oauth-j__authenticate",
            description: "",
        };
        let said = tool.run(&json!({}), &ToolContext::default()).unwrap();
        assert!(
            said.contains("The last sign-in to oauth-j failed:"),
            "{said}"
        );
        assert!(
            said.contains("invalid_grant") && !said.contains(&state),
            "{said}"
        );
    }
}
