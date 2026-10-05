//! The script host: runs a workflow's JavaScript on QuickJS, through rquickjs,
//! and answers its agent() calls through the run's journal.
//!
//! A run gets one thread of its own, as an engine context never leaves the
//! thread that made it. The loop there lets the script run until it has
//! nothing left to do but wait, then blocks on the run's next answer and hands
//! it in, so nothing polls. An answer the journal already has comes back on
//! the next turn of that loop, never inside the call that asked, so an await
//! behaves the same whether the agent ran or was replayed.
//!
//! The script's meta is read by a parser of its own before anything runs: it
//! has to come first and be a plain literal, so a run can be shown and
//! approved without running any of it. The body becomes an async function, so
//! top-level await and return work. What would make a resume see a different
//! run -- the clock, Math.random -- and every way of turning text into code
//! are taken away by the prelude, which then freezes the built-ins.
//!
//! QuickJS asks the host every ten thousand loop turns and calls whether to go
//! on, which is how a runaway stops however it is nested: ten million of those
//! without handing control back, or 30 s of engine time in all, and the run
//! stops with an error no try can catch. The heap and the stack have limits
//! of their own, so a script that grows without end fails instead of taking
//! the process down. Boa, the pure-Rust engine tried first, added 8.7 MB to
//! the terminal and could do none of this. The `script` tool runs on the
//! same engine, held to the same limits.

use super::{Run, Step};
use rquickjs::context::{EvalOptions, intrinsic};
use rquickjs::{Context, Ctx, Exception, Function, Object, Promise, Runtime, Undefined};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The largest script a run takes: Claude Code's cap, far past any script
/// written to be read.
const SCRIPT_SIZE: usize = 512 * 1024;
/// Loop turns and calls a script may make before it hands control back: a
/// fan-out over thousands of answers makes tens of thousands, and ten million
/// go by in well under a second.
const TURNS: u64 = 10_000_000;
/// How many loop turns and calls QuickJS makes between asking whether to go on.
const TURNS_PER_ASK: u64 = 10_000;
/// The engine time a run may use in all, so a script that spins stops even
/// while it keeps handing control back.
const ENGINE_TIME: Duration = Duration::from_secs(30);
/// The heap a script may use: far past a run's answers, which the journal
/// keeps anyway, and far short of what would squeeze the machine.
const HEAP: usize = 256 * 1024 * 1024;
/// The stack the engine may use, QuickJS's own default: calls a few thousand
/// deep, and a script nested past that fails to parse rather than overflow.
const JS_STACK: usize = 1024 * 1024;
/// The script thread's stack: the engine's, with room for the host around it.
pub(crate) const STACK: usize = 8 * 1024 * 1024;
/// How deep meta's objects and arrays may nest: it is two or three deep.
const META_DEPTH: usize = 16;
/// Lines of a run's log kept from one launch, Claude Code's cap, so a
/// logging loop cannot fill the disk.
const LOG_LINES: usize = 1_000;
/// The bytes of one log line kept: far past a line written to be read, and
/// with LOG_LINES at most 4 MB of log a launch however long the lines are.
const LOG_LINE: usize = 4 * 1024;
/// The prelude, which sets the script's world up before it runs.
const PRELUDE: &str = include_str!("prelude.js");
/// What a script's body is wrapped in: a function of the API that makes the
/// body an async function, so top-level await and return work.
const OPEN: &str =
    "({ agent, parallel, pipeline, phase, log, args, budget, workflow, console }) => async () => {";
const CLOSE: &str = "\n}";
/// Why a script without a proper meta is refused.
const NO_META: &str = "A workflow script starts with export const meta = { name: \"...\", \
                       description: \"...\" }, written out as a plain literal.";
/// Characters that end a line in JavaScript.
const LINE_ENDS: [char; 4] = ['\n', '\r', '\u{2028}', '\u{2029}'];

/// Finds the script of the saved workflow called `name`, for `workflow()`.
pub type Find = Box<dyn Fn(&str) -> Result<String, String> + Send>;

/// How a script's run ended.
#[derive(Debug, PartialEq)]
pub enum End {
    /// The script returned this.
    Returned(Value),
    /// It threw, was refused or stopped, or ran too long; this says which.
    Failed(String),
    /// The run paused, and why; its session has been told already.
    Paused(String),
}

/// A script's meta, read by parsing it and without running any of it; refused
/// unless it comes first and is a plain literal with a name and a description.
pub fn meta(script: &str) -> Result<Value, String> {
    check(script).map(|(meta, _)| meta)
}

/// Runs `script` with `args` on a thread of its own, driving `run`, and gives
/// `then` how it ended once its agents have stopped and the run has let its
/// folder go; refused at once when the script is not one.
pub fn start(
    run: Run,
    script: &str,
    args: &Value,
    find: Find,
    then: impl FnOnce(End) + Send + 'static,
) -> Result<(), String> {
    launch(run, script, args, find, ENGINE_TIME, then)
}

/// `start`, with the engine time the run may use.
fn launch(
    run: Run,
    script: &str,
    args: &Value,
    find: Find,
    most: Duration,
    then: impl FnOnce(End) + Send + 'static,
) -> Result<(), String> {
    let (_, body) = check(script)?;
    let args = args.to_string();
    std::thread::Builder::new()
        .name(format!("workflow {}", run.name()))
        .stack_size(STACK)
        .spawn(move || {
            // A panic in the engine still ends the run, so `then` always hears.
            let end = catch_unwind(AssertUnwindSafe(|| drive(run, find, &body, &args, most)))
                .unwrap_or_else(|_| {
                    End::Failed("The script engine failed; the run stopped.".into())
                });
            then(end);
        })
        .map(drop)
        .map_err(|e| format!("Could not start the workflow's script: {e}"))
}

/// What the script's calls into the host reach.
struct Host {
    run: Run,
    find: Find,
    /// The run's log, opened at its first line.
    log: Option<File>,
    /// Lines written to the log so far.
    lines: usize,
}

/// The engine time and turns a script has used, which the engine's question
/// whether to go on is answered from.
pub(crate) struct Clock {
    /// Engine time used before the current stretch.
    used: Cell<Duration>,
    /// When the current stretch of engine time began, while the script runs.
    since: Cell<Option<Instant>>,
    /// How often the engine has asked in the current stretch.
    asked: Cell<u64>,
    /// The engine time the run may use.
    most: Duration,
    /// The limit that stopped the script, once one has.
    hit: Cell<Option<Limit>>,
}

