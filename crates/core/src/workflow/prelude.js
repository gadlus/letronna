// The workflow API as a script sees it, and the locks on everything else.
//
// The host evaluates this once per run, before any script, and calls the
// function it gives with its own: `queue` takes an agent() call and gives the
// number its answer comes back under, `compile` turns the saved workflow of a
// name into a script function, `write` keeps a line in the run's log, and
// `spent` says how many tokens the run has used of its `total`. What comes
// back is how the host starts a script and hands each answer in.
//
// What a resumed run could see differently -- Math.random, the clock -- is
// taken away first, and so is every way of turning text into code, as only
// the host compiles; then every built-in is frozen, so a script can neither
// put them back nor change what the API below relies on. Answers, arguments
// and results cross as JSON text, so a script shares no object with the host
// or with a workflow it nests. `parallel` and `pipeline` are only promises
// over agent(), so they live here rather than in the host.
(queue, compile, write, spent, total) => {
  "use strict";
  // Claude Code's cap on an array crossing into a workflow's calls.
  const MOST = 4096;
  const { parse, stringify } = JSON;
  const waiting = new Map();

  const gone = (what, instead) => () => {
    throw new Error(
      `${what} not available in workflow scripts: a resumed run has to make the same calls to replay them. ${instead}`,
    );
  };
  Math.random = gone("Math.random() is", "Vary a prompt by its index instead.");
  const clock = gone("Date.now() and new Date() are", "Pass a date in through args instead.");
  const RealDate = Date;
  // Called without new, Date ignores what it is given and reads the clock.
  function SafeDate(...values) {
    if (new.target === undefined || values.length === 0) clock();
    return Reflect.construct(RealDate, values, new.target);
  }
  Object.defineProperty(SafeDate, "name", { value: "Date" });
  SafeDate.prototype = RealDate.prototype;
  SafeDate.UTC = RealDate.UTC;
  SafeDate.parse = RealDate.parse;
  SafeDate.now = clock;
  Object.defineProperty(RealDate.prototype, "constructor", { value: SafeDate });
  globalThis.Date = SafeDate;
  const compiling = () => {
    throw new TypeError("eval() and the Function constructor are not available in workflow scripts.");
  };
  // Kept as a name, so instanceof Function still works.
  function SafeFunction() {
    compiling();
  }
  Object.defineProperty(SafeFunction, "name", { value: "Function" });
  SafeFunction.prototype = Function.prototype;
  for (const kind of [function () {}, async function () {}, function* () {}, async function* () {}]) {
    Object.defineProperty(Object.getPrototypeOf(kind), "constructor", { value: SafeFunction });
  }
  globalThis.Function = SafeFunction;
  globalThis.eval = compiling;
  // What the engine may have that can tell time, see the collector or wait.
  for (const name of [
    "WeakRef",
    "FinalizationRegistry",
    "Atomics",
    "SharedArrayBuffer",
    "WebAssembly",
    "performance",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "queueMicrotask",
  ]) {
    delete globalThis[name];
  }

  // Frozen, a built-in's property would also keep an object of the script's
  // own from taking that name by assignment -- this.name in an Error
  // subclass, toString on a plain object -- so the names scripts give their
  // own objects become accessors that let each object have its own.
  const errors = Reflect.ownKeys(globalThis)
    .map((name) => globalThis[name])
    .filter((kind) => kind === Error || kind?.prototype instanceof Error);
  const overridable = [
    [Object.prototype, Reflect.ownKeys(Object.prototype)],
    [Function.prototype, ["constructor", "toString"]],
    [Array.prototype, ["constructor", "toString"]],
    [Promise.prototype, ["constructor"]],
    ...errors.map((kind) => [kind.prototype, ["constructor", "message", "name", "toString"]]),
  ];
  for (const [object, keys] of overridable) {
    for (const key of keys) {
      const property = Reflect.getOwnPropertyDescriptor(object, key);
      if (property === undefined || !("value" in property)) continue;
      const { value } = property;
      Object.defineProperty(object, key, {
        get() {
          return value;
        },
        set(own) {
          if (this === object) throw new TypeError(`${String(key)} is frozen.`);
          Object.defineProperty(this, key, { value: own, writable: true, enumerable: true, configurable: true });
        },
      });
    }
  }

  const frozen = new WeakSet();
  const freeze = (value) => {
    if (Object(value) !== value || frozen.has(value)) return;
    frozen.add(value);
    Object.freeze(value);
    for (const key of Reflect.ownKeys(value)) {
      const property = Reflect.getOwnPropertyDescriptor(value, key);
      freeze(property.value);
      freeze(property.get);
      freeze(property.set);
    }
    freeze(Reflect.getPrototypeOf(value));
  };
  // The global object itself stays open, so a sloppy `count = 0` without a
  // let still makes a variable rather than vanishing; the built-ins' names on
  // it are pinned below.
  frozen.add(globalThis);
  for (const name of Reflect.ownKeys(globalThis)) {
    const property = Reflect.getOwnPropertyDescriptor(globalThis, name);
    freeze(property.value);
    if ("value" in property) {
      Object.defineProperty(globalThis, name, { writable: false, configurable: false });
    }
  }

  const text = (value) => {
    if (typeof value === "string") return value;
    try {
      return value instanceof Error ? String(value) : (stringify(value) ?? String(value));
    } catch {
      return Object.prototype.toString.call(value);
    }
  };
  const check = (name, items) => {
    if (!Array.isArray(items)) throw new TypeError(`${name}() takes an array.`);
    if (items.length > MOST) {
      throw new RangeError(
        `${name}() takes at most ${MOST} items at once and was given ${items.length}: split the work.`,
      );
    }
  };
  const budget = Object.freeze({
    total,
    spent: () => spent(),
    remaining: () => Math.max(0, total - spent()),
  });

  // The API of the top-level script, or of the workflow it nested by `nested`.
  const api = (nested) => {
    let current;
    const log =
      nested === undefined
        ? (message) => write(text(message))
        : (message) => write(`[${nested}] ${text(message)}`);
    const agent = (prompt, options) =>
      new Promise((resolve) => {
        if (typeof prompt !== "string") throw new TypeError("agent() takes its task as a string.");
        let asked = options ?? {};
        if (typeof asked === "object") {
          // A nested workflow's agents show under its name, whatever phase they give.
          const phase = nested === undefined ? (asked.phase ?? current) : `▸ ${nested}`;
          asked = { ...asked, phase };
        }
        waiting.set(queue(prompt, stringify(asked)), resolve);
      });
    const parallel = async (thunks) => {
      check("parallel", thunks);
      if (!thunks.every((thunk) => typeof thunk === "function")) {
        throw new TypeError("parallel() takes functions: parallel(items.map(x => () => agent(...)))");
      }
      return Promise.all(
        thunks.map(async (thunk, index) => {
          try {
            return await thunk();
          } catch (error) {
            log(`parallel[${index}] failed: ${text(error)}`);
            return null;
          }
        }),
      );
    };
    const pipeline = async (items, ...stages) => {
      check("pipeline", items);
      if (!stages.every((stage) => typeof stage === "function")) {
        throw new TypeError("pipeline() takes the items, then a function for each stage.");
      }
      return Promise.all(
        items.map(async (item, index) => {
          let value = item;
          for (const stage of stages) {
            if (value === null) break;
            try {
              value = await stage(value, item, index);
            } catch (error) {
              log(`pipeline[${index}] failed: ${text(error)}`);
              value = null;
            }
          }
          return value;
        }),
      );
    };
    const phase =
      nested === undefined
        ? (title) => {
            current = String(title);
          }
        : () => {};
    const workflow =
      nested === undefined
        ? async (name, args) => parse(await run(compile(String(name)), String(name), stringify(args ?? null)))
        : async () => {
            throw new Error(
              "workflow() cannot be called from within a nested workflow: nesting is limited to one level. Inline the inner script or call its agents directly.",
            );
          };
    const console = Object.freeze({ log, info: log, warn: log, error: log });
    return { agent, parallel, pipeline, phase, log, workflow, console };
  };

  // Runs the script function `make` gives, as the top level or as the workflow
  // `nested`, with `args` as JSON text, to its result as JSON text.
  const run = async (make, nested, args) => {
    const body = make({ ...api(nested), args: parse(args), budget });
    return stringify(await body()) ?? "null";
  };
  const settle = (slot, answer) => {
    const resolve = waiting.get(slot);
    waiting.delete(slot);
    resolve?.(parse(answer));
  };
  return { run, settle };
}
