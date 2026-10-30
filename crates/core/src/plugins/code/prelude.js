// The world a plugin's JavaScript runs in: the `letronna` object, console,
// timers, fetch and require, built over the few natives the host gives.
// Everything crosses to the host as JSON text, so nothing a plugin holds
// reaches past its own engine. The host calls what this returns: `main` once
// with the plugin's file, then `receive` with each message for it.
(natives, hello) => {
  "use strict";
  const { parse, stringify } = JSON;
  const send = (message) => natives.post(stringify(message));
  const text = (value) => {
    if (typeof value === "string") return value;
    if (value instanceof Error) return value.stack ? `${value}\n${value.stack}` : String(value);
    try {
      return stringify(value) ?? String(value);
    } catch {
      return String(value);
    }
  };
  const started = parse(hello);
  const declared = new Set(started.events ?? []);
  let settings = started.settings ?? {};
  let store = started.storage ?? {};

  const listeners = new Map();
  const tools = new Map();
  const actions = new Map();
  const inputs = new Map();

  // A handler's failure, told to the user through the host, which keeps
  // them from filling the screen.
  const failed = (what, error) => send({ do: "error", text: `${what} failed: ${text(error)}` });
  // A handler's result as a promise, so a throw and a rejection are one.
  const called = (handler, ...args) => new Promise((resolve) => resolve(handler(...args)));
  const keep = (kind, into) => (name, handler) => {
    if (typeof name !== "string" || typeof handler !== "function") {
      throw new TypeError(`letronna.${kind}(name, handler) takes a name and a function`);
    }
    into.set(name, handler);
  };

  // Always heard, whatever plugin.json lists: the plugin's own.
  const OWN_EVENTS = new Set(["settings.change", "panel.open"]);
  const on = (event, handler) => {
    if (typeof event !== "string" || typeof handler !== "function") {
      throw new TypeError("letronna.on(event, handler) takes an event's name and a function");
    }
    if (!OWN_EVENTS.has(event) && !declared.has(event)) {
      send({
        do: "error",
        text: `letronna.on("${event}") will hear nothing: plugin.json does not list it in contributes.events.`,
      });
    }
    const list = listeners.get(event) ?? [];
    list.push(handler);
    listeners.set(event, list);
  };

  const timers = new Map();
  let nextTimer = 1;
  const later = (repeat) => (handler, ms = 0, ...args) => {
    if (typeof handler !== "function") throw new TypeError("A timer needs a function to call.");
    const id = nextTimer++;
    natives.timer(id, Number(ms) || 0, repeat);
    timers.set(id, { handler, args, repeat });
    return id;
  };
  const clear = (id) => {
    if (timers.delete(id)) natives.untimer(id);
  };

  const fetches = new Map();
  let nextFetch = 1;
  const response = (answer) =>
    Object.freeze({
      ok: answer.status >= 200 && answer.status < 300,
      status: answer.status,
      headers: Object.freeze({ ...(answer.headers ?? {}) }),
      text: async () => answer.body,
      json: async () => parse(answer.body),
    });

  const letronna = Object.freeze({
    plugin: started.plugin,
    dataDir: started.data,
    on,
    tool: keep("tool", tools),
    action: keep("action", actions),
    ui: Object.freeze({
      panel(tree) {
        send({ do: "ui", surface: "panel", tree });
      },
      onAction: keep("ui.onAction", inputs),
    }),
    status(line) {
      send({ do: "status", text: line == null ? "" : String(line) });
    },
    toast(line, kind = "info") {
      send({ do: "toast", text: String(line), kind: String(kind) });
    },
    log(...values) {
      send({ do: "log", text: values.map(text).join(" ") });
    },
    settings: Object.freeze({
      get: (key) => settings[key],
      all: () => ({ ...settings }),
    }),
    storage: Object.freeze({
      get: (key) => store[key],
      set(key, value) {
        const had = Object.prototype.hasOwnProperty.call(store, key);
        const before = store[key];
        store[key] = value;
        if (!natives.save(stringify(store))) {
          if (had) store[key] = before;
          else delete store[key];
          throw new Error("A plugin's storage holds at most 1 MB.");
        }
      },
      delete(key) {
        if (Object.prototype.hasOwnProperty.call(store, key)) {
          delete store[key];
          natives.save(stringify(store));
        }
      },
      all: () => ({ ...store }),
    }),
    fetch(url, options = {}) {
      return new Promise((resolve, reject) => {
        const id = nextFetch++;
        fetches.set(id, { resolve, reject });
        try {
          natives.fetch(
            id,
            stringify({
              url: String(url),
              method: String(options.method ?? "GET").toUpperCase(),
              headers: options.headers ?? {},
              body: options.body == null ? null : String(options.body),
            }),
          );
        } catch (error) {
          fetches.delete(id);
          reject(error);
        }
      });
    },
  });
  const log = (...values) => letronna.log(...values);
  const console = Object.freeze({ log, info: log, warn: log, error: log, debug: log });
  Object.assign(globalThis, {
    letronna,
    console,
    setTimeout: later(false),
    setInterval: later(true),
    clearTimeout: clear,
    clearInterval: clear,
  });

  // CommonJS, over the plugin's own files only.
  const modules = new Map();
  const folderOf = (path) => (path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "");
  const resolve = (from, wanted) => {
    if (typeof wanted !== "string" || !(wanted.startsWith("./") || wanted.startsWith("../"))) {
      throw new Error(`require("${wanted}"): a plugin requires its own files, by a path starting ./ or ../`);
    }
    const parts = from ? from.split("/") : [];
    for (const part of wanted.split("/")) {
      if (part === "" || part === ".") continue;
      if (part === "..") {
        if (!parts.length) throw new Error(`require("${wanted}") reaches outside the plugin`);
        parts.pop();
      } else {
        parts.push(part);
      }
    }
    return parts.join("/");
  };
  const load = (path) => {
    for (const file of [path, `${path}.js`, `${path}/index.js`, `${path}.json`]) {
      const known = modules.get(file);
      if (known) return known.exports;
      const make = natives.load(file);
      if (!make) continue;
      const module = { exports: {} };
      modules.set(file, module);
      const require = (wanted) => load(resolve(folderOf(file), wanted));
      make(module.exports, require, module, file, folderOf(file));
      return module.exports;
    }
    throw new Error(`require: there is no ${path} in the plugin`);
  };

  const reply = (id, answer) => send({ reply: id, ...answer });
  const answered = (id, promise) =>
    promise.then(
      (value) => reply(id, { ok: value === undefined ? "" : value }),
      (error) => reply(id, { error: error instanceof Error ? String(error) : text(error) }),
    );
  // Every handler of `event`, one after another, with what each gave back.
  const hear = async (event, data) => {
    const answers = [];
    for (const handler of listeners.get(event) ?? []) {
      try {
        answers.push(await handler(data));
      } catch (error) {
        failed(`Its ${event} handler`, error);
      }
    }
    return answers;
  };
  // What the handlers of a hook gave back, as one answer: the first block,
  // or every note.
  const verdict = (answers) => {
    let block = null;
    const notes = [];
    for (const answer of answers) {
      if (answer == null || answer === false) continue;
      if (typeof answer === "string") notes.push(answer);
      else if (typeof answer === "object") {
        if (answer.block != null && block == null) block = String(answer.block);
        if (answer.note != null) notes.push(String(answer.note));
      }
    }
    if (block != null) return { block };
    return notes.length ? { note: notes.join("\n") } : {};
  };

  const receive = (line) => {
    const message = parse(line);
    switch (message.type) {
      case "event":
        hear(message.event, message.data);
        break;
      case "hook":
        hear(message.event, message.data).then((answers) => reply(message.id, verdict(answers)));
        break;
      case "tool": {
        const handler = tools.get(message.name);
        if (!handler) {
          reply(message.id, { error: `The plugin has no handler for its tool ${message.name}.` });
        } else {
          answered(message.id, called(handler, message.args ?? {}, message.ctx ?? {}));
        }
        break;
      }
      case "action": {
        const handler = actions.get(message.name);
        if (!handler) {
          reply(message.id, { error: `The plugin has no handler for /${message.name}.` });
        } else {
          answered(message.id, called(handler, message.args ?? "", message.ctx ?? {}));
        }
        break;
      }
      case "ui": {
        const handler = inputs.get(message.id);
        if (handler) called(handler, message.value).catch((error) => failed(`Its ${message.id} control`, error));
        break;
      }
      case "ui.open":
        hear("panel.open", {});
        break;
      case "settings.change":
        settings = { ...settings, [message.key]: message.value };
        hear("settings.change", { key: message.key, value: message.value });
        break;
      case "timer": {
        const timer = timers.get(message.id);
        if (!timer) break;
        if (!timer.repeat) timers.delete(message.id);
        try {
          timer.handler(...timer.args);
        } catch (error) {
          failed("A timer", error);
        }
        break;
      }
      case "fetched": {
        const waiting = fetches.get(message.id);
        if (!waiting) break;
        fetches.delete(message.id);
        if (message.error != null) waiting.reject(new Error(message.error));
        else waiting.resolve(response(message));
        break;
      }
      default:
        break;
    }
  };

  return { receive, main: (path) => void load(path) };
};
