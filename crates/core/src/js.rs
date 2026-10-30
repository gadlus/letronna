//! The JavaScript engine everything here that runs a script shares: QuickJS,
//! through rquickjs, held to a heap, a stack and a question now and then
//! whether to go on.
//!
//! Workflows, the `script` tool and plugins each make an engine of their own
//! from here, on a thread of their own, as a context never leaves the thread
//! that made it. What differs between them -- how long a script may run, how
//! much it may hold, what the world it sees has in it -- is theirs to set.

use rquickjs::context::{EvalOptions, intrinsic};
use rquickjs::{Context, Ctx, Runtime};

/// The heap a workflow's script may use: far past a run's answers, which the
/// journal keeps anyway, and far short of what would squeeze the machine.
pub(crate) const HEAP: usize = 256 * 1024 * 1024;
/// The stack the engine may use, QuickJS's own default: calls a few thousand
/// deep, and a script nested past that fails to parse rather than overflow.
const JS_STACK: usize = 1024 * 1024;
/// The script thread's stack: the engine's, with room for the host around it.
pub(crate) const STACK: usize = 8 * 1024 * 1024;

/// An engine held to the workflows' heap and the stack limit, asking `stop`
/// now and then while a script runs whether to stop it, with a context of
/// its own.
pub(crate) fn engine(stop: impl FnMut() -> bool + 'static) -> Result<(Runtime, Context), String> {
    engine_holding(HEAP, stop)
}

/// `engine`, held to `heap` bytes instead.
pub(crate) fn engine_holding(
    heap: usize,
    stop: impl FnMut() -> bool + 'static,
) -> Result<(Runtime, Context), String> {
    let engine = Runtime::new().and_then(|runtime| {
        runtime.set_memory_limit(heap);
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

/// How a script is evaluated: as global code, not strict unless it says so,
/// named `file` where its errors say where they were.
pub(crate) fn options(file: &str) -> EvalOptions {
    // Made from the default, as the engine may add fields to it.
    let mut options = EvalOptions::default();
    options.strict = false;
    options.filename = Some(file.into());
    options
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
