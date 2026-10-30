//! Browser sign-in for providers that take a plan or an account rather than
//! a key: OAuth's authorization code grant, with PKCE, a loopback callback
//! and a paste for a browser that cannot reach this machine.
//!
//! A provider describes its sign-in as data, an [`OAuthSpec`]; everything
//! else is here. The tokens are kept in the vault, as JSON under
//! `provider:<name>`, never in a file and never in a log, and the access
//! token is refreshed shortly before it ends, or once when it is refused,
//! one refresh at a time. The helpers at the bottom are shared with MCP's
//! sign-in (`mcp_auth.rs`).

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How a provider is signed in to, as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthSpec {
    pub authorize: &'static str,
    pub token: &'static str,
    pub client_id: &'static str,
    pub client_secret: Option<&'static str>,
    /// Space-separated, as the authorize page takes them.
    pub scopes: &'static str,
    /// The loopback port the browser comes back to, 0 for any free one.
    pub port: u16,
    /// Where on that port, as `/callback`.
    pub path: &'static str,
    /// More query parameters the authorize page wants.
    pub extra: &'static [(&'static str, &'static str)],
    /// The token endpoint takes JSON rather than a form.
    pub json: bool,
    pub pkce: bool,
    /// The state goes to the token endpoint too, as some providers want.
    pub state_in_token: bool,
    /// JSON pointers to the account id and the plan in the id token's
    /// claims, or else the access token's.
    pub account_claim: Option<&'static str>,
    pub plan_claim: Option<&'static str>,
    /// A sign-in finished by typing a code on another device, for a
    /// provider that offers one.
    pub device: Option<&'static DeviceSpec>,
    /// A page the provider shows the code on, to be pasted back, in place of
    /// the loopback: for a sign-in finished on a device that cannot reach
    /// this machine, such as a phone.
    pub manual_redirect: Option<&'static str>,
}

/// A sign-in by a code the user types on any device, as a provider that
/// offers one describes it.
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceSpec {
    /// Where a code for the user to type is asked for.
    pub usercode: &'static str,
    /// Where Letronna asks whether the code was typed yet.
    pub token: &'static str,
    /// The page the code is typed on.
    pub verify: &'static str,
    /// The address the code that comes back is traded under.
    pub redirect: &'static str,
}

/// A provider's sign-in: its tokens, and whose they are.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub access: String,
    #[serde(default)]
    pub refresh: String,
    /// Unix seconds; 0 when the provider did not say.
    #[serde(default)]
    pub expires_at: u64,
    #[serde(default)]
    pub id_token: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub plan: String,
    /// When it was last refreshed, so a token refused right after a refresh
    /// is not refreshed again and again.
    #[serde(default)]
    pub refreshed_at: u64,
}

// Never prints a token, so a sign-in in a log or a panic gives nothing away.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("account", &self.account)
            .field("plan", &self.plan)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// How long before its end an access token is refreshed, in seconds.
const REFRESH_AHEAD: u64 = 5 * 60;
/// A token refused within this long of a refresh is not refreshed again.
const REFRESH_GAP: u64 = 60;
/// How long the browser has to come back.
const PATIENCE: Duration = Duration::from_secs(10 * 60);
const CALL_LIMIT: Duration = Duration::from_secs(60);
const CANCELLED: &str = "Sign-in cancelled.";

fn slot(name: &str) -> String {
    format!("provider:{name}")
}

fn spec_of(name: &str) -> Result<&'static OAuthSpec, String> {
    match super::find(name).map(|p| p.auth()) {
        Some(super::AuthKind::OAuth(spec)) => Ok(spec),
        _ => Err(format!("{name} is not signed in to with a browser.")),
    }
}

/// The sign-in kept for provider `name`: ChatGPT's may be the Codex app's,
/// read as it is now and never refreshed, or Letronna's own when Codex has
/// none.
pub fn tokens(name: &str) -> Option<Tokens> {
    if name == super::chatgpt::NAME
        && super::chatgpt::shares_codex()
        && let Some(shared) = super::chatgpt::codex_tokens()
    {
        return Some(shared);
    }
    let kept = crate::vault::get(&slot(name)).ok()??;
    serde_json::from_str(&kept).ok()
}

pub fn signed_in(name: &str) -> bool {
    tokens(name).is_some_and(|t| !t.access.is_empty())
}

/// Forget provider `name`'s sign-in.
pub fn sign_out(name: &str) -> Result<(), String> {
    crate::vault::remove(&slot(name))
}

