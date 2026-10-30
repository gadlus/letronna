//! JavaScript plugins, on the QuickJS engine Letronna already carries: a
//! thread and an engine of their own each, so nothing has to be installed
//! and one plugin cannot slow or break another.
//!
//! The thread sleeps on the plugin's queue and wakes for a message or the
//! next timer, so a plugin that waits costs no time at all. Each message is
//! handed to the prelude and the engine runs until it has nothing left to do
//! but wait; whatever the code waits on -- a fetch, a timer -- comes back as
//! another message, so nothing the host does blocks the engine and nothing
//! the engine does blocks the host. A stretch of the plugin's own work is
//! held to a time, after which the engine stops it with an error no `try`
//! catches, and the plugin to a heap far smaller than a workflow's, so a
//! plugin that loops or grows without end costs a moment and fails alone.
//!
//! What the code can reach is only what the natives below give it: its own
//! files to require, its own storage, timers, the plugin's messages, and
//! the internet over https when the plugin asked for the network. A plugin
//! can reach no file, no command and no address on this machine or the
//! local network.

use super::running::{Host, Loaded};
use crate::js::{STACK, engine_holding, options, thrown};
use rquickjs::{Ctx, Exception, Function, Object};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// The heap one plugin may use: room for any plugin written to be read, and
/// a tenth of a workflow's.
const HEAP: usize = 32 * 1024 * 1024;
/// The engine time its main file may take to run, and one message.
const START_TIME: Duration = Duration::from_secs(2);
const EVENT_TIME: Duration = Duration::from_millis(250);
const TOOL_TIME: Duration = Duration::from_secs(5);
/// Timers one plugin may hold, and how often one may repeat.
const TIMERS: usize = 32;
const SHORTEST_REPEAT: Duration = Duration::from_millis(100);
/// Fetches one plugin may have going at once, and what one may take.
const FETCHES: usize = 4;
const FETCH_TIME: Duration = Duration::from_secs(30);
const FETCH_BYTES: u64 = 4 * 1024 * 1024;
/// The bytes one plugin's storage may hold, and how often it is written.
const STORAGE_BYTES: usize = 1024 * 1024;
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// The prelude, which builds the plugin's world before its code runs.
const PRELUDE: &str = include_str!("prelude.js");
/// What the user is told of code the engine stopped.
const RAN_LONG: &str = "its code ran past its time and was stopped.";

/// A running JavaScript plugin, as `Loaded` holds it.
struct Running {
    stop: Arc<AtomicBool>,
    queue: SyncSender<Value>,
    generation: u64,
}

impl Host for Running {
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // The flag stops code that runs; the message wakes a thread that
        // waits, from a thread of its own when the queue is full. It names
        // this host, so a host started after it does not take it as its own.
        let stop = json!({"type": "stop", "generation": self.generation});
        if self.queue.try_send(stop.clone()).is_err() {
            let queue = self.queue.clone();
            std::thread::spawn(move || queue.send(stop));
        }
    }
}

/// Starts `plugin` on a thread of its own, as its host `generation`.
pub(super) fn start(plugin: Arc<Loaded>, generation: u64) -> Result<Box<dyn Host>, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let running = Running {
        stop: stop.clone(),
        queue: plugin.sender(),
        generation,
    };
    std::thread::Builder::new()
        .name(format!("plugin {}", plugin.name))
        .stack_size(STACK)
        .spawn(move || {
            // A panic in the engine still ends the host, so the plugin hears.
            let ended = catch_unwind(AssertUnwindSafe(|| run(&plugin, &stop, generation)))
                .unwrap_or_else(|_| Err("its script engine failed.".into()));
            plugin.ended(generation, ended);
        })
        .map_err(|e| format!("its thread could not start: {e}"))?;
    Ok(Box::new(running))
}

/// What the natives reach.
struct World {
    plugin: Arc<Loaded>,
    /// By id: when each is due, and how often it repeats.
    timers: HashMap<u64, (Instant, Option<Duration>)>,
    fetching: Arc<AtomicUsize>,
    /// Storage the code changed and is not written yet.
    unsaved: Option<String>,
    saved_at: Instant,
}