/// A limit that stops a script.
#[derive(Clone, Copy)]
pub(crate) enum Limit {
    Turns,
    Time,
}

impl Clock {
    /// A clock for a script that may use `most` engine time in all.
    pub(crate) fn new(most: Duration) -> Self {
        Self {
            used: Cell::new(Duration::ZERO),
            since: Cell::new(None),
            asked: Cell::new(0),
            most,
            hit: Cell::new(None),
        }
    }

    /// Whether the engine should stop the script, as it asks now and then
    /// while it runs; once a limit is hit, it stays hit.
    pub(crate) fn stop(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        let elapsed = self
            .since
            .get()
            .map_or(Duration::ZERO, |since| since.elapsed());
        if self.asked.get() > TURNS / TURNS_PER_ASK {
            self.hit.set(self.hit.get().or(Some(Limit::Turns)));
        } else if self.used.get() + elapsed > self.most {
            self.hit.set(self.hit.get().or(Some(Limit::Time)));
        }
        self.hit.get().is_some()
    }

    /// Runs `work` in the engine, counting the time and turns it takes.
    pub(crate) fn stretch<T>(&self, work: impl FnOnce() -> T) -> T {
        self.since.set(Some(Instant::now()));
        self.asked.set(0);
        let done = work();
        if let Some(since) = self.since.take() {
            self.used.set(self.used.get() + since.elapsed());
        }
        done
    }

    /// The limit that stopped the script, once one has.
    pub(crate) fn hit(&self) -> Option<Limit> {
        self.hit.get()
    }

    /// Why the script was stopped, when a limit stopped it.
    fn why(&self) -> Option<String> {
        Some(match self.hit()? {
            Limit::Turns => format!(
                "The workflow script was stopped: it made more than {TURNS} loop turns and calls \
                 without waiting for a helper. Look for a loop that never ends."
            ),
            Limit::Time => format!(
                "The workflow script was stopped: its own code ran for more than {} in all, \
                 not counting its waits for helpers. Look for a loop that never ends or \
                 heavy work between agent() calls.",
                if self.most.as_secs() > 0 {
                    format!("{} s", self.most.as_secs())
                } else {
                    format!("{} ms", self.most.as_millis())
                }
            ),
        })
    }
}

/// Runs the script to its end on an engine of its own, which is gone, and
/// the run with it, by the time this returns.
fn drive(run: Run, find: Find, body: &str, args: &str, most: Duration) -> End {
    let clock = Rc::new(Clock::new(most));
    let host = Rc::new(RefCell::new(Host {
        run,
        find,
        log: None,
        lines: 0,
    }));
    let asking = clock.clone();
    let (_runtime, context) = match engine(move || asking.stop()) {
        Ok(engine) => engine,
        Err(e) => return End::Failed(e),
    };
    let end = context.with(|ctx| turns(&ctx, &host, &clock, body, args));
    // However it ended, its agents are stopped and waited for, so none is
    // still writing when the run lets its folder go and a resume can start.
    let mut host = host.borrow_mut();
    host.run.cancel.set();
    while host.run.step().is_some() {}
    end
}

/// An engine held to the heap and stack limits, asking `stop` now and then
/// while a script runs whether to stop it, with a context of its own.
pub(crate) fn engine(stop: impl FnMut() -> bool + 'static) -> Result<(Runtime, Context), String> {
    let engine = Runtime::new().and_then(|runtime| {
        runtime.set_memory_limit(HEAP);
        runtime.set_max_stack_size(JS_STACK);
        runtime.set_interrupt_handler(Some(Box::new(stop)));
        // No WeakRef, which shows when the collector runs, and no
        // performance clock: only what a script needs.
        type Intrinsics = (
            intrinsic::Date,
            intrinsic::Eval,
            intrinsic::RegExpCompiler,
            intrinsic::RegExp,
            intrinsic::Json,
            intrinsic::Proxy,
            intrinsic::MapSet,
            intrinsic::TypedArrays,
            intrinsic::Promise,
        );
        Context::custom::<Intrinsics>(&runtime).map(|context| (runtime, context))
    });
    engine.map_err(|e| format!("Could not start the script engine: {e}"))
}

/// The host's loop: runs what the script can, then waits for its next answer
/// and hands it in, until the script returns or throws, its run pauses or is
/// stopped, or it waits on something nothing will answer.
fn turns(ctx: &Ctx, host: &Rc<RefCell<Host>>, clock: &Clock, body: &str, args: &str) -> End {
    let started = clock.stretch(|| begin(ctx, host, body, args));
    let (promise, settle) = match started {
        Ok(started) => started,
        Err(error) => return paused(host).unwrap_or(End::Failed(failed(ctx, clock, error))),
    };
    loop {
        // A job that throws past the script's reach is dropped here; the
        // clock says which limit threw it.
        clock.stretch(|| while ctx.execute_pending_job() {});
        if let Some(end) = paused(host) {
            return end;
        }
        if let Some(why) = clock.why() {
            return End::Failed(why);
        }
        let settled = promise.result::<String>().map(|result| match result {
            Ok(text) => returned(&text),
            Err(error) => End::Failed(failed(ctx, clock, error)),
        });
        let step = {
            let mut host = host.borrow_mut();
            let run = &mut host.run;
            // A stop wins over a script that returned on the nulls it left,
            // as one stopped while paused does: it ends stopped, telling nobody.
            if run.cancel.is_set() {
                return End::Failed(format!("Workflow {} was stopped.", run.name));
            }
            if let Some(end) = settled {
                return end;
            }
            run.step()
        };
        let (slot, answer) = match step {
            Some(Step::Answer(slot, answer)) => (slot, answer),
            Some(Step::Paused(reason)) => return End::Paused(reason),
            None => {
                return End::Failed(
                    "The workflow script can go no further: it waits on a promise nothing will \
                     settle."
                        .into(),
                );
            }
        };
        let answer = serde_json::to_string(&answer).unwrap_or_else(|_| "null".into());
        let handed = clock.stretch(|| settle.call::<_, ()>((slot as f64, answer)));
        if let Err(error) = handed {
            return paused(host).unwrap_or(End::Failed(failed(ctx, clock, error)));
        }
    }
}

