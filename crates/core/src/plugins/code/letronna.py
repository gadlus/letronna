"""Letronna's API for plugins written in Python.

A plugin's main file imports this and registers what it handles; Letronna
calls the handlers when it needs them:

    import letronna

    @letronna.tool("count")
    def count(args, ctx):
        return str(len(args["text"].split()))

    @letronna.on("turn.end")
    def ended(turn):
        letronna.status("done")

The names match the JavaScript API, in Python's spelling. A handler may be
a plain function or an `async def`; what a tool or an action gives back is
sent as it is when it is text, and as JSON otherwise. Events are handled in
the order they came, one at a time; tools, actions and hooks run beside
them, a few at once. Everything here is Python's own library.
"""

import asyncio
import inspect
import json
import os
import queue
import sys
import threading
import traceback
from concurrent.futures import ThreadPoolExecutor

__all__ = [
    "plugin",
    "data_dir",
    "settings",
    "storage",
    "ui",
    "on",
    "tool",
    "action",
    "status",
    "toast",
    "log",
]

plugin = os.environ.get("LETRONNA_PLUGIN", "")
"""This plugin's name."""
data_dir = os.environ.get("LETRONNA_PLUGIN_DATA", "")
"""A folder of this plugin's own, for whatever it keeps."""

# The events Letronna sends whatever plugin.json lists: the plugin's own.
_OWN_EVENTS = {"settings.change", "panel.open"}
# What one plugin's storage may hold, as Letronna's JavaScript plugins.
_STORAGE_BYTES = 1024 * 1024

_out = None
_writing = threading.Lock()
_declared = set()
_listeners = {}
_tools = {}
_actions = {}
_inputs = {}
_cancelled = {}
_events = queue.Queue()
_pool = ThreadPoolExecutor(max_workers=4, thread_name_prefix="letronna")
_loop = None
_loop_made = threading.Lock()


def _send(message):
    line = json.dumps(message, ensure_ascii=False, default=str)
    with _writing:
        _out.write(line + "\n")
        _out.flush()


class _Settings:
    """The plugin's settings as the user set them, read-only here."""

    def __init__(self):
        self._values = {}

    def get(self, key, default=None):
        return self._values.get(key, default)

    def all(self):
        return dict(self._values)

    def __getitem__(self, key):
        return self._values[key]

    def __contains__(self, key):
        return key in self._values


class _Storage:
    """A small store of the plugin's own, kept in its data folder."""

    def __init__(self):
        self._lock = threading.Lock()
        self._values = None

    def _file(self):
        return os.path.join(data_dir, "storage.json")

    def _loaded(self):
        if self._values is None:
            try:
                with open(self._file(), encoding="utf-8") as kept:
                    values = json.load(kept)
                self._values = values if isinstance(values, dict) else {}
            except (OSError, ValueError):
                self._values = {}
        return self._values

    def _save(self, values):
        text = json.dumps(values, ensure_ascii=False)
        if len(text.encode("utf-8")) > _STORAGE_BYTES:
            raise ValueError("A plugin's storage holds at most 1 MB.")
        os.makedirs(data_dir, exist_ok=True)
        staged = self._file() + ".new"
        with open(staged, "w", encoding="utf-8") as out:
            out.write(text)
        os.replace(staged, self._file())

    def get(self, key, default=None):
        with self._lock:
            return self._loaded().get(key, default)

    def set(self, key, value):
        with self._lock:
            values = dict(self._loaded())
            values[key] = value
            self._save(values)
            self._values = values

    def delete(self, key):
        with self._lock:
            values = dict(self._loaded())
            if values.pop(key, None) is not None:
                self._save(values)
                self._values = values

    def all(self):
        with self._lock:
            return dict(self._loaded())


settings = _Settings()
storage = _Storage()


class Context:
    """Where a tool or an action was called: the conversation and its
    folder, and whether Letronna stopped waiting for the answer."""

    def __init__(self, given, cancelled):
        self.conversation = given.get("conversation", 0)
        self.workspace = given.get("workspace")
        self.cancelled = cancelled


def _keep(kind, into):
    def register(name):
        if not isinstance(name, str):
            raise TypeError("letronna.%s(name) takes a name" % kind)

        def keep(handler):
            into[name] = handler
            return handler

        return keep

    return register


def on(event):
    """Handles `event`, one plugin.json lists in contributes.events."""

    def register(handler):
        if event not in _OWN_EVENTS and _declared and event not in _declared:
            _send({
                "do": "error",
                "text": 'letronna.on("%s") will hear nothing: plugin.json does not list it '
                "in contributes.events." % event,
            })
        _listeners.setdefault(event, []).append(handler)
        return handler

    return register


tool = _keep("tool", _tools)
tool.__doc__ = "Handles the tool `name`, one plugin.json lists in contributes.tools."
action = _keep("action", _actions)
action.__doc__ = "Handles the command `/name`, one plugin.json lists in contributes.actions."


