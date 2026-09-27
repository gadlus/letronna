//! Talking to a Chromium browser over the DevTools protocol.
//!
//! Letronna drives the Chrome, Edge or Brave already on the computer rather
//! than shipping a browser: the protocol is JSON over a WebSocket, which core
//! already speaks for Discord, so nothing is downloaded and no Node or Python
//! is needed. The browser runs on a profile of its own in the data folder, so
//! the user's own browser is never touched -- Chrome 136 refuses to be driven
//! on its default profile anyway -- and a site signed in to there stays signed
//! in for next time.
//!
//! Nothing here runs between calls. A call writes its request and reads until
//! the answer comes back, dealing on the way with whatever else the page said;
//! anything the page says while nobody is asking waits in the socket for the
//! next call. No thread, no timer, nothing to wake up for.
//!
//! A browser elsewhere -- a cloud one, or Chrome on another machine -- is
//! reached by one address instead, and gives one socket for the whole
//! browser rather than one per tab. Every tab driven on it is then a session
//! on that socket, and a `Link` thread reads it, handing each answer to
//! whoever asked and each event to the session it is about.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};
use tungstenite::{Message as Frame, WebSocket};

/// How long one request may take before the browser is taken to be stuck.
const CALL_TIME: Duration = Duration::from_secs(20);
/// How long a browser has to start and open its debugging port. The first
/// start makes the profile, which is the slow part.
const START_TIME: Duration = Duration::from_secs(20);
/// How long the debugging port gets to answer a look at whether it is up.
const PROBE_TIME: Duration = Duration::from_secs(2);
/// How long a browser elsewhere gets to take the connection: it may be
/// across the world, and a cloud one starts a browser for it.
const REMOTE_TIME: Duration = Duration::from_secs(15);
/// How often the one socket to a browser elsewhere stops reading to send
/// what its tabs have asked: the most a request waits to go.
const LINK_POLL: Duration = Duration::from_millis(10);
/// The largest message taken from the browser. A long page's accessibility
/// tree runs to megabytes -- Wikipedia's article on Rust is 8.5 MB -- and one
/// past this is refused rather than held in memory.
const MAX_MESSAGE: usize = 64 << 20;
/// Events kept for the action in progress. A page that reloads in a loop
/// cannot grow this without bound.
const MAX_EVENTS: usize = 500;
/// The file a browser writes its debugging port into, in its profile.
const PORT_FILE: &str = "DevToolsActivePort";
/// What the browser is started with, besides its port and profile.
///
/// The first two keep a fresh profile from opening on welcome and "make me
/// your default" pages. The three `backgrounding` switches stop Chromium
/// from pausing a page whose window is behind Letronna's: without them a
/// screenshot waits for a frame that never comes, and a page's own timers
/// stall mid-task.
///
/// The next two keep the profile small. Left to itself Edge fills a new
/// profile with downloaded components -- shopping, wallet and language
/// models, 443 MB within ten seconds of one test run -- where the browsing
/// itself took 13 MB; what is lost is DRM video and certificate-list
/// updates, which a browser for an agent does without. The disk cache is
/// capped at 64 MB, which Chromium would otherwise size to the free disk.
///
/// The last keeps it light while it runs: the built-in extensions that run
/// in the background took six processes and about 135 MB of Edge's 324 MB
/// with Google open, for nothing an agent uses -- pages, screenshots and
/// PDFs all work without them.
///
/// There is deliberately no `--enable-automation` (it tells every site the
/// browser is automated) and no `--remote-allow-origins` (it would let any
/// page the browser opens drive the debugging port).
const SWITCHES: &[&str] = &[
    "--no-first-run",
    "--no-default-browser-check",
    "--hide-crash-restore-bubble",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--disable-background-timer-throttling",
    "--disable-component-update",
    "--disk-cache-size=67108864",
    "--disable-component-extensions-with-background-pages",
];
/// A window size for a browser with no window, which otherwise draws pages
/// at 800 by 600 and gets the phone layout of many sites.
const HIDDEN_WINDOW: &str = "--window-size=1280,900";
/// A browser with no window draws without the GPU. It is only ever seen as
/// screencast pictures, which the GPU would have to copy back out of its
/// own memory: with Wikipedia's Rust article open in the window's panel,
/// 418 MB and 46% of a core while scrolling, against 605 MB and 59% with
/// the GPU. What goes is WebGL, which pages an agent reads seldom need.
const NO_GPU: &str = "--disable-gpu";
/// With no GPU, what it would draw is drawn in the browser's own process
/// rather than one of its own: a process and 10 MB fewer.
const GPU_INSIDE: &str = "--in-process-gpu";
/// What a browser with no window calls itself in its user agent, where a
/// browser with one says "Chrome".
const HEADLESS: &str = "HeadlessChrome";

/// Where the Chromium browsers install themselves, as a path under a
/// Windows program folder, most wanted first: Chrome is what sites are
/// tested against, and Edge is on every Windows computer.
const WINDOWS: &[&str] = &[
    r"Google\Chrome\Application\chrome.exe",
    r"Microsoft\Edge\Application\msedge.exe",
    r"BraveSoftware\Brave-Browser\Application\brave.exe",
    r"Chromium\Application\chrome.exe",
];
const MAC: &[&str] = &[
    "Google Chrome.app/Contents/MacOS/Google Chrome",
    "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "Brave Browser.app/Contents/MacOS/Brave Browser",
    "Chromium.app/Contents/MacOS/Chromium",
];
const LINUX: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "microsoft-edge",
    "microsoft-edge-stable",
    "brave-browser",
    "brave",
    "chromium",
    "chromium-browser",
];

/// The browser to drive: the one `LETRONNA_BROWSER` names, else the first
/// installed one.
fn installed() -> Option<PathBuf> {
    first_existing(&candidates())
}

fn first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.is_file()).cloned()
}