/// The end of a script whose run has paused. Its session has heard it paused,
/// so that wins over whatever the script made of the refusals after, even a
/// retry loop a limit stopped.
fn paused(host: &RefCell<Host>) -> Option<End> {
    host.borrow().run.paused.clone().map(End::Paused)
}

/// Sets the prelude up and starts the script, giving the promise of its
/// result as JSON text and the prelude's function that hands answers in.
fn begin<'js>(
    ctx: &Ctx<'js>,
    host: &Rc<RefCell<Host>>,
    body: &str,
    args: &str,
) -> rquickjs::Result<(Promise<'js>, Function<'js>)> {
    let prelude: Function = ctx.eval_with_options(PRELUDE, options("prelude.js"))?;
    let total = host.borrow().run.limit as f64;
    let natives = (
        queue(ctx, host)?,
        compile(ctx, host)?,
        write(ctx, host)?,
        spent(ctx, host)?,
        total,
    );
    let world: Object = prelude.call(natives)?;
    let run: Function = world.get("run")?;
    let settle: Function = world.get("settle")?;
    let make: Function = ctx.eval_with_options(body, options("workflow.js"))?;
    let promise: Promise = run.call((make, Undefined, args))?;
    Ok((promise, settle))
}

/// How a script is evaluated: as global code, not strict unless it says so,
/// named `file` where its errors say where they were.
pub(crate) fn options(file: &str) -> EvalOptions {
    // Made from the default, as the engine may add fields to it.
    let mut options = EvalOptions::default();
    options.strict = false;
    options.filename = Some(file.into());
    options
}

/// Why the script failed, in words for the user: the limit that stopped it,
/// or what it threw, and where in the script.
fn failed(ctx: &Ctx, clock: &Clock, error: rquickjs::Error) -> String {
    if let Some(why) = clock.why() {
        return why;
    }
    format!("The workflow script failed: {}", thrown(ctx, error))
}

/// What a script threw, or the engine's own error: its name and message,
/// then where in the script, without the prelude's own frames.
pub(crate) fn thrown(ctx: &Ctx, error: rquickjs::Error) -> String {
    if !matches!(error, rquickjs::Error::Exception) {
        return error.to_string();
    }
    let thrown = ctx.catch();
    if let Some(exception) = thrown.as_exception() {
        let name: String = exception.get("name").unwrap_or_else(|_| "Error".into());
        let message = exception.message().unwrap_or_default();
        // Where in the script, without the prelude's own frames.
        let stack = exception.stack().unwrap_or_default();
        let script = stack
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.contains("prelude.js"));
        std::iter::once(format!("{name}: {message}"))
            .chain(script.map(str::to_owned))
            .collect::<Vec<_>>()
            .join("\n")
    } else if let Some(text) = thrown.as_string() {
        text.to_string().unwrap_or_default()
    } else {
        ctx.json_stringify(thrown)
            .ok()
            .flatten()
            .and_then(|text| text.to_string().ok())
            .unwrap_or_else(|| "a value that is not an error".into())
    }
}

/// An error the script can catch, saying `message`.
fn refused(ctx: &Ctx, message: &str) -> rquickjs::Error {
    Exception::throw_message(ctx, message)
}

/// The host's agent(): takes a task and its options as JSON text, giving the
/// number its answer comes back under.
fn queue<'js>(ctx: &Ctx<'js>, host: &Rc<RefCell<Host>>) -> rquickjs::Result<Function<'js>> {
    let host = host.clone();
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, prompt: String, options: String| -> rquickjs::Result<f64> {
            let options: Value = serde_json::from_str(&options)
                .map_err(|e| refused(&ctx, &format!("agent() options must be plain data: {e}")))?;
            let slot = host.borrow_mut().run.agent(&prompt, &options);
            slot.map(|slot| slot as f64).map_err(|e| refused(&ctx, &e))
        },
    )
}

/// The host's workflow(): turns the saved workflow of a name into a script
/// function.
fn compile<'js>(ctx: &Ctx<'js>, host: &Rc<RefCell<Host>>) -> rquickjs::Result<Function<'js>> {
    let host = host.clone();
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, name: String| -> rquickjs::Result<Function<'js>> {
            let script = (host.borrow().find)(&name).map_err(|e| refused(&ctx, &e))?;
            let (_, body) =
                check(&script).map_err(|e| refused(&ctx, &format!("Workflow {name}: {e}")))?;
            ctx.eval_with_options(body, options(&format!("{name}.js")))
        },
    )
}

/// The host's log(): keeps a line in the run's log, up to its cap.
fn write<'js>(ctx: &Ctx<'js>, host: &Rc<RefCell<Host>>) -> rquickjs::Result<Function<'js>> {
    let host = host.clone();
    Function::new(ctx.clone(), move |line: String| {
        let mut host = host.borrow_mut();
        if host.lines > LOG_LINES {
            return;
        }
        host.lines += 1;
        let line = if host.lines > LOG_LINES {
            format!("(the log stops here: a run keeps {LOG_LINES} lines of it)")
        } else {
            clipped(line)
        };
        if host.log.is_none() {
            let path = host.run.dir.join("log.txt");
            host.log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok();
        }
        // A line that cannot be written costs only itself.
        if let Some(log) = &mut host.log {
            let _ = writeln!(log, "{line}");
        }
    })
}

/// `line`, cut to LOG_LINE bytes at a character's edge, saying so where it
/// was cut.
fn clipped(mut line: String) -> String {
    if line.len() <= LOG_LINE {
        return line;
    }
    line.truncate(line.floor_char_boundary(LOG_LINE));
    line.push_str(&format!(
        " ... (cut: a log line keeps {} KB)",
        LOG_LINE / 1024
    ));
    line
}

/// The host's budget.spent(): the tokens the run has spent, earlier
/// launches' included.
fn spent<'js>(ctx: &Ctx<'js>, host: &Rc<RefCell<Host>>) -> rquickjs::Result<Function<'js>> {
    let host = host.clone();
    Function::new(ctx.clone(), move || {
        let run = &host.borrow().run;
        (run.earlier + run.budget.spent()) as f64
    })
}