fn store(name: &str, tokens: &Tokens) -> Result<(), String> {
    let json = serde_json::to_string(tokens).map_err(|e| e.to_string())?;
    let in_file = crate::vault::save(&slot(name), &json, "")
        .map_err(|why| format!("Could not keep the {name} sign-in: {why}."))?;
    // With no vault `save` hands the secret back for a file to keep, which
    // a sign-in never is.
    if !in_file.is_empty() {
        return Err(format!(
            "There is no system vault to keep the {name} sign-in in, and it is never kept in a file."
        ));
    }
    Ok(())
}

/// Held while tokens are refreshed: a refresh token is often spent by its
/// use, so two refreshing at once would end the sign-in.
fn refreshing() -> MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn held(name: &str) -> Result<Tokens, String> {
    tokens(name).ok_or_else(|| format!("{name} is signed out. Sign in again."))
}

/// An access token for `name` good for a while yet, refreshed first when it
/// is about to end.
pub fn access(name: &str) -> Result<String, String> {
    let kept = held(name)?;
    if !ending(&kept, now()) {
        return Ok(kept.access);
    }
    let _one = refreshing();
    // Another request may have refreshed it while this one waited.
    let kept = held(name)?;
    if !ending(&kept, now()) {
        return Ok(kept.access);
    }
    renew(name, spec_of(name)?, kept).map(|t| t.access)
}

/// A new access token for `name` after `stale` was refused, unless one has
/// come since or it was refreshed a moment ago.
pub fn refused(name: &str, stale: &str) -> Result<String, String> {
    let _one = refreshing();
    let kept = held(name)?;
    if kept.access != stale {
        return Ok(kept.access);
    }
    if now() < kept.refreshed_at + REFRESH_GAP {
        return Err(format!("{name} refused a sign-in just refreshed."));
    }
    renew(name, spec_of(name)?, kept).map(|t| t.access)
}

fn ending(tokens: &Tokens, now: u64) -> bool {
    tokens.expires_at != 0 && tokens.expires_at <= now + REFRESH_AHEAD
}

/// Refresh `kept` and keep what comes back; a refresh the provider refuses
/// ends the sign-in.
fn renew(name: &str, spec: &OAuthSpec, kept: Tokens) -> Result<Tokens, String> {
    let ended = || {
        let _ = sign_out(name);
        format!("{name} ended the sign-in. Sign in again.")
    };
    if kept.refresh.is_empty() {
        return Err(ended());
    }
    let asked = token_call(
        spec,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &kept.refresh),
        ],
    );
    let reply = match asked {
        Ok(reply) => reply,
        Err((true, _)) => return Err(ended()),
        Err((false, why)) => return Err(format!("Could not refresh the {name} sign-in: {why}")),
    };
    let mut fresh = from_reply(spec, &reply, Some(&kept))?;
    fresh.refreshed_at = now();
    store(name, &fresh)?;
    Ok(fresh)
}