/// Runs `plugin` until it is stopped or fails.
fn run(plugin: &Arc<Loaded>, stop: &Arc<AtomicBool>, generation: u64) -> Result<(), String> {
    // Held while this runs, so a host started after a stop reads nothing
    // until this one has let go.
    let queue = plugin
        .queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let since = plugin.epoch();
    let deadline = Rc::new(Cell::new(None::<Instant>));
    // Whether the engine stopped the code since the last message came, so
    // what it answers with is put in words.
    let tripped = Rc::new(Cell::new(false));
    let asking = (deadline.clone(), tripped.clone(), stop.clone());
    let (_runtime, context) = engine_holding(HEAP, move || {
        let late = asking.0.get().is_some_and(|at| Instant::now() >= at);
        if late {
            asking.1.set(true);
        }
        late || asking.2.load(Ordering::SeqCst)
    })?;
    let world = Rc::new(RefCell::new(World {
        plugin: plugin.clone(),
        timers: HashMap::new(),
        fetching: Arc::new(AtomicUsize::new(0)),
        unsaved: None,
        saved_at: Instant::now(),
    }));
    let ended = context.with(|ctx| -> Result<(), String> {
        let budget = |time: Duration| deadline.set(Some(Instant::now() + time));
        let failed = |error: rquickjs::Error| match stop.load(Ordering::SeqCst) {
            true => "it was stopped.".to_owned(),
            false if tripped.get() => RAN_LONG.to_owned(),
            false => thrown(&ctx, error),
        };
        let mut hello = plugin.hello();
        hello["storage"] = stored(plugin);
        budget(START_TIME);
        let started = (|| -> rquickjs::Result<(Function, Function)> {
            let prelude: Function = ctx.eval_with_options(PRELUDE, options("prelude.js"))?;
            let natives = natives(&ctx, &world, &tripped)?;
            let api: Object = prelude.call((natives, hello.to_string()))?;
            Ok((api.get("receive")?, api.get("main")?))
        })();
        let (receive, main) = started.map_err(|e| format!("its prelude failed: {}", failed(e)))?;
        main.call::<_, ()>((plugin.code.main.clone(),))
            .map_err(|e| format!("its main file failed: {}", failed(e)))?;
        jobs(&ctx, &deadline, plugin);
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let next = wake_at(&world.borrow());
            let message = match next {
                Some(at) => queue.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => queue.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match message {
                Ok(message) if message["type"] == "stop" => {
                    if message["generation"] == generation {
                        return Ok(());
                    }
                }
                Ok(message) if Loaded::stale(&message, since) => {}
                Ok(message) => {
                    tripped.set(false);
                    budget(match message["type"].as_str() {
                        Some("tool" | "action") => TOOL_TIME,
                        _ => EVENT_TIME,
                    });
                    if let Err(error) = receive.call::<_, ()>((message.to_string(),)) {
                        let why = failed(error);
                        if let Some(id) = message["id"].as_u64() {
                            plugin.heard(json!({"reply": id, "error": why}));
                        }
                        plugin.problem(&why);
                    }
                    jobs(&ctx, &deadline, plugin);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            // Gathered first: a timer's code sets timers of its own.
            let due = due(&mut world.borrow_mut());
            for id in due {
                tripped.set(false);
                budget(EVENT_TIME);
                let tick = json!({"type": "timer", "id": id}).to_string();
                if let Err(error) = receive.call::<_, ()>((tick,)) {
                    plugin.problem(&format!("a timer failed: {}", failed(error)));
                }
                jobs(&ctx, &deadline, plugin);
            }
            save(&mut world.borrow_mut(), false);
        }
    });
    save(&mut world.borrow_mut(), true);
    ended
}

/// Runs what the code left to do -- the rest of its async functions -- until
/// it waits on nothing but the host. Past its time every job left fails at
/// once, so the work is dropped and the plugin goes on.
fn jobs(ctx: &Ctx, deadline: &Cell<Option<Instant>>, plugin: &Loaded) {
    deadline.set(Some(Instant::now() + TOOL_TIME));
    while ctx.execute_pending_job() {}
    if deadline.get().is_some_and(|at| Instant::now() >= at) {
        plugin.problem("ran past its time, and the work it was doing was dropped.");
    }
    deadline.set(None);
}

/// When the thread must wake though no message came: the next timer, or a
/// write of storage that is due.
fn wake_at(world: &World) -> Option<Instant> {
    let timer = world.timers.values().map(|(at, _)| *at).min();
    let save = world.unsaved.as_ref().map(|_| world.saved_at + SAVE_EVERY);
    timer.into_iter().chain(save).min()
}

/// The timers due now, in the order they fell due; a repeating one is set
/// again, a one-off forgotten.
fn due(world: &mut World) -> Vec<u64> {
    let now = Instant::now();
    let mut due: Vec<(Instant, u64)> = world
        .timers
        .iter()
        .filter(|(_, (at, _))| *at <= now)
        .map(|(id, (at, _))| (*at, *id))
        .collect();
    due.sort();
    for (_, id) in &due {
        match world.timers.get(id).and_then(|(_, every)| *every) {
            Some(every) => {
                world.timers.insert(*id, (now + every, Some(every)));
            }
            None => {
                world.timers.remove(id);
            }
        }
    }
    due.into_iter().map(|(_, id)| id).collect()
}

/// Writes the storage the code changed, at most once every `SAVE_EVERY`
/// unless `now`, whole, so a crash leaves the last one written.
fn save(world: &mut World, now: bool) {
    if world.unsaved.is_none() || (!now && world.saved_at.elapsed() < SAVE_EVERY) {
        return;
    }
    let Some(text) = world.unsaved.take() else {
        return;
    };
    world.saved_at = Instant::now();
    let own = &world.plugin.own;
    let written = std::fs::create_dir_all(own).and_then(|()| {
        let staged = own.join("storage.json.new");
        std::fs::write(&staged, text)?;
        std::fs::rename(&staged, own.join("storage.json"))
    });
    if let Err(why) = written {
        world
            .plugin
            .problem(&format!("could not keep its storage: {why}"));
    }
}

/// What the plugin's storage holds, or an empty object.
fn stored(plugin: &Loaded) -> Value {
    std::fs::read(plugin.own.join("storage.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// The natives the prelude builds the plugin's world on.
fn natives<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
    tripped: &Rc<Cell<bool>>,
) -> rquickjs::Result<Object<'js>> {
    let natives = Object::new(ctx.clone())?;
    let plugin = world.borrow().plugin.clone();
    natives.set(
        "post",
        Function::new(ctx.clone(), {
            let plugin = plugin.clone();
            let tripped = tripped.clone();
            move |ctx: Ctx<'js>, text: String| -> rquickjs::Result<()> {
                let mut message: Value = serde_json::from_str(&text)
                    .map_err(|e| Exception::throw_message(&ctx, &format!("Not plain data: {e}")))?;
                // The engine's stop reaches a promise as a rejection, which
                // the prelude answers with; the user is told why instead.
                if tripped.get() && message.get("reply").is_some() && message.get("error").is_some()
                {
                    message["error"] = RAN_LONG.into();
                }
                plugin.heard(message);
                Ok(())
            }
        })?,
    )?;
    natives.set(
        "timer",
        Function::new(ctx.clone(), {
            let world = world.clone();
            move |ctx: Ctx<'js>, id: f64, ms: f64, repeat: bool| -> rquickjs::Result<()> {
                let mut world = world.borrow_mut();
                if world.timers.len() >= TIMERS {
                    return Err(Exception::throw_message(
                        &ctx,
                        &format!("A plugin keeps at most {TIMERS} timers at once."),
                    ));
                }
                let wait = Duration::from_millis(ms.max(0.0).min(u32::MAX as f64) as u64);
                let every = repeat.then(|| wait.max(SHORTEST_REPEAT));
                let at = Instant::now() + every.unwrap_or(wait);
                world.timers.insert(id as u64, (at, every));
                Ok(())
            }
        })?,
    )?;
    natives.set(
        "untimer",
        Function::new(ctx.clone(), {
            let world = world.clone();
            move |id: f64| {
                world.borrow_mut().timers.remove(&(id as u64));
            }
        })?,
    )?;
    natives.set(
        "save",
        Function::new(ctx.clone(), {
            let world = world.clone();
            move |text: String| -> bool {
                if text.len() > STORAGE_BYTES {
                    return false;
                }
                world.borrow_mut().unsaved = Some(text);
                true
            }
        })?,
    )?;
    natives.set(
        "fetch",
        Function::new(ctx.clone(), {
            let world = world.clone();
            move |ctx: Ctx<'js>, id: f64, request: String| -> rquickjs::Result<()> {
                let world = world.borrow();
                if !world.plugin.code.network {
                    return Err(Exception::throw_message(
                        &ctx,
                        "This plugin did not ask for the network permission in plugin.json, so it \
                         cannot fetch.",
                    ));
                }
                let fetching = world.fetching.clone();
                if fetching.fetch_add(1, Ordering::SeqCst) >= FETCHES {
                    fetching.fetch_sub(1, Ordering::SeqCst);
                    return Err(Exception::throw_message(
                        &ctx,
                        &format!("A plugin has at most {FETCHES} fetches going at once."),
                    ));
                }
                let answer_to = world.plugin.sender();
                let id = id as u64;
                let spawned = std::thread::Builder::new()
                    .name(format!("plugin {} fetch", world.plugin.name))
                    .spawn(move || {
                        let mut answer =
                            fetch(&request).unwrap_or_else(|why| json!({"error": why}));
                        answer["type"] = "fetched".into();
                        answer["id"] = id.into();
                        // Its own thread, so it can wait for room.
                        let _ = answer_to.send(answer);
                        fetching.fetch_sub(1, Ordering::SeqCst);
                    });
                if let Err(e) = spawned {
                    world.fetching.fetch_sub(1, Ordering::SeqCst);
                    return Err(Exception::throw_message(
                        &ctx,
                        &format!("Could not fetch: {e}"),
                    ));
                }
                Ok(())
            }
        })?,
    )?;
    natives.set(
        "load",
        Function::new(ctx.clone(), {
            let plugin = plugin.clone();
            move |ctx: Ctx<'js>, path: String| -> rquickjs::Result<Option<Function<'js>>> {
                let Some(source) =
                    module(&plugin, &path).map_err(|e| Exception::throw_message(&ctx, &e))?
                else {
                    return Ok(None);
                };
                ctx.eval_with_options(source, options(&path)).map(Some)
            }
        })?,
    )?;
    Ok(natives)
}