/// Every place a browser might be, most wanted first.
fn candidates() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var_os("LETRONNA_BROWSER")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    if cfg!(windows) {
        let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .into_iter()
            .filter_map(env)
            .collect();
        for path in WINDOWS {
            paths.extend(roots.iter().map(|root| root.join(path)));
        }
    } else if cfg!(target_os = "macos") {
        let roots: Vec<PathBuf> = [
            Some(PathBuf::from("/Applications")),
            env("HOME").map(|h| h.join("Applications")),
        ]
        .into_iter()
        .flatten()
        .collect();
        for path in MAC {
            paths.extend(roots.iter().map(|root| root.join(path)));
        }
    } else {
        let dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        for name in LINUX {
            paths.extend(dirs.iter().map(|dir| dir.join(name)));
        }
    }
    paths
}

/// Keep a browser from holding on to this process's output. Windows gives a
/// new process every handle marked to be inherited, and a pipe from the
/// shell is: after `letronna -p ... | more`, a browser left open would hold
/// the pipe open, and whatever reads it would wait for the browser to be
/// closed. Children that need this process's output are handed it
/// explicitly, so none of them loses anything.
#[cfg(windows)]
fn keep_output_to_self() {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    for handle in handles {
        // SAFETY: these are this process's own standard handles, and only
        // whether a child inherits them changes. A window has none, and
        // the call then fails, which is fine.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
}

/// The debugging port written in a profile's port file.
fn port_in(text: &str) -> Option<u16> {
    text.lines()
        .next()?
        .trim()
        .parse()
        .ok()
        .filter(|port| *port > 0)
}

fn read_port(profile: &Path) -> Option<u16> {
    port_in(&std::fs::read_to_string(profile.join(PORT_FILE)).ok()?)
}

/// One HTTP client for the debugging port, kept rather than built per call:
/// each one starts a thread of its own.
fn http() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(PROBE_TIME)
        .no_proxy()
        .build()
        .map_err(|e| format!("Could not set up a connection to the browser: {e}"))?;
    Ok(CLIENT.get_or_init(|| client))
}

/// A call's parameters, in a browser context if one is named.
fn in_context(mut params: Value, context: Option<&str>) -> Value {
    if let Some(context) = context {
        params["browserContextId"] = json!(context);
    }
    params
}