/// The tokens a token endpoint's `reply` stands for, with `before`'s where
/// it leaves one out, as a refresh may.
pub(crate) fn from_reply(
    spec: &OAuthSpec,
    reply: &Value,
    before: Option<&Tokens>,
) -> Result<Tokens, String> {
    let text = |key: &str| {
        reply[key]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    let kept = |pick: fn(&Tokens) -> &String| before.map(|t| pick(t).clone()).unwrap_or_default();
    let access = text("access_token").ok_or("No access token came back.")?;
    let refresh = text("refresh_token").unwrap_or_else(|| kept(|t| &t.refresh));
    let id_token = text("id_token").unwrap_or_else(|| kept(|t| &t.id_token));
    let said = [claims(&id_token), claims(&access)];
    let claim = |pointer: Option<&str>| {
        let pointer = pointer?;
        said.iter().flatten().find_map(|c| {
            c.pointer(pointer)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
    };
    let expires_at = reply["expires_in"]
        .as_u64()
        .map(|secs| now().saturating_add(secs))
        .or_else(|| claims(&access).and_then(|c| c["exp"].as_u64()))
        .unwrap_or_default();
    Ok(Tokens {
        account: claim(spec.account_claim).unwrap_or_else(|| kept(|t| &t.account)),
        plan: claim(spec.plan_claim).unwrap_or_else(|| kept(|t| &t.plan)),
        email: claim(Some("/email")).unwrap_or_else(|| kept(|t| &t.email)),
        refreshed_at: before.map_or(0, |t| t.refreshed_at),
        access,
        refresh,
        id_token,
        expires_at,
    })
}

/// The claims a token carries, when it is a JWT. Nothing checks its
/// signature: it only says whose the sign-in is.
pub(crate) fn claims(token: &str) -> Option<Value> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// POST `params` and the client's id and secret to the spec's token
/// endpoint, as JSON or a form: the reply, or whether it refused and why.
fn token_call(spec: &OAuthSpec, params: &[(&str, &str)]) -> Result<Value, (bool, String)> {
    let mut params = params.to_vec();
    params.push(("client_id", spec.client_id));
    params.extend(spec.client_secret.map(|s| ("client_secret", s)));
    let client = crate::chat::client().map_err(|e| (false, e))?;
    let request = client.post(spec.token).timeout(CALL_LIMIT);
    let request = if spec.json {
        let body: serde_json::Map<String, Value> = params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
            .collect();
        request.json(&body)
    } else {
        request.form(&params)
    };
    let response = request
        .send()
        .map_err(|e| (false, format!("cannot reach the sign-in server: {e}.")))?;
    let status = response.status().as_u16();
    let reply: Value = response.json().unwrap_or_default();
    if status == 200 && reply["access_token"].is_string() {
        return Ok(reply);
    }
    // An error code is a word; anything else is left out of what is shown.
    let error = reply["error"].as_str().unwrap_or_default();
    let plain = !error.is_empty() && error.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let why = if plain {
        format!(", {error}")
    } else {
        String::new()
    };
    Err((
        matches!(status, 400 | 401 | 403),
        format!("no token was given (HTTP {status}{why})."),
    ))
}

/// A sign-in waiting for the browser, or a paste, to bring the code back.
pub struct SignIn {
    pub name: &'static str,
    /// The page to open in the browser.
    pub url: String,
    spec: &'static OAuthSpec,
    redirect: String,
    state: String,
    verifier: String,
    listeners: Mutex<Vec<TcpListener>>,
    pasted: Mutex<Option<String>>,
    stop: AtomicBool,
    /// Finished only by a paste, as one finished on another device is:
    /// nothing comes back to this machine.
    pasted_only: bool,
}

/// Start signing in to provider `name`: listen for the browser and work out
/// the page to send it to. `open_browser` sends it there; `finish` waits.
pub fn begin(name: &str) -> Result<Arc<SignIn>, String> {
    let spec = spec_of(name)?;
    // Kept under the account's own name: a provider's further account has
    // a sign-in apart from its first.
    let name = (crate::gateway::gateways().into_iter())
        .map(|g| g.name)
        .find(|g| *g == name)
        .or_else(|| super::find(name).map(|p| p.info().name))
        .unwrap_or_default();
    start(name, spec)
}

/// A sign-in started here and finished on another device: a phone, or the
/// browser of a computer this terminal is reached from over SSH.
pub struct Away {
    pub name: &'static str,
    /// The page to open, on any device.
    pub url: String,
    /// The code to type on that page, for a provider that signs in by one;
    /// `None` for one whose page shows a code to paste back instead.
    pub code: Option<String>,
    how: AwayHow,
}

enum AwayHow {
    Device {
        spec: &'static OAuthSpec,
        device: &'static DeviceSpec,
        id: String,
        every: Duration,
        stop: AtomicBool,
    },
    Paste(Arc<SignIn>),
}

/// How long a code typed on another device may take.
const AWAY_PATIENCE: Duration = Duration::from_secs(15 * 60);

/// Start signing in to `name` on another device: a code to type there, or a
/// page that shows one to paste back. `finish` waits for it.
pub fn begin_away(name: &str) -> Result<Arc<Away>, String> {
    let spec = spec_of(name)?;
    let name = (crate::gateway::gateways().into_iter())
        .map(|g| g.name)
        .find(|g| *g == name)
        .or_else(|| super::find(name).map(|p| p.info().name))
        .unwrap_or_default();
    away_with(name, spec)
}

fn away_with(name: &'static str, spec: &'static OAuthSpec) -> Result<Arc<Away>, String> {
    if let Some(device) = spec.device {
        let client = crate::chat::client()?;
        let reply: Value = client
            .post(device.usercode)
            .timeout(CALL_LIMIT)
            .json(&serde_json::json!({"client_id": spec.client_id}))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| format!("{name} would not give a code to sign in with."))?
            .json()
            .map_err(|_| format!("{name} gave a code Letronna cannot read."))?;
        let field = |key: &str| {
            reply[key]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let (Some(id), Some(code)) = (field("device_auth_id"), field("user_code")) else {
            return Err(format!("{name} gave no code to sign in with."));
        };
        let every = (field("interval").and_then(|i| i.parse().ok()))
            .or(reply["interval"].as_u64())
            .unwrap_or(5)
            .max(1);
        return Ok(Arc::new(Away {
            name,
            url: device.verify.to_owned(),
            code: Some(code),
            how: AwayHow::Device {
                spec,
                device,
                id,
                every: Duration::from_secs(every),
                stop: AtomicBool::new(false),
            },
        }));
    }
    let Some(redirect) = spec.manual_redirect else {
        return Err(format!(
            "{name} can only be signed in to in a browser on this computer."
        ));
    };
    let state = random(32)?;
    let verifier = random(64)?;
    let url = authorize_url(spec, redirect, &state, &verifier);
    let flow = Arc::new(SignIn {
        name,
        url: url.clone(),
        spec,
        redirect: redirect.to_owned(),
        state,
        verifier,
        // Nothing comes back here: the code is pasted.
        listeners: Mutex::new(Vec::new()),
        pasted: Mutex::new(None),
        stop: AtomicBool::new(false),
        pasted_only: true,
    });
    Ok(Arc::new(Away {
        name,
        url,
        code: None,
        how: AwayHow::Paste(flow),
    }))
}

impl Away {
    /// Take the code the provider's page showed, for a sign-in finished by
    /// pasting one back.
    pub fn paste(&self, text: &str) -> Result<(), String> {
        match &self.how {
            AwayHow::Paste(flow) => flow.paste(text),
            AwayHow::Device { .. } => Err("Type the code on the page instead.".into()),
        }
    }

    pub fn cancel(&self) {
        match &self.how {
            AwayHow::Paste(flow) => flow.cancel(),
            AwayHow::Device { stop, .. } => stop.store(true, Ordering::Relaxed),
        }
    }

    /// Wait for the code to be typed or pasted, trade it for tokens and keep
    /// them under the account's name.
    pub fn finish(&self) -> Result<Tokens, String> {
        let AwayHow::Device {
            spec,
            device,
            id,
            every,
            stop,
        } = &self.how
        else {
            let AwayHow::Paste(flow) = &self.how else {
                unreachable!()
            };
            return flow.finish();
        };
        let client = crate::chat::client()?;
        let deadline = Instant::now() + AWAY_PATIENCE;
        let code = self.code.clone().unwrap_or_default();
        let granted = loop {
            if stop.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            if Instant::now() >= deadline {
                return Err("The code was not typed in 15 minutes; start again.".into());
            }
            let reply = client
                .post(device.token)
                .timeout(CALL_LIMIT)
                .json(&serde_json::json!({"device_auth_id": id, "user_code": code}))
                .send()
                .map_err(|e| format!("Cannot reach {}'s sign-in: {e}.", self.name))?;
            match reply.status().as_u16() {
                200 => break reply.json::<Value>().unwrap_or_default(),
                // Not typed yet.
                403 | 404 => {
                    // Slept in steps, so a cancel is heard at once.
                    let until = Instant::now() + *every;
                    while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                status => {
                    return Err(format!(
                        "{} turned the code down (HTTP {status}); start again.",
                        self.name
                    ));
                }
            }
        };
        let field = |key: &str| granted[key].as_str().unwrap_or_default().to_owned();
        let (code, verifier) = (field("authorization_code"), field("code_verifier"));
        if code.is_empty() {
            return Err(format!("{} said yes but gave no code.", self.name));
        }
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", device.redirect),
        ];
        if !verifier.is_empty() {
            params.push(("code_verifier", &verifier));
        }
        let reply = token_call(spec, &params).map_err(|(_, why)| why)?;
        let tokens = from_reply(spec, &reply, None)?;
        store(self.name, &tokens)?;
        Ok(tokens)
    }
}

fn start(name: &'static str, spec: &'static OAuthSpec) -> Result<Arc<SignIn>, String> {
    let (listeners, port) = listen(spec.port, true)?;
    let redirect = format!("http://localhost:{port}{}", spec.path);
    let state = random(32)?;
    let verifier = random(64)?;
    let url = authorize_url(spec, &redirect, &state, &verifier);
    Ok(Arc::new(SignIn {
        name,
        url,
        spec,
        redirect,
        state,
        verifier,
        listeners: Mutex::new(listeners),
        pasted: Mutex::new(None),
        stop: AtomicBool::new(false),
        pasted_only: false,
    }))
}

/// Sign in to provider `name` start to end: open the browser and wait. A
/// frontend that offers a paste calls `begin` and `finish` itself.
pub fn sign_in(name: &str) -> Result<Tokens, String> {
    let flow = begin(name)?;
    flow.open_browser();
    flow.finish()
}

fn authorize_url(spec: &OAuthSpec, redirect: &str, state: &str, verifier: &str) -> String {
    let challenge = pkce(verifier);
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", spec.client_id),
        ("redirect_uri", redirect),
        ("scope", spec.scopes),
        ("state", state),
    ];
    if spec.pkce {
        params.push(("code_challenge", &challenge));
        params.push(("code_challenge_method", "S256"));
    }
    params.extend(spec.extra.iter().copied());
    Url::parse_with_params(spec.authorize, &params)
        .map(String::from)
        .unwrap_or_default()
}

impl SignIn {
    /// Open the sign-in page in the user's browser; false when none started,
    /// and `url` is shown for the user to open instead.
    pub fn open_browser(&self) -> bool {
        crate::mcp_auth::open_browser(&self.url)
    }

    /// Take what the user pasted: the address the browser ended on, the code
    /// alone, or `code#state` as some providers show it.
    pub fn paste(&self, text: &str) -> Result<(), String> {
        let code = pasted_code(text, &self.state)?;
        if let Ok(mut pasted) = self.pasted.lock() {
            *pasted = Some(code);
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Wait for the code, trade it for tokens and keep them.
    pub fn finish(&self) -> Result<Tokens, String> {
        let code = self.wait()?;
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", self.redirect.as_str()),
        ];
        if self.spec.pkce {
            params.push(("code_verifier", &self.verifier));
        }
        if self.spec.state_in_token {
            params.push(("state", &self.state));
        }
        let reply = token_call(self.spec, &params).map_err(|(_, why)| why)?;
        let tokens = from_reply(self.spec, &reply, None)?;
        store(self.name, &tokens)?;
        Ok(tokens)
    }

    fn wait(&self) -> Result<String, String> {
        let listeners = self
            .listeners
            .lock()
            .map(|mut held| std::mem::take(&mut *held))
            .unwrap_or_default();
        if listeners.is_empty() && !self.pasted_only {
            return Err("This sign-in has already ended; start it again.".into());
        }
        // Blocking in `accept` would never look at the time, the switch or
        // a paste, and there may be two ports.
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
            let Some((stream, _)) = listeners.iter().find_map(|l| l.accept().ok()) else {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            if let Some(came) = self.callback(stream) {
                return came;
            }
        }
        Err("The sign-in was not finished in 10 minutes; start it again.".into())
    }

    /// Read one request and answer it: None for a knock that was not this
    /// sign-in coming back, a page's icon say, and the wait goes on.
    fn callback(&self, mut stream: TcpStream) -> Option<Result<String, String>> {
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
        let mut request = BufReader::new((&stream).take(8 * 1024));
        let mut line = String::new();
        request.read_line(&mut line).ok()?;
        // Read to the end of the head: a socket closed with some unread is
        // reset, and the browser shows that, not the page.
        let mut header = String::new();
        while request.read_line(&mut header).is_ok_and(|n| n > 2) {
            header.clear();
        }
        let target = line.split_whitespace().nth(1)?;
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != self.spec.path {
            reply(&mut stream, "404 Not Found", "Nothing is here.");
            return None;
        }
        let came = returned(query, &self.state);
        let words = match &came {
            Some(Ok(_)) => format!(
                "Signed in to {}. You can close this tab and go back to Letronna.",
                self.name
            ),
            Some(Err(_)) => "The sign-in was refused. Go back to Letronna to try again.".into(),
            None => "That did not work: this is not the sign-in Letronna started.".into(),
        };
        reply(&mut stream, "200 OK", &words);
        came
    }
}

/// `pasted`, taking `code#state` as well.
fn pasted_code(text: &str, state: &str) -> Result<String, String> {
    let text = text.trim();
    if !text.contains('?')
        && !text.contains("code=")
        && let Some((code, came)) = text.split_once('#')
    {
        if !same(came, state) {
            return Err("That code is not from the sign-in just started.".into());
        }
        return pasted(code, state);
    }
    pasted(text, state)
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

// The pieces of an OAuth sign-in MCP's shares.

/// `count` bytes from the system's secure generator, as base64url.
pub(crate) fn random(count: usize) -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut bytes = vec![0u8; count];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "This computer would not give a random number for the sign-in.".to_owned())?;
    Ok(base64url(&bytes))
}

/// The PKCE challenge for `verifier`: its SHA-256, as base64url (S256).
pub(crate) fn pkce(verifier: &str) -> String {
    base64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

/// `bytes` in base64url without padding, as PKCE and states are written.
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(LETTERS[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// Listen for the browser on 127.0.0.1 at `port`, 0 for any free one, and
/// on [::1] as well when `both`, as `localhost` may be either; the port.
pub(crate) fn listen(port: u16, both: bool) -> Result<(Vec<TcpListener>, u16), String> {
    let first = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("Cannot listen for the sign-in on 127.0.0.1:{port}: {e}"))?;
    let port = first.local_addr().map_err(|e| e.to_string())?.port();
    let mut all = vec![first];
    // A machine without IPv6 still has 127.0.0.1 to come back to.
    if both && let Ok(second) = TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        all.push(second);
    }
    Ok((all, port))
}

/// Answer the browser with plain text, which runs nothing and fetches
/// nothing.
pub(crate) fn reply(stream: &mut TcpStream, status: &str, words: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{words}",
        words.len()
    );
    let _ = stream.flush();
}

/// What a query the sign-in came back with brought: the code, why it was
/// refused, or None when its state is not `state`, as a page other than the
/// one sent would give.
pub(crate) fn returned(query: &str, state: &str) -> Option<Result<String, String>> {
    let url = Url::parse(&format!("http://127.0.0.1/?{query}")).ok()?;
    let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if !pairs.get("state").is_some_and(|came| same(came, state)) {
        return None;
    }
    if let Some(error) = pairs.get("error") {
        // An error code is a word; anything else could be words for the
        // model, so it is left out.
        let plain = error.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        let why = if plain {
            format!(" ({error})")
        } else {
            String::new()
        };
        return Some(Err(format!("The sign-in was refused{why}.")));
    }
    let code = pairs.get("code").filter(|code| !code.is_empty())?;
    Some(Ok(code.clone()))
}

/// Whether `a` and `b` are the same, taking as long wherever they differ,
/// so the time a knock takes says nothing of the state.
pub(crate) fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |diff, (x, y)| diff | (x ^ y))
            == 0
}