/// What the script returned, from its result as JSON text.
fn returned(text: &str) -> End {
    match serde_json::from_str(text) {
        Ok(value) => End::Returned(value),
        Err(e) => End::Failed(format!(
            "The workflow script's result could not be read: {e}"
        )),
    }
}

/// Checks a script by reading its meta, giving the meta and the body wrapped
/// for the engine.
fn check(script: &str) -> Result<(Value, String), String> {
    if script.len() > SCRIPT_SIZE {
        return Err(format!(
            "The workflow script is {} KB, more than the {} KB a script may be.",
            script.len() / 1024,
            SCRIPT_SIZE / 1024
        ));
    }
    let mut reader = Reader {
        text: script,
        at: 0,
    };
    reader.skip().ok_or(NO_META)?;
    let export = reader.at;
    let meta = reader.meta().ok_or(NO_META)?;
    let named = meta["name"]
        .as_str()
        .is_some_and(|name| !name.trim().is_empty());
    if !named || !meta["description"].is_string() {
        return Err(NO_META.into());
    }
    // `export` belongs at a module's top level and the body runs as a
    // function, so it gives way to spaces, which keep the columns as they were.
    let after = &script[export + "export".len()..];
    let body = format!("{OPEN}{}      {after}{CLOSE}", &script[..export]);
    Ok((meta, body))
}

/// A place in a script's text, for reading its meta as a parser would.
struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Reader<'a> {
    /// Reads `export const meta = <literal>` and its end, giving the literal.
    fn meta(&mut self) -> Option<Value> {
        for word in ["export", "const", "meta"] {
            self.word(word)?;
        }
        self.punct('=')?;
        let meta = self.value(0)?;
        (meta.is_object() && self.ends_statement()).then_some(meta)
    }

    /// What is left of the text.
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    /// Moves past whitespace and comments; None when a comment never ends.
    fn skip(&mut self) -> Option<()> {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            self.at += rest.len() - trimmed.len();
            if let Some(comment) = trimmed.strip_prefix("//") {
                self.at += 2 + comment.find(LINE_ENDS).unwrap_or(comment.len());
            } else if let Some(comment) = trimmed.strip_prefix("/*") {
                self.at += 2 + comment.find("*/")? + 2;
            } else {
                return Some(());
            }
        }
    }

    /// The next character, past whitespace and comments.
    fn peek(&mut self) -> Option<char> {
        self.skip()?;
        self.rest().chars().next()
    }

    /// Takes `word` when it comes next as a whole word.
    fn word(&mut self, word: &str) -> Option<()> {
        self.skip()?;
        let after = self.rest().strip_prefix(word)?;
        if after.starts_with(identifier) {
            return None;
        }
        self.at += word.len();
        Some(())
    }

    /// Takes `mark` when it comes next.
    fn punct(&mut self, mark: char) -> Option<()> {
        (self.peek()? == mark).then(|| self.at += mark.len_utf8())
    }

    /// Whether the statement ends here: at a semicolon, the end, or a line
    /// break that what follows does not carry on from.
    fn ends_statement(&mut self) -> bool {
        let before = self.at;
        if self.skip().is_none() {
            return false;
        }
        let broke = self.text[before..self.at].contains(LINE_ENDS);
        let rest = self.rest();
        rest.is_empty()
            || rest.starts_with(';')
            || (broke && !rest.starts_with(|c: char| ",.([`+-*/%?=<>&|^".contains(c)))
    }

    /// The literal that comes next, inside `depth` others.
    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > META_DEPTH {
            return None;
        }
        match self.peek()? {
            '{' => {
                self.at += 1;
                let mut fields = serde_json::Map::new();
                while self.punct('}').is_none() {
                    let key = self.key()?;
                    self.punct(':')?;
                    fields.insert(key, self.value(depth + 1)?);
                    if self.punct(',').is_none() {
                        self.punct('}')?;
                        break;
                    }
                }
                Some(Value::Object(fields))
            }
            '[' => {
                self.at += 1;
                let mut items = Vec::new();
                while self.punct(']').is_none() {
                    items.push(self.value(depth + 1)?);
                    if self.punct(',').is_none() {
                        self.punct(']')?;
                        break;
                    }
                }
                Some(Value::Array(items))
            }
            '"' | '\'' | '`' => self.string().map(Value::String),
            '-' | '.' | '0'..='9' => self.number(),
            _ => [
                ("true", Value::Bool(true)),
                ("false", Value::Bool(false)),
                ("null", Value::Null),
            ]
            .into_iter()
            .find_map(|(word, value)| self.word(word).map(|()| value)),
        }
    }

    /// A property's name: a plain identifier or a quoted string.
    fn key(&mut self) -> Option<String> {
        if matches!(self.peek()?, '"' | '\'') {
            return self.string();
        }
        let rest = self.rest();
        let length = rest.find(|c| !identifier(c)).unwrap_or(rest.len());
        let key = &rest[..length];
        if key.is_empty() || key.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        self.at += length;
        Some(key.to_owned())
    }

    /// A quoted string, or a template with nothing put into it, as its text.
    fn string(&mut self) -> Option<String> {
        let quote = self.rest().chars().next()?;
        self.at += quote.len_utf8();
        let rest = self.rest();
        let mut text = String::new();
        let mut chars = rest.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            match c {
                _ if c == quote => {
                    self.at += at + 1;
                    return Some(text);
                }
                '$' if quote == '`' && rest[at..].starts_with("${") => return None,
                '\n' | '\r' if quote != '`' => return None,
                // A template reads a line break as \n, however it was written.
                '\r' => {
                    text.push('\n');
                    chars.next_if(|&(_, next)| next == '\n');
                }
                '\\' => {
                    let (_, escaped) = chars.next()?;
                    let code = match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'v' => '\u{b}',
                        '0' => '\0',
                        'x' | 'u' => {
                            let after = &rest[at + 2..];
                            let (digits, used) = match after.strip_prefix('{') {
                                Some(braced) if escaped == 'u' => {
                                    let end = braced.find('}')?;
                                    (&braced[..end], end + 2)
                                }
                                _ if escaped == 'u' => (after.get(..4)?, 4),
                                _ => (after.get(..2)?, 2),
                            };
                            for _ in 0..used {
                                chars.next();
                            }
                            let mut code = u32::from_str_radix(digits, 16).ok()?;
                            // A character past U+FFFF may be written as two
                            // escapes, a surrogate pair, as JSON writes it.
                            if (0xD800..0xDC00).contains(&code) {
                                let low = after
                                    .get(used..)?
                                    .strip_prefix("\\u")?
                                    .get(..4)
                                    .and_then(|low| u32::from_str_radix(low, 16).ok())
                                    .filter(|low| (0xDC00..0xE000).contains(low))?;
                                for _ in 0.."\\uXXXX".len() {
                                    chars.next();
                                }
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            char::from_u32(code)?
                        }
                        // A line continuation adds nothing.
                        '\r' => {
                            chars.next_if(|&(_, next)| next == '\n');
                            continue;
                        }
                        '\n' | '\u{2028}' | '\u{2029}' => continue,
                        other => other,
                    };
                    text.push(code);
                }
                _ => text.push(c),
            }
        }
        None
    }

    /// A decimal number, which may be negative, whole or not.
    fn number(&mut self) -> Option<Value> {
        let rest = self.rest();
        let mut length = usize::from(rest.starts_with('-'));
        let mut previous = ' ';
        for c in rest[length..].chars() {
            let sign = matches!(c, '+' | '-') && matches!(previous, 'e' | 'E');
            if !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || sign) {
                break;
            }
            previous = c;
            length += c.len_utf8();
        }
        let written = rest[..length].replace('_', "");
        self.at += length;
        if let Ok(whole) = written.parse::<i64>() {
            return Some(whole.into());
        }
        let number: f64 = written.parse().ok()?;
        serde_json::Number::from_f64(number).map(Value::Number)
    }
}