/// The plugin's file at `path` as a CommonJS module's function, `None` when
/// there is no such file, or why it cannot be one.
fn module(plugin: &Loaded, path: &str) -> Result<Option<String>, String> {
    let inside = super::super::inside(path)?;
    let json = inside.ends_with(".json");
    if !(json || inside.ends_with(".js") || inside.ends_with(".cjs") || inside.ends_with(".mjs")) {
        return Ok(None);
    }
    let file = plugin.folder.join(&inside);
    if !file.is_file() {
        return Ok(None);
    }
    let mut source = String::new();
    std::fs::File::open(&file)
        .and_then(|file| {
            use std::io::Read;
            file.take(super::CODE_BYTES).read_to_string(&mut source)
        })
        .map_err(|e| format!("Could not read {inside}: {e}"))?;
    // On one line with the opening, so its lines keep their numbers.
    Ok(Some(match json {
        true => {
            format!("(function (exports, require, module) {{ module.exports = (\n{source}\n); }})")
        }
        false => {
            format!("(function (exports, require, module, __filename, __dirname) {{ {source}\n}})")
        }
    }))
}

/// The answer to one fetch: `{status, headers, body}`, or why there is none.
fn fetch(request: &str) -> Result<Value, String> {
    let request: Value =
        serde_json::from_str(request).map_err(|e| format!("The request is not plain data: {e}"))?;
    let url = request["url"].as_str().unwrap_or_default();
    let url = reqwest::Url::parse(url).map_err(|_| format!("{url} is not an address."))?;
    if url.scheme() != "https" {
        return Err(format!(
            "{url} is not an https address; a plugin fetches over https only."
        ));
    }
    if crate::web::local(&url) {
        return Err(format!(
            "{url} is on this machine or the local network, which a plugin cannot reach."
        ));
    }
    let method = request["method"].as_str().unwrap_or("GET");
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| format!("{method} is not a method."))?;
    let mut call = client()?.request(method, url.clone());
    for (name, value) in request["headers"].as_object().into_iter().flatten() {
        if let Some(value) = value.as_str() {
            call = call.header(name.as_str(), value);
        }
    }
    if let Some(body) = request["body"].as_str() {
        call = call.body(body.to_owned());
    }
    let response = call
        .send()
        .map_err(|e| format!("Could not reach {url}: {e}"))?;
    let status = response.status().as_u16();
    let headers: serde_json::Map<String, Value> = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.into())))
        .collect();
    let mut body = Vec::new();
    use std::io::Read;
    response
        .take(FETCH_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("Could not read {url}: {e}"))?;
    if body.len() as u64 > FETCH_BYTES {
        return Err(format!(
            "{url} sent more than the {} MB a plugin's fetch takes.",
            FETCH_BYTES / 1024 / 1024
        ));
    }
    Ok(json!({
        "status": status,
        "headers": headers,
        "body": String::from_utf8_lossy(&body),
    }))
}

/// The client plugins fetch with: https only, never redirected onto this
/// machine or the local network.
fn client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let policy = reqwest::redirect::Policy::custom(|attempt| {
                let next = attempt.url();
                if next.scheme() != "https" || crate::web::local(next) {
                    attempt.stop()
                } else if attempt.previous().len() > 5 {
                    attempt.error("redirected more than five times")
                } else {
                    attempt.follow()
                }
            });
            reqwest::blocking::Client::builder()
                .timeout(FETCH_TIME)
                .redirect(policy)
                .user_agent(concat!("Letronna-plugin/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| format!("Could not make the plugins' web client: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}