/// The code in what the user pasted: the whole address the browser ended
/// on, its query, or the code alone.
pub(crate) fn pasted(text: &str, state: &str) -> Result<String, String> {
    let text = text.trim();
    let query = match text.split_once('?') {
        Some((_, query)) => Some(query),
        None if text.contains("code=") => Some(text),
        None => None,
    };
    if let Some(query) = query {
        let query = query.split('#').next().unwrap_or_default();
        return returned(query, state).unwrap_or_else(|| {
            Err("That address is not from the sign-in just started, or has no code on it.".into())
        });
    }
    let code_like = !text.is_empty()
        && text.len() <= 2048
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~+/=".contains(c));
    match code_like {
        true => Ok(text.to_owned()),
        false => Err("Paste the whole address the browser ended on, or the code on it.".into()),
    }
}

/// `text` form-encoded, as the id and secret in a Basic header are (RFC 6749
/// 2.3.1).
pub(crate) fn form_encoded(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                char::from(b).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// A data folder of this test's own, held from other tests, with an
    /// empty vault on this thread; the vault, and what to keep alive.
    pub(crate) fn vault(
        name: &str,
    ) -> (
        Arc<crate::vault::tests::Fake>,
        (MutexGuard<'static, ()>, crate::store::tests::Scratch),
    ) {
        let held = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let dir = crate::store::tests::Scratch::new(name);
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        (crate::vault::tests::fake(), (held, dir))
    }

    /// Keep a sign-in for `name` with `access`, as a finished one is.
    pub(crate) fn keep(name: &str, access: &str) {
        let tokens = Tokens {
            access: access.into(),
            refresh: "refresh-1".into(),
            ..Default::default()
        };
        store(name, &tokens).unwrap();
    }

    /// A JWT with `claims`, unsigned, as only its claims are read.
    fn jwt(claims: Value) -> String {
        let part = |value: &Value| base64url(value.to_string().as_bytes());
        format!("{}.{}.sig", part(&json!({"alg": "none"})), part(&claims))
    }

    /// A token endpoint on this machine that answers each POST with the
    /// next of `replies`, and hands back the bodies it was sent.
    fn token_server(
        replies: Vec<(u16, Value)>,
    ) -> (&'static str, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let (sent, bodies) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, reply) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let _ = sent.send(String::from_utf8(body).unwrap());
                let reply = reply.to_string();
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                reader.get_mut().write_all(head.as_bytes()).unwrap();
            }
        });
        (Box::leak(url.into_boxed_str()), bodies)
    }

    fn spec(token: &'static str) -> &'static OAuthSpec {
        Box::leak(Box::new(OAuthSpec {
            authorize: "https://auth.example.com/authorize",
            token,
            client_id: "client-1",
            client_secret: None,
            scopes: "openid offline_access",
            port: 0,
            path: "/cb",
            extra: &[("prompt", "login")],
            json: false,
            pkce: true,
            state_in_token: false,
            account_claim: Some("/auth/account"),
            plan_claim: Some("/auth/plan"),
            device: None,
            manual_redirect: Some("https://auth.example.com/code"),
        }))
    }

    #[test]
    fn a_code_typed_on_another_device_signs_in_once_it_is_typed() {
        let (vault, _held) = vault("provider-sign-in-away");
        let (token, bodies) = token_server(vec![
            (
                200,
                json!({"device_auth_id": "dev-1", "user_code": "K392-Z5EW5", "interval": "1"}),
            ),
            (403, json!({"error": "pending"})),
            (
                200,
                json!({"authorization_code": "auth-1", "code_verifier": "ver-1"}),
            ),
            (
                200,
                json!({"access_token": "access-away", "refresh_token": "refresh-away"}),
            ),
        ]);
        let device: &'static DeviceSpec = Box::leak(Box::new(DeviceSpec {
            usercode: token,
            token,
            verify: "https://auth.example.com/device",
            redirect: "https://auth.example.com/device/callback",
        }));
        let spec: &'static OAuthSpec = Box::leak(Box::new(OAuthSpec {
            device: Some(device),
            json: true,
            ..spec(token).clone()
        }));
        let flow = away("Away", spec).unwrap();
        assert_eq!(flow.code.as_deref(), Some("K392-Z5EW5"));
        assert_eq!(flow.url, "https://auth.example.com/device");
        assert!(flow.paste("anything").is_err());
        let tokens = flow.finish().unwrap();
        assert_eq!(tokens.access, "access-away");
        let sent: Vec<String> = bodies.try_iter().collect();
        assert!(sent[0].contains("client-1"));
        assert!(sent[1].contains("dev-1") && sent[1].contains("K392-Z5EW5"));
        assert!(sent[3].contains("auth-1") && sent[3].contains("ver-1"));
        assert!(sent[3].contains("device/callback"));
        assert!(vault.holds("provider:Away").is_some());
    }

    #[test]
    fn a_page_that_shows_a_code_is_finished_by_pasting_it() {
        let (_vault, _held) = vault("provider-sign-in-paste");
        let (token, bodies) = token_server(vec![(
            200,
            json!({"access_token": "access-pasted", "refresh_token": "refresh-pasted"}),
        )]);
        let flow = away("Pasted", spec(token)).unwrap();
        assert!(flow.code.is_none());
        let url = Url::parse(&flow.url).unwrap();
        let asked: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(asked["redirect_uri"], "https://auth.example.com/code");
        flow.paste(&format!("c9#{}", asked["state"])).unwrap();
        assert_eq!(flow.finish().unwrap().access, "access-pasted");
        assert!(
            bodies
                .recv()
                .unwrap()
                .contains("redirect_uri=https%3A%2F%2Fauth.example.com%2Fcode")
        );
    }

    /// `begin_away` for a spec of the test's own, as `start` is for `begin`.
    fn away(name: &'static str, spec: &'static OAuthSpec) -> Result<Arc<Away>, String> {
        super::away_with(name, spec)
    }

    fn knock(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut said = String::new();
        let _ = stream.read_to_string(&mut said);
        said
    }

    #[test]
    fn a_sign_in_comes_back_to_its_own_state_and_keeps_its_tokens_in_the_vault() {
        let (vault, _held) = vault("provider-sign-in");
        let id =
            jwt(json!({"email": "a@example.com", "auth": {"account": "acct-1", "plan": "plus"}}));
        let (token, bodies) = token_server(vec![(
            200,
            json!({"access_token": "access-1", "refresh_token": "refresh-1", "id_token": id, "expires_in": 3600}),
        )]);
        let flow = start("Test", spec(token)).unwrap();
        let url = Url::parse(&flow.url).unwrap();
        let asked: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(asked["client_id"], "client-1");
        assert_eq!(asked["code_challenge_method"], "S256");
        assert_eq!(asked["prompt"], "login");
        let port: u16 = asked["redirect_uri"]
            .strip_prefix("http://localhost:")
            .and_then(|rest| rest.strip_suffix("/cb"))
            .unwrap()
            .parse()
            .unwrap();
        let state = asked["state"].clone();
        // The vault is the test thread's, so the flow is finished here and
        // the browser knocks from another thread.
        let browser = std::thread::spawn(move || {
            [
                knock(port, "/favicon.ico"),
                knock(port, "/cb?code=c1&state=other"),
                knock(port, &format!("/cb?code=c1&state={state}")),
            ]
        });
        let tokens = flow.finish().unwrap();
        let said = browser.join().unwrap();
        assert!(said[0].contains("404"));
        assert!(said[1].contains("not the sign-in"));
        assert!(said[2].contains("Signed in to Test"));
        assert_eq!(
            (
                tokens.account.as_str(),
                tokens.plan.as_str(),
                tokens.email.as_str()
            ),
            ("acct-1", "plus", "a@example.com")
        );
        assert!(tokens.expires_at > now());
        let sent = bodies.recv().unwrap();
        assert!(sent.contains("code=c1") && sent.contains("code_verifier="));
        assert!(sent.contains("client_id=client-1"));
        assert_eq!(super::tokens("Test"), Some(tokens));
        assert!(vault.holds("provider:Test").is_some());
        assert!(!format!("{:?}", super::tokens("Test").unwrap()).contains("access-1"));
        sign_out("Test").unwrap();
        assert!(!signed_in("Test"));
    }

    #[test]
    fn a_pasted_code_or_code_and_state_finishes_the_sign_in() {
        assert_eq!(pasted_code("c1#s1", "s1"), Ok("c1".into()));
        assert!(pasted_code("c1#other", "s1").is_err());
        assert_eq!(
            pasted_code("http://localhost:1455/auth/callback?code=c1&state=s1", "s1"),
            Ok("c1".into())
        );
        assert_eq!(pasted_code("c1", "s1"), Ok("c1".into()));
    }

    #[test]
    fn a_refresh_keeps_what_it_leaves_out_and_a_refused_one_ends_the_sign_in() {
        let (_vault, _held) = vault("provider-refresh");
        let (token, bodies) = token_server(vec![
            (
                200,
                json!({"access_token": jwt(json!({"exp": now() + 7200}))}),
            ),
            (400, json!({"error": "invalid_grant"})),
        ]);
        let spec = spec(token);
        let kept = Tokens {
            access: "old".into(),
            refresh: "refresh-1".into(),
            account: "acct-1".into(),
            expires_at: now() + 10,
            ..Default::default()
        };
        assert!(ending(&kept, now()) && !ending(&Tokens::default(), now()));
        store("Test", &kept).unwrap();
        let fresh = renew("Test", spec, kept).unwrap();
        assert!(bodies.recv().unwrap().contains("refresh_token=refresh-1"));
        assert_eq!(
            (fresh.refresh.as_str(), fresh.account.as_str()),
            ("refresh-1", "acct-1")
        );
        assert!(!ending(&fresh, now()) && fresh.refreshed_at > 0);
        assert_eq!(super::tokens("Test"), Some(fresh.clone()));
        assert!(renew("Test", spec, fresh).unwrap_err().contains("ended"));
        assert!(!signed_in("Test"));
    }

    #[test]
    fn a_token_refused_just_after_a_refresh_is_not_refreshed_again() {
        let (_vault, _held) = vault("provider-refused");
        let mut kept = Tokens {
            access: "a1".into(),
            refreshed_at: now(),
            ..Default::default()
        };
        store("ChatGPT", &kept).unwrap();
        assert_eq!(refused("ChatGPT", "a0"), Ok("a1".into()));
        assert!(refused("ChatGPT", "a1").is_err());
        // Long since refreshed, with no refresh token to refresh with, it
        // has ended.
        kept.refreshed_at = 0;
        store("ChatGPT", &kept).unwrap();
        assert!(refused("ChatGPT", "a1").is_err());
        assert!(!signed_in("ChatGPT"));
    }

    #[test]
    fn with_no_vault_a_sign_in_is_never_kept() {
        assert!(store("Test", &Tokens::default()).is_err());
    }
}