/// Whether `c` can be part of a plain identifier.
fn identifier(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

#[cfg(test)]
mod tests {
    use super::super::BUDGET;
    use super::super::tests::{Asked, Data, answering, done, provider, report};
    use super::*;
    use crate::budget::Budget;
    use crate::driver::{Outcome, Report};
    use serde_json::json;
    use std::sync::{Arc, mpsc};

    /// Far longer than any script here takes, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(60);
    /// A meta for scripts whose meta is not what they test.
    const META: &str = "export const meta = { name: \"test\", description: \"A test.\" };\n";

    /// Finds no saved workflow.
    fn nothing_saved() -> Find {
        Box::new(|name| Err(format!("There is no saved workflow \"{name}\".")))
    }

    /// How `script` ends on `run`, given `most` engine time and `find` for
    /// the workflows it nests, failing rather than hanging.
    fn ends_within(run: Run, script: &str, find: Find, most: Duration) -> End {
        let (sent, got) = mpsc::channel();
        let then = move |end| {
            let _ = sent.send(end);
        };
        launch(run, script, &json!({"file": "a.rs"}), find, most, then).unwrap();
        got.recv_timeout(WAIT).expect("the script never ended")
    }

    /// How `body`, under a plain meta, ends on a new run whose agents answer
    /// as `answer` does.
    fn ends(data: &Data, body: &str, answer: fn(&str) -> Report) -> End {
        let run = data.run(None, answering(&Asked::default(), answer));
        ends_within(run, &format!("{META}{body}"), nothing_saved(), ENGINE_TIME)
    }

    /// Answers as a helper does, but fails every task that starts with "Fail".
    fn failing(task: &str) -> Report {
        if task.starts_with("Fail") {
            report(Outcome::Failed("The provider is down.".into()), "")
        } else {
            done(task)
        }
    }

    /// What a script returned, failing the test when it did not return.
    fn gave(end: End) -> Value {
        let End::Returned(value) = end else {
            panic!("the script did not return: {end:?}");
        };
        value
    }

    /// Whether a script failed with an error that starts with `start`.
    fn failed_with(end: &End, start: &str) -> bool {
        matches!(end, End::Failed(error) if error.starts_with(start))
    }

    #[test]
    fn math_random_is_gone() {
        let data = Data::new("host-random");
        let tried = gave(ends(
            &data,
            r#"
            const tried = [];
            const call = () => {
              try {
                tried.push(Math.random());
              } catch (error) {
                tried.push(error.message);
              }
            };
            call();
            // Neither putting it back nor swapping Math brings it back.
            try { Math.random = () => 0.5; } catch {}
            try { Object.defineProperty(Math, "random", { value: () => 0.5 }); } catch {}
            try { globalThis.Math = { random: () => 0.5 }; } catch {}
            call();
            return tried;
            "#,
            done,
        ));
        let gone = "Math.random() is not available in workflow scripts: a resumed run has to \
                    make the same calls to replay them. Vary a prompt by its index instead.";
        assert_eq!(tried, json!([gone, gone]));
    }

    #[test]
    fn a_frozen_built_in_can_still_be_given_an_object_of_its_own() {
        let data = Data::new("host-frozen");
        let seen = gave(ends(
            &data,
            r#"
            class Oops extends Error {
              constructor(message) {
                super(message);
                this.name = "Oops";
              }
            }
            const assigned = {};
            assigned.toString = () => "its own";
            let hijacked;
            try {
              Object.prototype.toString = () => "hijacked";
            } catch (error) {
              hijacked = error.message;
            }
            try { Array.prototype.map = () => []; } catch {}
            return {
              oops: String(new Oops("bad")),
              assigned: String(assigned),
              hijacked,
              plain: String({}),
              mapped: [1, 2].map((n) => n * 2),
            };
            "#,
            done,
        ));
        assert_eq!(
            seen,
            json!({
                "oops": "Oops: bad",
                "assigned": "its own",
                "hijacked": "toString is frozen.",
                "plain": "[object Object]",
                "mapped": [2, 4],
            })
        );
    }

    #[test]
    fn date_now_and_argless_date_throw_but_date_of_x_works() {
        let data = Data::new("host-date");
        let seen = gave(ends(
            &data,
            r#"
            const tried = [() => Date.now(), () => new Date(), () => Date(), () => Date(0)].map((call) => {
              try {
                call();
                return "ran";
              } catch (error) {
                return error.message;
              }
            });
            const day = new Date("2026-09-28T12:30:00Z");
            return {
              tried,
              iso: day.toISOString(),
              hours: day.getUTCHours(),
              parts: new Date(2026, 8, 28).getDate(),
              epoch: new Date(0).getTime(),
              same: day instanceof Date && Object.getPrototypeOf(day).constructor === Date,
            };
            "#,
            done,
        ));
        let gone = "Date.now() and new Date() are not available in workflow scripts: a resumed \
                    run has to make the same calls to replay them. Pass a date in through args \
                    instead.";
        assert_eq!(
            seen,
            json!({
                "tried": [gone, gone, gone, gone],
                "iso": "2026-09-28T12:30:00.000Z",
                "hours": 12,
                "parts": 28,
                "epoch": 0,
                "same": true,
            })
        );
    }

    #[test]
    fn eval_and_function_are_gone() {
        let data = Data::new("host-eval");
        let seen = gave(ends(
            &data,
            r#"
            const tries = {
              direct: () => eval("1 + 1"),
              indirect: () => (0, eval)("1 + 1"),
              called: () => Function("return 1")(),
              constructed: () => new Function("return 1")(),
              reached: () => (() => {}).constructor("return 1")(),
              asynchronous: () => (async () => {}).constructor("return 1"),
              generator: () => (function* () {}).constructor("yield 1"),
              asyncGenerator: () => Object.getPrototypeOf(async function* () {}).constructor("yield 1"),
            };
            const tried = {};
            for (const [name, call] of Object.entries(tries)) {
              try {
                tried[name] = call();
              } catch (error) {
                tried[name] = error.message;
              }
            }
            const present = ["WebAssembly", "setTimeout", "setInterval", "queueMicrotask", "WeakRef",
              "FinalizationRegistry", "performance", "require", "process", "std", "os"]
              .filter((name) => name in globalThis);
            let imported;
            try {
              await import("./elsewhere.js");
            } catch (error) {
              imported = error instanceof Error;
            }
            return { tried, present, imported, still: (() => {}) instanceof Function };
            "#,
            done,
        ));
        let gone = "eval() and the Function constructor are not available in workflow scripts.";
        assert_eq!(
            seen,
            json!({
                "tried": {
                    "direct": gone,
                    "indirect": gone,
                    "called": gone,
                    "constructed": gone,
                    "reached": gone,
                    "asynchronous": gone,
                    "generator": gone,
                    "asyncGenerator": gone,
                },
                "present": [],
                "imported": true,
                "still": true,
            })
        );
    }

    #[test]
    fn a_runaway_loop_is_stopped() {
        let data = Data::new("host-runaway");
        let turns = "The workflow script was stopped: it made more than 10000000 loop turns and \
                     calls without waiting for a helper. Look for a loop that never ends.";
        for body in [
            "while (true) {}",
            // After an await, the rest runs from the engine's job queue.
            "await agent(\"Read the notes.\");\nfor (;;) {}",
            // The limit is past a script's reach: catching it changes nothing.
            "for (;;) { try { while (true) {} } catch {} }",
            // However it is nested: no one loop here runs long.
            "const spin = () => { for (let n = 0; n < 1000; n++) {} };\n\
             for (;;) { try { spin(); } catch {} }",
            "for (;;) { await null; }",
        ] {
            let started = Instant::now();
            assert_eq!(ends(&data, body, done), End::Failed(turns.into()), "{body}");
            assert!(started.elapsed() < Duration::from_secs(30), "{body}");
        }
        // Each stretch stays under the turns, but together they run past the
        // engine time.
        let run = data.run(None, answering(&Asked::default(), done));
        let spin = "for (let n = 0; n < 1000; n++) {\n\
                    await agent(`Read part ${n}.`);\n\
                    for (let m = 0; m < 5000000; m++) {}\n\
                    }";
        let started = Instant::now();
        assert_eq!(
            ends_within(
                run,
                &format!("{META}{spin}"),
                nothing_saved(),
                Duration::from_millis(100)
            ),
            End::Failed(
                "The workflow script was stopped: its own code ran for more than 100 ms in all, \
                 not counting its waits for helpers. Look for a loop that never ends or heavy \
                 work between agent() calls."
                    .into()
            )
        );
        assert!(started.elapsed() < Duration::from_secs(30));
        // Calls that never end, and a heap that never stops growing, fail
        // on their own.
        let deeper = ends(&data, "const deeper = () => deeper();\ndeeper();", done);
        assert!(
            failed_with(&deeper, "The workflow script failed: "),
            "{deeper:?}"
        );
        assert!(
            format!("{deeper:?}").contains("Maximum call stack size exceeded"),
            "{deeper:?}"
        );
        let hoard = "const hoard = [];\n\
                     for (;;) hoard.push(String.fromCharCode(65 + hoard.length % 26).repeat(1 << 20));";
        let grown = ends(&data, hoard, done);
        assert!(format!("{grown:?}").contains("out of memory"), "{grown:?}");
    }

    #[test]
    fn a_script_nested_too_deep_fails_without_taking_the_process_down() {
        let data = Data::new("host-deep");
        let depth = 100_000;
        let (open, close) = ("[".repeat(depth), "]".repeat(depth));
        let end = ends(&data, &format!("return {open}1{close};"), done);
        assert!(failed_with(&end, "The workflow script failed: "), "{end:?}");
        assert!(
            format!("{end:?}").contains("Maximum call stack size exceeded"),
            "{end:?}"
        );
        let end = ends(&data, "return (;", done);
        assert!(
            failed_with(&end, "The workflow script failed: SyntaxError"),
            "{end:?}"
        );
    }

    #[test]
    fn meta_must_be_a_plain_literal_first() {
        let script = r#"// Finds things.
            /* Then reviews them. */
            export const meta = {
              name: "unwrap-audit",
              'description': `Find unwrap() calls
and "check" them`,
              phases: [{ title: "Find" }, { title: "Review", detail: null, }],
              estimate: { agents: 40, share: 0.5, big: 1_000, less: -2, tiny: 1e-3, strict: true },
              // Escapes read as JavaScript reads them.
              escapes: "tab\there é \u{1F600} \uD83D\uDE80 \x41 \'quoted\' back\\slash",
            } // It ends at the line break.
            return meta.name;"#;
        assert_eq!(
            meta(script),
            Ok(json!({
                "name": "unwrap-audit",
                "description": "Find unwrap() calls\nand \"check\" them",
                "phases": [{"title": "Find"}, {"title": "Review", "detail": null}],
                "estimate": {"agents": 40, "share": 0.5, "big": 1000, "less": -2, "tiny": 0.001, "strict": true},
                "escapes": "tab\there \u{e9} \u{1F600} \u{1F680} A 'quoted' back\\slash",
            }))
        );
        let refused = [
            "return 1;",
            "const x = 1;\nexport const meta = { name: \"a\", description: \"b\" };",
            "const meta = { name: \"a\", description: \"b\" };",
            "export let meta = { name: \"a\", description: \"b\" };",
            "export const meta = { name: \"a\" + \"b\", description: \"b\" };",
            "export const meta = { name: \"a\", description: (() => { throw 1; })() };",
            "const name = \"a\";\nexport const meta = { name, description: \"b\" };",
            "export const meta = { name: `a${1}`, description: \"b\" };",
            "export const meta = { [\"name\"]: \"a\", description: \"b\" };",
            "export const meta = { name: \"a\", description: \"b\" }, other = 1;",
            "export const meta = { name: \"a\", description: \"b\" }\n.name;",
            "export const meta = { name: \"a\", description: \"b\", phases: [1,,2] };",
            "export const meta = { name: \"a\", description: \"b\", size: 0x10 };",
            "export const meta = { name: \"\", description: \"b\" };",
            "export const meta = { name: \"a\", description: \"unended };",
            "export const meta = { name: \"a\", description: \"half \\uD83D a pair\" };",
            "export const meta = [\"a\", \"b\"];",
            "/* never closed export const meta = { name: \"a\", description: \"b\" };",
            "exported = 1;",
        ];
        for script in refused {
            assert_eq!(meta(script), Err(NO_META.into()), "{script}");
        }
        let deep = format!(
            "export const meta = {{ name: \"a\", description: \"b\", deep: {}1{} }};",
            "[".repeat(META_DEPTH + 1),
            "]".repeat(META_DEPTH + 1)
        );
        assert_eq!(meta(&deep), Err(NO_META.into()));
        let large = format!("{META}// {}", "x".repeat(SCRIPT_SIZE));
        assert!(meta(&large).is_err_and(|e| e.contains("more than the 512 KB")));

        // Refused before a thread starts, and the meta is the script's to read.
        let data = Data::new("host-meta");
        let run = data.run(None, answering(&Asked::default(), done));
        assert_eq!(
            start(run, "return 1;", &Value::Null, nothing_saved(), |_| {}),
            Err(NO_META.into())
        );
        let run = data.run(None, answering(&Asked::default(), done));
        assert_eq!(
            ends_within(run, script, nothing_saved(), ENGINE_TIME),
            End::Returned(json!("unwrap-audit"))
        );
    }

    #[test]
    fn parallel_gives_null_for_a_failed_slot() {
        let data = Data::new("host-parallel");
        let seen = gave(ends(
            &data,
            r#"
            const answers = await parallel(["Read a.rs.", "Fail on b.rs.", "Read c.rs."].map((task) => () => agent(task)));
            const thrown = await parallel([() => { throw new Error("no"); }, async () => 7, () => 8]);
            let promises;
            try {
              await parallel([Promise.resolve(1)]);
            } catch (error) {
              promises = error.message;
            }
            return { answers, thrown, promises };
            "#,
            failing,
        ));
        assert_eq!(
            seen,
            json!({
                "answers": ["Read a.rs. -> done", null, "Read c.rs. -> done"],
                "thrown": [null, 7, 8],
                "promises": "parallel() takes functions: parallel(items.map(x => () => agent(...)))",
            })
        );
        let log = std::fs::read_to_string(data.folder("wf-1").join("log.txt")).unwrap();
        assert_eq!(log, "parallel[0] failed: Error: no\n");
    }

    #[test]
    fn pipeline_stops_an_item_at_null() {
        let data = Data::new("host-pipeline");
        let seen = gave(ends(
            &data,
            r#"
            const reached = [];
            const out = await pipeline(["a", "b", "c", "d"],
              (file) => file === "b" ? null : agent(file === "d" ? "Fail on d." : `Read ${file}.`),
              (found, file, index) => {
                reached.push(file);
                if (file === "c") throw new Error("c is bad");
                return `${found} (${index})`;
              },
              (last, file) => {
                reached.push(`${file} last`);
                return last;
              });
            return { out, reached: reached.sort() };
            "#,
            failing,
        ));
        assert_eq!(
            seen,
            json!({
                "out": ["Read a. -> done (0)", null, null, null],
                "reached": ["a", "a last", "c"],
            })
        );
    }

    #[test]
    fn arrays_past_4096_are_refused() {
        let data = Data::new("host-arrays");
        let seen = gave(ends(
            &data,
            r#"
            const many = Array.from({ length: 4097 }, (_, n) => n);
            const tried = [];
            for (const call of [() => parallel(many.map((n) => () => n)), () => pipeline(many, (n) => n)]) {
              try {
                await call();
                tried.push("ran");
              } catch (error) {
                tried.push(error.message);
              }
            }
            const most = await pipeline(many.slice(1), (n) => n + 1);
            return { tried, most: most.length, last: most[4095] };
            "#,
            done,
        ));
        let too_many = |name| {
            format!("{name}() takes at most 4096 items at once and was given 4097: split the work.")
        };
        assert_eq!(
            seen,
            json!({
                "tried": [too_many("parallel"), too_many("pipeline")],
                "most": 4096,
                "last": 4097,
            })
        );
    }

    #[test]
    fn nesting_stops_at_one_level() {
        let data = Data::new("host-nesting");
        let asked = Asked::default();
        let run = data.run(None, answering(&asked, done));
        let find: Find = Box::new(|name| match name {
            "inner" => Ok(
                r#"export const meta = { name: "inner", description: "Nested once." };
                phase("Ignored");
                log(`reading ${args.file}`);
                const read = await agent(`Read ${args.file}.`, { phase: "Its own" });
                let deeper;
                try {
                    await workflow("inner", { file: "z.rs" });
                } catch (error) {
                    deeper = error.message;
                }
                return { read, deeper };"#
                    .into(),
            ),
            _ => Err(format!("There is no saved workflow \"{name}\".")),
        });
        let script = format!(
            "{META}{}",
            r#"phase("Find");
            const first = await agent("Read the notes.");
            const inner = await workflow("inner", { file: "b.rs" });
            let missing;
            try {
              await workflow("nowhere");
            } catch (error) {
              missing = error.message;
            }
            return { first, inner, missing, file: args.file };"#
        );
        let seen = gave(ends_within(run, &script, find, ENGINE_TIME));
        assert_eq!(
            seen,
            json!({
                "first": "Read the notes. -> done",
                "inner": {
                    "read": "Read b.rs. -> done",
                    "deeper": "workflow() cannot be called from within a nested workflow: nesting \
                               is limited to one level. Inline the inner script or call its \
                               agents directly.",
                },
                "missing": "There is no saved workflow \"nowhere\".",
                "file": "a.rs",
            })
        );
        // The nested workflow's agents are the run's, and show under its name.
        assert_eq!(*asked.lock().unwrap(), ["Read the notes.", "Read b.rs."]);
        let phases: Vec<Value> = data
            .journal("wf-1")
            .into_iter()
            .filter(|line| line["type"] == "started")
            .map(|line| line["phase"].clone())
            .collect();
        assert_eq!(phases, [json!("Find"), json!("▸ inner")]);
        let log = std::fs::read_to_string(data.folder("wf-1").join("log.txt")).unwrap();
        assert_eq!(log, "[inner] reading b.rs\n");
    }

    #[test]
    fn a_script_runs_its_agents_through_the_driver() {
        let data = Data::new("host-driver");
        // The prompt, not a word: the tools' own descriptions say "colour" too.
        let (endpoint, provider) = provider(2, |body| {
            if body.contains("Name a colour") {
                "Blue."
            } else {
                "A fig."
            }
        });
        let run = Run::open(
            None,
            &data.connection(&endpoint),
            "script",
            &json!({}),
            BUDGET,
            &crate::cancel::Cancel::default(),
        )
        .unwrap();
        let body = "const [colour, fruit] = await parallel([() => agent(\"Name a colour.\"), \
                    () => agent(\"Name a fruit.\")]);\nreturn { colour, fruit, spent: budget.spent() };";
        assert_eq!(
            ends_within(run, &format!("{META}{body}"), nothing_saved(), ENGINE_TIME),
            End::Returned(json!({"colour": "Blue.", "fruit": "A fig.", "spent": 2_100}))
        );
        assert_eq!(provider.join().unwrap(), 2);
    }

    #[test]
    fn a_resumed_script_gets_its_answers_back_on_the_next_turn() {
        let data = Data::new("host-resume");
        let body = format!(
            "{META}{}",
            r#"const order = [];
            const answered = agent("Read the notes.").then((notes) => order.push(notes));
            order.push("asked");
            await answered;
            return order;"#
        );
        let first = Asked::default();
        let run = data.run(None, answering(&first, done));
        let again = Asked::default();
        let resumed = || data.run(Some("wf-1"), answering(&again, done));
        let expected = End::Returned(json!(["asked", "Read the notes. -> done"]));
        assert_eq!(
            ends_within(run, &body, nothing_saved(), ENGINE_TIME),
            expected
        );
        assert_eq!(
            ends_within(resumed(), &body, nothing_saved(), ENGINE_TIME),
            expected
        );
        assert_eq!(first.lock().unwrap().len(), 1);
        assert!(again.lock().unwrap().is_empty());
    }

    #[test]
    fn a_paused_run_ends_its_script() {
        let data = Data::new("host-paused");
        let end = ends(
            &data,
            "await parallel([1, 2, 3, 4].map((n) => () => agent(`Fail on part ${n}.`)));\nreturn \"finished\";",
            failing,
        );
        assert_eq!(
            end,
            End::Paused(
                "3 helpers in a row failed with the same error: The provider is down.".into()
            )
        );
        // A spent budget pauses the run as the next agent() is refused, which
        // the script need not catch for the run to end paused.
        let mut run = data.run(None, answering(&Asked::default(), done));
        run.limit = 1_000;
        run.budget = Arc::new(Budget::new(1_000, None));
        let body = "await agent(\"Read the notes.\");\nawait agent(\"Read the logs.\");";
        assert_eq!(
            ends_within(run, &format!("{META}{body}"), nothing_saved(), ENGINE_TIME),
            End::Paused("its budget is spent (1k of 1k tokens)".into())
        );
        // A script that retries its nulls for ever after the pause is stopped
        // by the engine time, and still ends paused.
        let mut run = data.run(None, answering(&Asked::default(), done));
        run.limit = 1_000;
        run.budget = Arc::new(Budget::new(1_000, None));
        let body = "await agent(\"Read the notes.\");\n\
                    let got = [null];\n\
                    while (got.includes(null)) got = await parallel([() => agent(\"Read the logs.\")]);";
        assert_eq!(
            ends_within(
                run,
                &format!("{META}{body}"),
                nothing_saved(),
                Duration::from_millis(200)
            ),
            End::Paused("its budget is spent (1k of 1k tokens)".into())
        );
    }

    #[test]
    fn a_log_line_is_cut_so_logging_cannot_fill_the_disk() {
        let data = Data::new("host-log");
        let body = "const long = \"\u{e9}x\".repeat(1 << 20);\n\
                    for (let n = 0; n < 3; n++) log(long + long);\n\
                    return 1;";
        assert_eq!(ends(&data, body, done), End::Returned(json!(1)));
        let log = std::fs::read_to_string(data.folder("wf-1").join("log.txt")).unwrap();
        let cut = format!(
            "{} ... (cut: a log line keeps 4 KB)",
            // Cut where a character starts, not inside the é at byte 4,095.
            "\u{e9}x".repeat(LOG_LINE / 3)
        );
        assert_eq!(log, format!("{cut}\n{cut}\n{cut}\n"));
    }

    #[test]
    fn a_variable_made_without_let_still_works() {
        let data = Data::new("host-sloppy");
        let end = ends(&data, "count = 1;\ncount += 1;\nreturn count;", done);
        assert_eq!(end, End::Returned(json!(2)));
    }
}