class _UI:
    def panel(self, tree):
        """Shows `tree` as the plugin's panel in the window."""
        _send({"do": "ui", "surface": "panel", "tree": tree})

    def on_action(self, id):
        """Handles the user pressing, flipping or typing into the part `id`."""
        return _keep("ui.on_action", _inputs)(id)


ui = _UI()


def status(text):
    """Shows `text` as the plugin's status; empty shows none."""
    _send({"do": "status", "text": "" if text is None else str(text)})


def toast(text, kind="info"):
    """Puts up a notice: kind is info, warning or error."""
    _send({"do": "toast", "text": str(text), "kind": str(kind)})


def log(*values):
    """Writes a line to the plugin's log."""
    _send({"do": "log", "text": " ".join(str(value) for value in values)})


def _failed(what):
    _send({"do": "error", "text": "%s failed: %s" % (what, traceback.format_exc().strip())})


def _async_loop():
    global _loop
    with _loop_made:
        if _loop is None:
            _loop = asyncio.new_event_loop()
            threading.Thread(target=_loop.run_forever, name="letronna-async", daemon=True).start()
        return _loop


def _call(handler, *args):
    """Calls `handler` with as many of `args` as it takes, waiting for it
    when it is an async function."""
    try:
        parameters = inspect.signature(handler).parameters.values()
        takes_all = any(p.kind == p.VAR_POSITIONAL for p in parameters)
        positional = [p for p in parameters if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)]
        if not takes_all:
            args = args[: len(positional)]
    except (TypeError, ValueError):
        pass
    result = handler(*args)
    if inspect.isawaitable(result):
        result = asyncio.run_coroutine_threadsafe(_awaited(result), _async_loop()).result()
    return result


async def _awaited(awaitable):
    return await awaitable


def _hear(event, data):
    answers = []
    for handler in list(_listeners.get(event, [])):
        try:
            answers.append(_call(handler, data))
        except Exception:
            _failed("Its %s handler" % event)
    return answers


def _verdict(answers):
    block = None
    notes = []
    for answer in answers:
        if answer is None or answer is False:
            continue
        if isinstance(answer, str):
            notes.append(answer)
        elif isinstance(answer, dict):
            if answer.get("block") is not None and block is None:
                block = str(answer["block"])
            if answer.get("note") is not None:
                notes.append(str(answer["note"]))
    if block is not None:
        return {"block": block}
    return {"note": "\n".join(notes)} if notes else {}


def _answer(id, handler, *args):
    try:
        value = _call(handler, *args)
        _send({"reply": id, "ok": "" if value is None else value})
    except Exception as error:
        traceback.print_exc()
        _send({"reply": id, "error": "%s: %s" % (type(error).__name__, error)})
    finally:
        _cancelled.pop(id, None)


def _requested(message, handlers, what):
    id = message.get("id")
    handler = handlers.get(message.get("name"))
    if handler is None:
        _send({"reply": id, "error": "The plugin has no handler for %s." % what})
        return
    cancelled = threading.Event()
    _cancelled[id] = cancelled
    context = Context(message.get("ctx") or {}, cancelled)
    _pool.submit(_answer, id, handler, message.get("args"), context)


def _hook(message):
    _send({"reply": message.get("id"), **_verdict(_hear(message.get("event"), message.get("data")))})


def _ui(message):
    handler = _inputs.get(message.get("id"))
    if handler is None:
        return
    try:
        _call(handler, message.get("value"))
    except Exception:
        _failed("Its %s control" % message.get("id"))


def _in_order():
    while True:
        event, data = _events.get()
        _hear(event, data)


def _start(out, hello):
    global _out, plugin, data_dir
    _out = out
    plugin = hello.get("plugin", plugin)
    data_dir = hello.get("data", data_dir)
    _declared.update(hello.get("events") or [])
    settings._values = dict(hello.get("settings") or {})
    threading.Thread(target=_in_order, name="letronna-events", daemon=True).start()


def _serve(stdin):
    _send({"do": "ready"})
    for line in stdin:
        try:
            message = json.loads(line.decode("utf-8"))
        except ValueError:
            continue
        kind = message.get("type")
        if kind == "event":
            _events.put((message.get("event"), message.get("data")))
        elif kind == "hook":
            _pool.submit(_hook, message)
        elif kind == "tool":
            _requested(message, _tools, "its tool %s" % message.get("name"))
        elif kind == "action":
            _requested(message, _actions, "/%s" % message.get("name"))
        elif kind == "ui":
            _pool.submit(_ui, message)
        elif kind == "ui.open":
            _events.put(("panel.open", {}))
        elif kind == "settings.change":
            settings._values[message.get("key")] = message.get("value")
            _events.put(("settings.change", {"key": message.get("key"), "value": message.get("value")}))
        elif kind == "cancel":
            cancelled = _cancelled.get(message.get("id"))
            if cancelled is not None:
                cancelled.set()
        elif kind == "stop":
            return