/// The user agent of the browser whose debugging port this is, if it is up.
/// It says whether that browser has no window: one found running may not be
/// in the mode asked for, and a window someone may be using must not be
/// closed as a hidden one.
fn running(port: u16) -> Option<String> {
    let version: Value = http()
        .ok()?
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .ok()?;
    Some(
        version["User-Agent"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

/// A running browser: one Letronna started, or one it found still running on
/// its profile from an earlier run.
pub struct Browser {
    /// How to reach it, which each conversation using it keeps a copy of.
    pub reach: Reach,
    /// The process, when this run of Letronna started it.
    child: Option<Child>,
}

/// Where a running browser listens, and what it is: all it takes to use it.
#[derive(Clone)]
pub struct Reach {
    port: u16,
    /// Running with no window, where nobody can see it or close it.
    pub headless: bool,
    /// What it calls itself to sites.
    agent: String,
    /// The one socket to a browser elsewhere, for one reached by address.
    link: Option<Arc<Link>>,
}

/// One tab, as the browser lists it.
#[derive(Clone, Debug, Deserialize)]
pub struct Target {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(rename = "webSocketDebuggerUrl", default)]
    socket: String,
}

impl Browser {
    /// The browser on `profile`: the one already running there, or a new one.
    /// One with no window draws at `scale`, the screen's that shows it.
    pub fn start(profile: &Path, headless: bool, scale: Option<f64>) -> Result<Browser, String> {
        let found = read_port(profile).and_then(|port| Some((port, running(port)?)));
        if let Some((port, agent)) = found {
            return Ok(Browser::on(port, None, agent));
        }
        let exe = installed().ok_or(
            "No Chromium browser was found: install Chrome, Edge or Brave, or set \
             LETRONNA_BROWSER to the path of one.",
        )?;
        std::fs::create_dir_all(profile)
            .map_err(|e| format!("Could not make the browser's profile folder: {e}"))?;
        // Left over from a browser that has since gone; the new one writes
        // its own, and until it has, this one would point nowhere.
        let _ = std::fs::remove_file(profile.join(PORT_FILE));
        #[cfg(windows)]
        keep_output_to_self();
        let mut command = Command::new(&exe);
        command
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.display()))
            .args(SWITCHES)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if headless {
            command.args(["--headless=new", HIDDEN_WINDOW, NO_GPU, GPU_INSIDE]);
            if let Some(scale) = scale.filter(|scale| *scale > 1.) {
                command.arg(format!("--force-device-scale-factor={scale:.2}"));
            }
        }
        command.arg("about:blank");
        // Its own process group, so ctrl-c in the terminal that started
        // Letronna does not take the browser with it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let name = exe
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "The browser".into());
        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start {name}: {e}"))?;
        let started = Instant::now();
        loop {
            if let Some((port, agent)) =
                read_port(profile).and_then(|port| Some((port, running(port)?)))
            {
                return Ok(Browser::on(port, Some(child), agent));
            }
            if started.elapsed() > START_TIME {
                let _ = child.kill();
                return Err(format!(
                    "{name} did not open its debugging port within {} seconds.",
                    START_TIME.as_secs()
                ));
            }
            // A second browser on the same profile hands over to the first
            // and exits; give that one a moment to write its port before
            // giving up on it.
            if let Ok(Some(_)) = child.try_wait()
                && started.elapsed() > Duration::from_secs(3)
            {
                return Err(format!(
                    "{name} closed as soon as it started. If it is already open on \
                     Letronna's profile, close that window and try again."
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn on(port: u16, child: Option<Child>, agent: String) -> Browser {
        let reach = Reach {
            port,
            headless: agent.contains(HEADLESS),
            agent,
            link: None,
        };
        Browser { reach, child }
    }

    /// A browser elsewhere, at `address`: a cloud browser's connect address,
    /// its key in it as the service gives it, or the DevTools address of
    /// Chrome on another machine (`http://host:9222`), which says where its
    /// socket is.
    pub fn remote(address: &str) -> Result<Browser, String> {
        let address = address.trim();
        let socket = match address.starts_with("http://") || address.starts_with("https://") {
            true => {
                // Not the quick look used on this computer: it may be far.
                let far = reqwest::blocking::Client::builder()
                    .timeout(REMOTE_TIME)
                    .no_proxy()
                    .build()
                    .map_err(|e| format!("Could not set up a connection to the browser: {e}"))?;
                let version: Value = far
                    .get(format!("{}/json/version", address.trim_end_matches('/')))
                    .send()
                    .and_then(|reply| reply.json())
                    .map_err(|_| format!("Nothing at {} answered as a browser.", shown(address)))?;
                version["webSocketDebuggerUrl"]
                    .as_str()
                    .ok_or_else(|| {
                        format!(
                            "The browser at {} did not say where to reach it.",
                            shown(address)
                        )
                    })?
                    .to_owned()
            }
            false => address.to_owned(),
        };
        let link = Link::open(&socket)?;
        let version: Value =
            Tab::over(&link, None, "browser").call("Browser.getVersion", json!({}))?;
        let agent = version["userAgent"].as_str().unwrap_or_default().to_owned();
        let reach = Reach {
            port: 0,
            headless: agent.contains(HEADLESS),
            agent,
            link: Some(link),
        };
        Ok(Browser { reach, child: None })
    }

    /// A browser with no window that an earlier run left on the profile --
    /// one that crashed, or was stopped to be updated -- if nothing drives
    /// it: nothing is attached to any tab of it.
    pub fn left_over(profile: &Path) -> Option<Browser> {
        let port = read_port(profile)?;
        let browser = Browser::on(port, None, running(port)?);
        if !browser.reach.headless {
            return None;
        }
        let targets: Value = browser
            .reach
            .connect()
            .ok()?
            .call("Target.getTargets", json!({}))
            .ok()?;
        let driven = targets["targetInfos"]
            .as_array()?
            .iter()
            .any(|info| info["attached"] == true);
        (!driven).then_some(browser)
    }

    /// Close the whole browser, every tab of it. One elsewhere is somebody
    /// else's and outlives Letronna: it is only let go of, with the private
    /// contexts made on it.
    pub fn close(&mut self) {
        if let Some(link) = &self.reach.link {
            return link.let_go();
        }
        if let Ok(mut browser) = self.reach.connect() {
            let _ = browser.call::<Ignored>("Browser.close", json!({}));
        }
        let asked = Instant::now();
        // One this run did not start is not its child to wait on: it has
        // gone once its port stops answering, and its profile is free.
        let Some(child) = self.child.as_mut() else {
            while asked.elapsed() < PROBE_TIME && self.reach.alive() {
                std::thread::sleep(Duration::from_millis(50));
            }
            return;
        };
        while asked.elapsed() < PROBE_TIME {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Reach {
    /// Whether the browser still answers.
    pub fn alive(&self) -> bool {
        match &self.link {
            Some(link) => link.open.load(Ordering::Relaxed),
            None => running(self.port).is_some(),
        }
    }

    /// A connection to the browser itself, rather than to one of its tabs.
    fn connect(&self) -> Result<Tab, String> {
        if let Some(link) = &self.link {
            return Ok(Tab::over(link, None, "browser"));
        }
        let version: Value = http()?
            .get(format!("http://127.0.0.1:{}/json/version", self.port))
            .send()
            .and_then(|reply| reply.json())
            .map_err(|_| "The browser did not answer.".to_owned())?;
        let socket = version["webSocketDebuggerUrl"]
            .as_str()
            .ok_or("The browser did not say where to reach it.")?;
        Tab::connect(socket, "browser")
    }

    /// A browser on this computer at `port`, for tests to stand in for.
    #[cfg(test)]
    pub(crate) fn on_port(port: u16, headless: bool) -> Reach {
        Reach {
            port,
            headless,
            agent: String::new(),
            link: None,
        }
    }

    /// A tab's own debugging socket.
    fn socket_of(&self, id: &str) -> String {
        format!("ws://127.0.0.1:{}/devtools/page/{id}", self.port)
    }

    /// A connection to one tab: its own socket, or a session on the one
    /// socket to a browser elsewhere.
    pub fn open(&self, id: &str) -> Result<Tab, String> {
        let Some(link) = &self.link else {
            return Tab::connect(&self.socket_of(id), id);
        };
        let attached: Value = self.connect()?.call(
            "Target.attachToTarget",
            json!({"targetId": id, "flatten": true}),
        )?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or("The browser did not let the tab be driven.")?;
        Ok(Tab::over(link, Some(session.to_owned()), id))
    }

    /// The tabs in a browser context -- a private one, or with `None` the
    /// browser's own -- the one used most recently first.
    pub fn tabs(&self, context: Option<&str>) -> Result<Vec<Target>, String> {
        if self.link.is_some() {
            return self.tabs_elsewhere(context);
        }
        let listed: Vec<Target> = http()?
            .get(format!("http://127.0.0.1:{}/json/list", self.port))
            .send()
            .and_then(|reply| reply.json())
            .map_err(|_| "The browser did not list its tabs.".to_owned())?;
        // The list says nothing of contexts: the browser itself does.
        let mut browser = self.connect()?;
        let targets: Value = browser.call("Target.getTargets", json!({}))?;
        let private: Value = browser.call("Target.getBrowserContexts", json!({}))?;
        let private: Vec<&str> = private["browserContextIds"]
            .as_array()
            .map(|ids| ids.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let inside: Vec<&str> = targets["targetInfos"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|info| {
                let own = info["browserContextId"].as_str().unwrap_or_default();
                match context {
                    Some(context) => own == context,
                    None => !private.contains(&own),
                }
            })
            .filter_map(|info| info["targetId"].as_str())
            .collect();
        Ok(listed
            .into_iter()
            .filter(|target| target.kind == "page" && !target.socket.is_empty())
            .filter(|target| inside.contains(&target.id.as_str()))
            .collect())
    }

    /// The tabs in a browser context of a browser elsewhere, which lists
    /// them only as targets, in no order of use.
    fn tabs_elsewhere(&self, context: Option<&str>) -> Result<Vec<Target>, String> {
        let mut browser = self.connect()?;
        let targets: Value = browser.call("Target.getTargets", json!({}))?;
        let private: Value = browser.call("Target.getBrowserContexts", json!({}))?;
        let private: Vec<&str> = private["browserContextIds"]
            .as_array()
            .map(|ids| ids.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        Ok(targets["targetInfos"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|info| info["type"] == "page")
            .filter(|info| {
                let own = info["browserContextId"].as_str().unwrap_or_default();
                match context {
                    Some(context) => own == context,
                    None => !private.contains(&own),
                }
            })
            .map(|info| Target {
                id: info["targetId"].as_str().unwrap_or_default().to_owned(),
                title: info["title"].as_str().unwrap_or_default().to_owned(),
                url: info["url"].as_str().unwrap_or_default().to_owned(),
                kind: "page".into(),
                socket: String::new(),
            })
            .collect())
    }

    /// A new blank tab in a browser context.
    pub fn new_tab(&self, context: Option<&str>) -> Result<Target, String> {
        let Some(context) = context.or(self.link.as_ref().map(|_| "")) else {
            return http()?
                .put(format!(
                    "http://127.0.0.1:{}/json/new?about:blank",
                    self.port
                ))
                .send()
                .and_then(|reply| reply.json())
                .map_err(|_| "The browser did not open a new tab.".to_owned());
        };
        // A browser elsewhere makes its own tabs too; an empty context is
        // its own.
        let made: Value = self.connect()?.call(
            "Target.createTarget",
            in_context(
                json!({"url": "about:blank"}),
                Some(context).filter(|c| !c.is_empty()),
            ),
        )?;
        let id = made["targetId"]
            .as_str()
            .ok_or("The browser did not open a new tab.")?;
        Ok(Target {
            id: id.to_owned(),
            title: String::new(),
            url: "about:blank".into(),
            kind: "page".into(),
            socket: self.socket_of(id),
        })
    }

    /// A private browser context: its own cookies, storage and tabs, as an
    /// incognito window has, kept in memory only.
    pub fn new_context(&self) -> Result<String, String> {
        let made: Value = self.connect()?.call(
            "Target.createBrowserContext",
            json!({"disposeOnDetach": false}),
        )?;
        let context = made["browserContextId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "The browser did not make a private context.".to_owned())?;
        if let Some(link) = &self.link
            && let Ok(mut made) = link.contexts.lock()
        {
            made.push(context.clone());
        }
        Ok(context)
    }

    /// Close a private browser context, every tab of it, and forget it.
    pub fn drop_context(&self, context: &str) {
        if let Some(link) = &self.link
            && let Ok(mut made) = link.contexts.lock()
        {
            made.retain(|made| made != context);
        }
        if let Ok(mut browser) = self.connect() {
            let _ = browser.call::<Ignored>(
                "Target.disposeBrowserContext",
                json!({"browserContextId": context}),
            );
        }
    }

    /// Every cookie a browser context holds -- with `None`, the browser's
    /// own -- sign-ins among them.
    pub fn cookies(&self, context: Option<&str>) -> Result<Vec<Value>, String> {
        let mut got: Value = self
            .connect()?
            .call("Storage.getCookies", in_context(json!({}), context))?;
        Ok(match got["cookies"].take() {
            Value::Array(cookies) => cookies,
            _ => Vec::new(),
        })
    }

    /// Give a browser context back cookies it held.
    pub fn set_cookies(
        &self,
        context: Option<&str>,
        mut cookies: Vec<Value>,
    ) -> Result<(), String> {
        for cookie in &mut cookies {
            // One kept only while the browser runs ends at -1, which given
            // back would end it long ago.
            if cookie["session"] == true
                && let Some(fields) = cookie.as_object_mut()
            {
                fields.remove("expires");
            }
        }
        let params = in_context(json!({"cookies": cookies}), context);
        self.connect()?
            .call::<Ignored>("Storage.setCookies", params)
            .map(drop)
    }

    /// Connect to a tab, ready to be driven.
    pub fn attach(&self, target: &Target) -> Result<Tab, String> {
        let mut tab = match self.link {
            Some(_) => self.open(&target.id)?,
            None => Tab::connect(&target.socket, &target.id)?,
        };
        tab.call::<Ignored>("Page.enable", json!({}))?;
        tab.call::<Ignored>("Page.setLifecycleEventsEnabled", json!({"enabled": true}))?;
        // Some sites turn away a browser that says it has no window. This
        // holds while the connection does, which is while the tab is in use.
        if self.headless {
            let agent = self.agent.replace(HEADLESS, "Chrome");
            tab.call::<Ignored>(
                "Emulation.setUserAgentOverride",
                json!({"userAgent": agent}),
            )?;
        }
        Ok(tab)
    }

    /// Bring a tab to the front of its window.
    pub fn show(&self, target: &Target) {
        if self.link.is_some() {
            if let Ok(mut browser) = self.connect() {
                let _ = browser
                    .call::<Ignored>("Target.activateTarget", json!({"targetId": target.id}));
            }
            return;
        }
        if let Ok(client) = http() {
            let _ = client
                .get(format!(
                    "http://127.0.0.1:{}/json/activate/{}",
                    self.port, target.id
                ))
                .send();
        }
    }
}

/// A result nobody reads.
pub type Ignored = serde::de::IgnoredAny;

/// Something the page said without being asked.
#[derive(Debug)]
pub struct Event {
    pub method: String,
    pub params: Value,
}

/// A frame from the browser, read far enough to know what it is.
#[derive(Deserialize)]
struct Envelope {
    id: Option<u64>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
struct Answer<T> {
    result: Option<T>,
    error: Option<Refusal>,
}

#[derive(Deserialize)]
struct Refusal {
    message: String,
}

/// A connection to one tab.
pub struct Tab {
    pub id: String,
    wire: Wire,
    next: u64,
    /// Set once the connection has failed. The tab is no use after that: it
    /// was closed, or the browser was.
    pub broken: bool,
    /// What the page said while calls waited, oldest first.
    pub events: VecDeque<Event>,
    /// Whether a confirm or prompt the page opens is answered OK.
    pub agree: bool,
    /// Dialogs answered since these were last taken, as the model is told.
    pub dialogs: Vec<String>,
    /// The dialog open now: its kind and what it asks.
    open_dialog: Option<(String, String)>,
    /// Leave a dialog the page opens for someone to answer, and keep it
    /// with the other events, rather than answer it at once.
    pub leaves_dialogs: bool,
}

/// How a tab is talked to.
enum Wire {
    /// A socket of its own, as a browser on this computer gives each tab.
    Socket(WebSocket<TcpStream>),
    /// A session on the one socket to a browser elsewhere -- or none, for
    /// the browser itself -- with the answers and events meant for it.
    Session {
        link: Arc<Link>,
        session: Option<String>,
        inbox: mpsc::Receiver<String>,
        to_inbox: mpsc::Sender<String>,
    },
}

impl Tab {
    /// A tab, or with no session the browser itself, on the one socket to a
    /// browser elsewhere.
    fn over(link: &Arc<Link>, session: Option<String>, id: &str) -> Tab {
        let (to_inbox, inbox) = mpsc::channel();
        if let Some(session) = &session
            && let Ok(mut sessions) = link.sessions.lock()
        {
            sessions.insert(session.clone(), to_inbox.clone());
        }
        Tab::on(
            id,
            Wire::Session {
                link: link.clone(),
                session,
                inbox,
                to_inbox,
            },
        )
    }

    fn on(id: &str, wire: Wire) -> Tab {
        Tab {
            id: id.to_owned(),
            wire,
            next: 0,
            broken: false,
            events: VecDeque::new(),
            agree: false,
            dialogs: Vec::new(),
            open_dialog: None,
            leaves_dialogs: false,
        }
    }

    /// Connect to a debugging socket, such as a tab's `webSocketDebuggerUrl`.
    pub fn connect(address: &str, id: &str) -> Result<Tab, String> {
        let unreachable = || "Could not connect to the browser.".to_owned();
        let url = reqwest::Url::parse(address).map_err(|_| unreachable())?;
        let host = url.host_str().ok_or_else(unreachable)?;
        let port = url.port_or_known_default().ok_or_else(unreachable)?;
        let place = (host, port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut places| places.next())
            .ok_or_else(unreachable)?;
        let stream = TcpStream::connect_timeout(&place, PROBE_TIME).map_err(|_| unreachable())?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(CALL_TIME));
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (socket, _) = tungstenite::client::client_with_config(address, stream, Some(config))
            .map_err(|_| unreachable())?;
        Ok(Tab::on(id, Wire::Socket(socket)))
    }

    /// Ask the browser something and wait for its answer.
    pub fn call<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T, String> {
        self.call_within(method, params, CALL_TIME)
    }

    /// `call`, with its own time limit.
    pub fn call_within<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<T, String> {
        let id = self.send(method, params)?;
        let deadline = Instant::now() + limit;
        loop {
            let Some(text) = self.read(deadline)? else {
                return Err(format!("The browser did not answer in time ({method})."));
            };
            let envelope: Envelope = serde_json::from_str(&text)
                .map_err(|_| "The browser sent something unreadable.".to_owned())?;
            if envelope.id != Some(id) {
                self.heard(envelope);
                continue;
            }
            // Read a second time, into the shape asked for: a page's whole
            // accessibility tree comes this way, and going through a `Value`
            // first would hold it in memory twice over.
            let answer: Answer<T> = serde_json::from_str(&text)
                .map_err(|e| format!("The browser's answer to {method} could not be read: {e}"))?;
            return match (answer.result, answer.error) {
                (_, Some(refusal)) => Err(refusal.message),
                (Some(result), None) => Ok(result),
                (None, None) => Err(format!("The browser sent no answer to {method}.")),
            };
        }
    }

    /// Ask the browser something without waiting for the answer, which is
    /// let go of when it comes. For input: a click that opens a dialog is
    /// not answered until the dialog is.
    pub fn tell(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(method, params).map(drop)
    }

    /// Read until an event `until` accepts has come, or `limit` has passed.
    /// Events already read count. True when it came.
    pub fn wait(
        &mut self,
        limit: Duration,
        until: impl Fn(&Event) -> bool,
    ) -> Result<bool, String> {
        if self.events.iter().any(&until) {
            return Ok(true);
        }
        let deadline = Instant::now() + limit;
        while let Some(text) = self.read(deadline)? {
            let Ok(envelope) = serde_json::from_str::<Envelope>(&text) else {
                continue;
            };
            let seen = self.heard(envelope);
            if seen.is_some_and(|at| self.events.get(at).is_some_and(&until)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn send(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let sent = match &mut self.wire {
            Wire::Socket(socket) => {
                self.next += 1;
                let request = json!({"id": self.next, "method": method, "params": params});
                socket
                    .send(Frame::text(request.to_string()))
                    .map(|_| self.next)
                    .map_err(drop)
            }
            // Ids are the socket's, not the tab's: every tab on it shares them.
            Wire::Session {
                link,
                session,
                to_inbox,
                ..
            } => {
                let id = link.next.fetch_add(1, Ordering::Relaxed) + 1;
                let mut request = json!({"id": id, "method": method, "params": params});
                if let Some(session) = session {
                    request["sessionId"] = json!(session);
                }
                if let Ok(mut waiting) = link.waiting.lock() {
                    waiting.insert(id, to_inbox.clone());
                }
                link.send(request.to_string()).map(|_| id)
            }
        };
        sent.map_err(|_| {
            self.broken = true;
            GONE.to_owned()
        })
    }

    /// The next frame's text, or `None` once `deadline` has passed.
    fn read(&mut self, deadline: Instant) -> Result<Option<tungstenite::Utf8Bytes>, String> {
        let socket = match &mut self.wire {
            Wire::Socket(socket) => socket,
            Wire::Session { link, inbox, .. } => loop {
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    return Ok(None);
                };
                // Looked at again now and then: the socket may have gone.
                match inbox.recv_timeout(left.clamp(Duration::from_millis(1), PROBE_TIME)) {
                    Ok(text) if text == DETACHED => {
                        self.broken = true;
                        return Err(GONE.into());
                    }
                    Ok(text) => return Ok(Some(text.into())),
                    Err(mpsc::RecvTimeoutError::Timeout) if link.open.load(Ordering::Relaxed) => {}
                    Err(_) => {
                        self.broken = true;
                        return Err(GONE.into());
                    }
                }
            },
        };
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            // A zero timeout means none at all, which would wait forever.
            let left = left.max(Duration::from_millis(1));
            let _ = socket.get_ref().set_read_timeout(Some(left));
            match socket.read() {
                Ok(Frame::Text(text)) => return Ok(Some(text)),
                Ok(Frame::Close(_)) => break,
                Ok(_) => continue,
                // A timed-out read keeps what it had of the frame, so the
                // next one carries on from there.
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            }
        }
        self.broken = true;
        Err(GONE.into())
    }

    /// Deal with a frame nobody asked for. A dialog is answered at once --
    /// the page stands still until it is -- and anything else is kept for
    /// the action in progress to look at. Where it was kept, if it was.
    fn heard(&mut self, envelope: Envelope) -> Option<usize> {
        let method = envelope.method?;
        let text = |key: &str| envelope.params[key].as_str().unwrap_or_default().to_owned();
        if method == "Page.javascriptDialogOpening" && !self.leaves_dialogs {
            let (kind, message) = (text("type"), text("message"));
            let reply = json!({
                "accept": agrees(&kind, self.agree),
                "promptText": text("defaultPrompt"),
            });
            // Its answer is not waited for: the call that was waiting when
            // the dialog opened gets its own once the page moves again.
            let _ = self.send("Page.handleJavaScriptDialog", reply);
            self.open_dialog = Some((kind, message));
            return None;
        }
        // Told once it is closed, and how: the user may have answered it
        // first, from the window.
        if method == "Page.javascriptDialogClosed" && !self.leaves_dialogs {
            if let Some((kind, message)) = self.open_dialog.take() {
                let agreed = envelope.params["result"].as_bool().unwrap_or(false);
                self.dialogs.push(note(&kind, &message, agreed));
            }
            return None;
        }
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            method,
            params: envelope.params,
        });
        Some(self.events.len() - 1)
    }
}

impl Drop for Tab {
    /// A session on a browser elsewhere is let go of there too.
    fn drop(&mut self) {
        if let Wire::Session {
            link,
            session: Some(session),
            ..
        } = &self.wire
        {
            if let Ok(mut sessions) = link.sessions.lock() {
                sessions.remove(session);
            }
            let id = link.next.fetch_add(1, Ordering::Relaxed) + 1;
            let detach = json!({"id": id, "method": "Target.detachFromTarget",
                "params": {"sessionId": session}});
            let _ = link.send(detach.to_string());
        }
    }
}

const GONE: &str = "The browser tab was closed.";

/// Handed to a session's tab when the browser lets the session go. Not
/// JSON, so nothing takes it for the browser's word.
const DETACHED: &str = "detached";

/// The one socket to a browser elsewhere, shared by every tab driven on it.
pub struct Link {
    /// What the tabs ask, for the thread to send.
    outgoing: Mutex<mpsc::Sender<String>>,
    /// Who waits for the answer to each request, by its id.
    waiting: Mutex<HashMap<u64, mpsc::Sender<String>>>,
    /// Where each session's events go.
    sessions: Mutex<HashMap<String, mpsc::Sender<String>>>,
    next: AtomicU64,
    open: AtomicBool,
    /// Private contexts made on it, closed when it is let go of: the browser
    /// is somebody else's, and they would stay there.
    contexts: Mutex<Vec<String>>,
}

impl Link {
    fn open(address: &str) -> Result<Arc<Link>, String> {
        let unreachable =
            |why: String| format!("Could not reach the browser at {}: {why}", shown(address));
        // Parsed, it gains the `/` a service's `wss://host?token=…` leaves out,
        // which the handshake needs.
        let url = reqwest::Url::parse(address).map_err(|_| unreachable("not an address".into()))?;
        let host = url
            .host_str()
            .ok_or_else(|| unreachable("no host".into()))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| unreachable("no port".into()))?;
        let place = (host, port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut places| places.next())
            .ok_or_else(|| unreachable("the name did not resolve".into()))?;
        let stream = TcpStream::connect_timeout(&place, REMOTE_TIME)
            .map_err(|e| unreachable(e.to_string()))?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(REMOTE_TIME));
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (socket, _) =
            tungstenite::client_tls_with_config(url.as_str(), stream, Some(config), None).map_err(
                |e| match e {
                    tungstenite::HandshakeError::Failure(tungstenite::Error::Http(reply))
                        if matches!(reply.status().as_u16(), 401 | 403) =>
                    {
                        format!(
                            "The browser at {} refused the key in its address (HTTP {}).",
                            shown(address),
                            reply.status().as_u16()
                        )
                    }
                    e => unreachable(e.to_string()),
                },
            )?;
        let (outgoing, queue) = mpsc::channel();
        let link = Arc::new(Link {
            outgoing: Mutex::new(outgoing),
            waiting: Mutex::default(),
            sessions: Mutex::default(),
            next: AtomicU64::new(0),
            open: AtomicBool::new(true),
            contexts: Mutex::default(),
        });
        let reader = link.clone();
        std::thread::spawn(move || reader.run(socket, queue));
        Ok(link)
    }

    fn send(&self, text: String) -> Result<(), ()> {
        if !self.open.load(Ordering::Relaxed) {
            return Err(());
        }
        let outgoing = self.outgoing.lock().map_err(drop)?;
        outgoing.send(text).map_err(drop)
    }

    /// Send what is asked and read what comes, until the socket closes or
    /// is let go of.
    fn run(
        &self,
        mut socket: WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
        queue: mpsc::Receiver<String>,
    ) {
        use tungstenite::stream::MaybeTlsStream;
        let stream = match socket.get_ref() {
            MaybeTlsStream::Plain(stream) => Some(stream),
            MaybeTlsStream::Rustls(tls) => Some(tls.get_ref()),
            _ => None,
        };
        if let Some(stream) = stream {
            let _ = stream.set_read_timeout(Some(LINK_POLL));
        }
        'running: loop {
            while let Ok(text) = queue.try_recv() {
                if socket.send(Frame::text(text)).is_err() {
                    break 'running;
                }
            }
            if !self.open.load(Ordering::Relaxed) {
                break;
            }
            match socket.read() {
                Ok(Frame::Text(text)) => self.hand_on(text.as_str().to_owned()),
                Ok(Frame::Close(_)) => break,
                Ok(_) => {}
                // Nothing yet: a timed-out read keeps what it had of a frame.
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
        self.open.store(false, Ordering::Relaxed);
        let _ = socket.close(None);
        let _ = socket.flush();
        // Whoever still waits hears it is gone now, not at their time limit.
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.clear();
        }
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }

    /// Hand an answer to whoever asked, and an event to its session.
    fn hand_on(&self, text: String) {
        #[derive(Deserialize)]
        struct Routing {
            id: Option<u64>,
            #[serde(rename = "sessionId")]
            session: Option<String>,
            method: Option<String>,
        }
        let Ok(routing) = serde_json::from_str::<Routing>(&text) else {
            return;
        };
        // A session let go of by the browser, as when its tab closed: a tab
        // with a socket of its own sees it close, and this one is told.
        if routing.method.as_deref() == Some("Target.detachedFromTarget") {
            #[derive(Deserialize)]
            struct Detached {
                params: Session,
            }
            #[derive(Deserialize)]
            struct Session {
                #[serde(rename = "sessionId")]
                session: String,
            }
            let gone = serde_json::from_str::<Detached>(&text)
                .ok()
                .and_then(|detached| self.sessions.lock().ok()?.remove(&detached.params.session));
            if let Some(to) = gone {
                let _ = to.send(DETACHED.to_owned());
            }
            return;
        }
        let to = match (routing.id, routing.session) {
            (Some(id), _) => self.waiting.lock().ok().and_then(|mut w| w.remove(&id)),
            (None, Some(session)) => self
                .sessions
                .lock()
                .ok()
                .and_then(|s| s.get(&session).cloned()),
            (None, None) => None,
        };
        if let Some(to) = to {
            let _ = to.send(text);
        }
    }

    /// Close the private contexts made on it, and the socket.
    fn let_go(&self) {
        let contexts = self
            .contexts
            .lock()
            .map(|mut c| std::mem::take(&mut *c))
            .unwrap_or_default();
        for context in contexts {
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            let dispose = json!({"id": id, "method": "Target.disposeBrowserContext",
                "params": {"browserContextId": context}});
            let _ = self.send(dispose.to_string());
        }
        // The thread sends what is queued before it sees this and stops.
        self.open.store(false, Ordering::Relaxed);
    }
}

/// An address as it may be shown: without what follows the host, where a
/// service puts its key.
pub(crate) fn shown(address: &str) -> String {
    match reqwest::Url::parse(address) {
        Ok(url) => match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => format!("{}://{host}:{port}", url.scheme()),
            (Some(host), None) => format!("{}://{host}", url.scheme()),
            _ => "that address".into(),
        },
        Err(_) => "that address".into(),
    }
}

/// Whether a dialog is answered OK. Leaving a page and closing a message
/// are what the action was for; anything asking for agreement is refused
/// unless the action said to agree.
fn agrees(kind: &str, agree: bool) -> bool {
    matches!(kind, "beforeunload" | "alert") || agree
}

/// What the model is told of a dialog, answered OK or not.
fn note(kind: &str, message: &str, agreed: bool) -> String {
    let message: String = message.chars().take(200).collect();
    match (kind, agreed) {
        ("beforeunload", true) => "The page asked to confirm leaving it; it was left.".into(),
        ("beforeunload", false) => "The page asked to confirm leaving it; it stayed.".into(),
        ("alert", _) => format!("The page showed a message: \"{message}\"."),
        (_, true) => format!("The page asked \"{message}\" and was answered OK."),
        (_, false) => format!(
            "The page asked \"{message}\" and was answered Cancel; use_page with confirm: \
             true answers OK."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn the_debugging_port_is_read_from_the_profile() {
        assert_eq!(port_in("54340\n/devtools/browser/b4f24c57\n"), Some(54340));
        assert_eq!(port_in("9222"), Some(9222));
        // Half written, or from something else entirely.
        assert_eq!(port_in(""), None);
        assert_eq!(port_in("0\n"), None);
        assert_eq!(port_in("ws://127.0.0.1\n"), None);
    }

    #[test]
    fn the_first_browser_installed_is_the_one_used() {
        let dir = std::env::temp_dir().join(format!("letronna-find-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (chrome, edge) = (dir.join("chrome.exe"), dir.join("msedge.exe"));
        std::fs::write(&edge, "").unwrap();
        assert_eq!(
            first_existing(&[chrome.clone(), edge.clone()]),
            Some(edge.clone())
        );
        std::fs::write(&chrome, "").unwrap();
        assert_eq!(first_existing(&[chrome.clone(), edge]), Some(chrome));
        // A folder where a browser should be is not one.
        assert_eq!(first_existing(std::slice::from_ref(&dir)), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dialog_is_answered_as_the_action_said() {
        assert!(agrees("alert", false) && agrees("beforeunload", false));
        assert!(!agrees("confirm", false) && agrees("confirm", true));
        assert!(!agrees("prompt", false));
        let refused = note("confirm", "Delete this?", false);
        assert!(
            refused.contains("Delete this?") && refused.contains("confirm: true"),
            "{refused}"
        );
        assert!(note("confirm", "Delete this?", true).contains("answered OK"));
        assert!(note("beforeunload", "", false).contains("stayed"));
    }

    /// A stand-in for a browser's debugging socket: answers each request
    /// with `reply(method, params)`, after sending the events it returns.
    fn fake_browser(
        reply: impl Fn(&str, &Value) -> (Vec<Value>, Value) + Send + 'static,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("ws://{}/devtools/page/T1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let mut heard = Vec::new();
            while let Ok(Frame::Text(text)) = socket.read() {
                let request: Value = serde_json::from_str(text.as_str()).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                heard.push(method.clone());
                let (events, result) = reply(&method, &request["params"]);
                for event in events {
                    socket.send(Frame::text(event.to_string())).unwrap();
                }
                let answer = if result.get("error").is_some() {
                    json!({"id": request["id"], "error": result["error"]})
                } else {
                    json!({"id": request["id"], "result": result})
                };
                socket.send(Frame::text(answer.to_string())).unwrap();
            }
            heard
        });
        (address, server)
    }

    #[test]
    fn a_call_gets_its_answer_past_what_the_page_says_meanwhile() {
        let (address, server) = fake_browser(|method, params| match method {
            "Page.navigate" => (
                vec![
                    json!({"method": "Page.frameStartedLoading", "params": {"frameId": "T1"}}),
                    json!({"method": "Page.javascriptDialogOpening",
                           "params": {"type": "confirm", "message": "Leave?"}}),
                ],
                json!({"frameId": "T1", "loaderId": "L2"}),
            ),
            "Page.handleJavaScriptDialog" => (
                vec![json!({"method": "Page.javascriptDialogClosed",
                            "params": {"result": params["accept"]}})],
                json!({}),
            ),
            "Broken.method" => (
                Vec::new(),
                json!({"error": {"message": "'Broken.method' wasn't found"}}),
            ),
            _ => (
                vec![json!({"method": "Page.frameStoppedLoading", "params": {"frameId": "T1"}})],
                json!({}),
            ),
        });
        let mut tab = Tab::connect(&address, "T1").unwrap();
        let navigated: Value = tab
            .call("Page.navigate", json!({"url": "https://example.com"}))
            .unwrap();
        assert_eq!(navigated["loaderId"], "L2");
        assert_eq!(tab.events.len(), 1);
        assert_eq!(tab.events[0].method, "Page.frameStartedLoading");
        // An event already read counts; a new one is read for.
        assert!(
            tab.wait(Duration::from_millis(50), |e| e.method
                == "Page.frameStartedLoading")
                .unwrap()
        );
        tab.call::<Ignored>("Page.stopLoading", json!({})).unwrap();
        // The dialog was answered on the spot, and No, since nothing agreed;
        // the model is told once the page says it closed.
        assert_eq!(tab.dialogs.len(), 1);
        assert!(
            tab.dialogs[0].contains("Leave?") && tab.dialogs[0].contains("Cancel"),
            "{:?}",
            tab.dialogs
        );
        assert!(
            tab.wait(Duration::from_secs(2), |e| e.method
                == "Page.frameStoppedLoading")
                .unwrap()
        );
        assert!(
            !tab.wait(Duration::from_millis(50), |e| e.method
                == "Page.loadEventFired")
                .unwrap()
        );
        // The browser's own refusal comes back as the error.
        let refused = tab.call::<Ignored>("Broken.method", json!({})).unwrap_err();
        assert!(refused.contains("wasn't found"), "{refused}");
        assert!(!tab.broken);
        drop(tab);
        let heard = server.join().unwrap();
        assert_eq!(
            heard,
            [
                "Page.navigate",
                "Page.handleJavaScriptDialog",
                "Page.stopLoading",
                "Broken.method"
            ]
        );
    }

    #[test]
    fn a_tab_that_goes_away_says_so_and_stays_gone() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("ws://{}/devtools/page/T1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let _ = socket.read();
            let _ = socket.close(None);
            let _ = socket.flush();
        });
        let mut tab = Tab::connect(&address, "T1").unwrap();
        let error = tab.call::<Ignored>("Page.reload", json!({})).unwrap_err();
        assert_eq!(error, GONE);
        assert!(tab.broken);
        server.join().unwrap();
    }

    #[test]
    fn a_browser_that_does_not_answer_is_given_up_on() {
        let (address, _server) = fake_browser(|_, _| {
            std::thread::sleep(Duration::from_millis(400));
            (Vec::new(), json!({}))
        });
        let mut tab = Tab::connect(&address, "T1").unwrap();
        let started = Instant::now();
        let error = tab
            .call_within::<Ignored>("Page.reload", json!({}), Duration::from_millis(100))
            .unwrap_err();
        assert!(error.contains("did not answer"), "{error}");
        assert!(started.elapsed() < Duration::from_millis(350));
        // Slow is not gone: the late answer is skipped, and the next call
        // still gets its own.
        assert!(!tab.broken);
        tab.call::<Ignored>("Page.reload", json!({})).unwrap();
    }
}
