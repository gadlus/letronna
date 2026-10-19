"""A relay between Harbor's task containers and the model provider, on the host.

Letronna in a task container calls it over plain HTTP on the Docker bridge
(`LETRONNA_ALLOW_HTTP=1` admits `host.docker.internal`), and it calls the
provider over HTTPS with the host's trust store. So:

- the key stays on the host, out of every container and trial log;
- a network that re-terminates TLS with its own CA, which Letronna's built-in
  roots do not hold, still reaches the provider;
- like OpenHuman's metering proxy, it can pin the model, the provider and the
  reasoning effort on every call, and it writes one meter line per call with
  the usage the provider reported, and the charge where the upstream sends
  one in a response header.

    OPENROUTER_API_KEY=... python3 relay.py --meter meter.jsonl \
        --provider DeepSeek --reasoning high

With --stub it needs no key and answers as a canned model: a run_command
call that prints Letronna's LETRONNA_ variables, then a closing message,
enough to show a trial's plumbing works end to end.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import ssl
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STUB_COMMAND = "env | grep -E '^LETRONNA_' | sort; uname -a"


class Relay:
    def __init__(self, args: argparse.Namespace, key: str) -> None:
        self.args = args
        self.key = key
        self.lock = threading.Lock()
        self.ssl = ssl.create_default_context()

    def meter(self, row: dict) -> None:
        with self.lock, open(self.args.meter, "a", encoding="utf8") as f:
            f.write(json.dumps(row) + "\n")

    def pinned(self, body: dict) -> dict:
        """The request as sent upstream: the pins applied, and usage asked for."""
        if self.args.model:
            body["model"] = self.args.model
        openrouter = self.args.style == "openrouter"
        if self.args.provider and openrouter:
            body["provider"] = {"order": [self.args.provider], "allow_fallbacks": False}
        # OpenRouter's own field, or OpenAI's, which other gateways take.
        if self.args.reasoning and openrouter:
            body["reasoning"] = {"effort": self.args.reasoning}
            body.pop("reasoning_effort", None)
        elif self.args.reasoning:
            body["reasoning_effort"] = self.args.reasoning
            body.pop("reasoning", None)
        if body.get("stream"):
            body.setdefault("stream_options", {})["include_usage"] = True
        if openrouter:
            body["usage"] = {"include": True}
        return body


def usage_of(captured: bytes) -> dict | None:
    """The usage a reply reported: the last chunk's that carried one when streamed."""
    found = None
    for line in captured.split(b"\n"):
        line = line.strip()
        if line.startswith(b"data:") and b'"usage"' in line:
            try:
                chunk = json.loads(line[5:].strip())
            except ValueError:
                continue
            if isinstance(chunk, dict) and chunk.get("usage"):
                found = chunk["usage"]
    if found is None:
        try:
            found = json.loads(captured).get("usage")
        except (ValueError, AttributeError):
            pass
    return found


def charged(headers) -> float | None:
    """What the upstream says the call cost: a response header whose name ends in -cost."""
    for name, value in headers.items():
        if name.lower().endswith("-cost"):
            try:
                cost = float(value.strip().lstrip("$").split()[0])
            except (ValueError, IndexError):
                continue
            if math.isfinite(cost):
                return cost
    return None


def stub_reply(body: dict, call: str = "") -> dict:
    """A run_command call while no tool result is in, then a closing message."""
    tools = {t.get("function", {}).get("name") for t in body.get("tools") or []}
    ran = any(m.get("role") == "tool" for m in body.get("messages") or [])
    if not ran and "run_command" in tools:
        call = {
            "index": 0,
            "id": "call_stub_1",
            "type": "function",
            "function": {"name": "run_command", "arguments": call or json.dumps({"command": STUB_COMMAND})},
        }
        return {"tool_calls": [call], "finish": "tool_calls"}
    return {"content": "Done: the stub ran one command.", "finish": "stop"}


def handler(relay: Relay):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args) -> None:
            pass

        def route(self) -> tuple[str, str]:
            """A /t/<tag>/ prefix, which tells concurrent trials apart in the meter, and the rest."""
            m = re.match(r"^/t/([^/]+)(/.*)$", self.path)
            return (m.group(1), m.group(2)) if m else ("", self.path)

        def do_GET(self) -> None:
            tag, path = self.route()
            if relay.args.stub:
                listing = {"data": [{"id": "stub-model", "context_length": 128000}]}
                return self.whole(200, json.dumps(listing).encode())
            self.forward("GET", path, None, tag)

        def do_POST(self) -> None:
            tag, path = self.route()
            raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
            try:
                body = json.loads(raw) if raw else None
            except ValueError:
                body = None
            if relay.args.stub:
                return self.stub(body or {}, tag)
            if isinstance(body, dict) and path.rstrip("/").endswith("/chat/completions"):
                raw = json.dumps(relay.pinned(body)).encode()
            self.forward("POST", path, raw, tag)

        def whole(self, code: int, data: bytes, kind: str = "application/json") -> None:
            self.send_response(code)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def chunk(self, data: bytes) -> None:
            self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            self.wfile.flush()

        def stub(self, body: dict, tag: str) -> None:
            reply = stub_reply(body, relay.args.stub_call)
            usage = {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}
            relay.meter({"t": time.time(), "tag": tag, "stub": True, "usage": usage})
            model = body.get("model") or "stub-model"
            if not body.get("stream"):
                message = {"role": "assistant", "content": reply.get("content")}
                if reply.get("tool_calls"):
                    message["tool_calls"] = [
                        {k: v for k, v in c.items() if k != "index"} for c in reply["tool_calls"]
                    ]
                choice = {"index": 0, "message": message, "finish_reason": reply["finish"]}
                answer = {"id": "stub", "model": model, "choices": [choice], "usage": usage}
                return self.whole(200, json.dumps(answer).encode())
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            delta = {"role": "assistant"}
            for key in ("content", "tool_calls"):
                if reply.get(key):
                    delta[key] = reply[key]
            chunks = [
                {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": reply["finish"]}]},
                {"choices": [], "usage": usage},
            ]
            for c in chunks:
                c.update({"id": "stub", "object": "chat.completion.chunk", "model": model})
                self.chunk(b"data: " + json.dumps(c).encode() + b"\n\n")
            self.chunk(b"data: [DONE]\n\n")
            self.chunk(b"")

        def forward(self, method: str, path: str, raw: bytes | None, tag: str) -> None:
            url = relay.args.upstream.rstrip("/") + re.sub(r"^/v1(?=/|$)", "", path)
            headers = {
                "Content-Type": "application/json",
                "Authorization": f"Bearer {relay.key}",
                # The caller's, which is Letronna's: some gateways turn away
                # Python's own as a bot's.
                "User-Agent": self.headers.get("User-Agent") or "letronna-tb2-relay/1",
            }
            request = urllib.request.Request(url, data=raw, method=method, headers=headers)
            started = time.time()
            first = None
            status = 0
            cost = None
            captured = bytearray()
            cut = None
            try:
                try:
                    response = urllib.request.urlopen(
                        request, timeout=relay.args.read_timeout, context=relay.ssl
                    )
                except urllib.error.HTTPError as error:
                    response = error
                status = response.status
                cost = charged(response.headers)
                self.send_response(status)
                self.send_header("Content-Type", response.headers.get("Content-Type", "application/json"))
                self.send_header("Transfer-Encoding", "chunked")
                self.end_headers()
                while True:
                    try:
                        piece = response.read1(65536)
                    except Exception as error:
                        # The provider's side broke off: Letronna's is cut
                        # too, with no last chunk, so it asks again.
                        cut = f"upstream: {type(error).__name__}: {error}"
                        break
                    if not piece:
                        # An end with bytes still owed, or a stream that never
                        # said [DONE], broke off too, though no read failed.
                        owed = getattr(response, "length", None)
                        streamed = "event-stream" in response.headers.get("Content-Type", "")
                        if owed:
                            cut = f"upstream: closed {owed} bytes short"
                        elif (
                            streamed
                            and b"[DONE]" not in captured[-200:]
                            and b'"finish_reason":"' not in captured[-4000:]
                        ):
                            cut = "upstream: the stream ended with no [DONE] or finish"
                        break
                    first = first or time.time()
                    # Usage comes at the end; the start is kept for errors.
                    captured += piece
                    if len(captured) > 4_000_000:
                        del captured[2_000:-1_000_000]
                    try:
                        self.chunk(piece)
                    except OSError as error:
                        cut = f"client: {type(error).__name__}"
                        break
                if cut is None:
                    self.chunk(b"")
            except Exception as error:  # unreachable, timed out, or hung up
                if status == 0:
                    # No answer began: the provider could not be reached or
                    # hung up first, as http.client's RemoteDisconnected says.
                    cut = f"upstream: {type(error).__name__}: {error}"
                    try:
                        message = {"error": {"message": f"relay: {error}"}}
                        self.whole(502, json.dumps(message).encode())
                    except OSError:
                        pass
                    status = 502
                elif isinstance(error, (BrokenPipeError, ConnectionResetError)):
                    cut = f"client: {type(error).__name__}"
                else:
                    cut = f"relay: {type(error).__name__}: {error}"
                captured += f"relay: {error}".encode()
            row = {
                "t": round(started, 3),
                "tag": tag,
                "method": method,
                "path": path,
                "status": status,
                "ttfb_s": round(first - started, 3) if first else None,
                "s": round(time.time() - started, 3),
                "usage": usage_of(bytes(captured)) if method == "POST" else None,
            }
            if cost is not None:
                row["cost"] = cost
            if status >= 400:
                row["error"] = bytes(captured[:500]).decode("utf8", "replace")
            if cut:
                row["cut"] = cut
                # A body cut short never gets its last chunk, so a caller
                # that keeps the connection alive would wait on it for its
                # whole read timeout: closing it says the stream is over.
                self.close_connection = True
            if method == "POST" and status < 400 and row["usage"] is None:
                # How a reply that never said what it used ended.
                row["tail"] = bytes(captured[-300:]).decode("utf8", "replace")
            relay.meter(row)

    return Handler


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--port", type=int, default=18080)
    p.add_argument("--upstream", default="https://openrouter.ai/api/v1")
    p.add_argument("--key-env", default="OPENROUTER_API_KEY", help="where the key is read from")
    p.add_argument("--model", default="", help="send this model on every call")
    p.add_argument("--provider", default="", help="OpenRouter provider to pin, with no fallbacks")
    p.add_argument("--reasoning", default="", help="reasoning effort to pin, as the upstream takes it")
    p.add_argument(
        "--style",
        choices=["openrouter", "openai"],
        help="the upstream's fields for pins: OpenRouter's (with --provider) or OpenAI's; "
        "by default OpenRouter's for openrouter.ai and OpenAI's elsewhere",
    )
    p.add_argument("--meter", default="meter.jsonl")
    p.add_argument("--read-timeout", type=float, default=1200)
    p.add_argument("--stub", action="store_true", help="answer as a canned model, with no key")
    p.add_argument("--stub-call", default="", help="the stub's run_command arguments, as JSON")
    args = p.parse_args()
    if args.style is None:
        args.style = "openrouter" if "openrouter.ai" in args.upstream else "openai"
    key = os.environ.get(args.key_env, "")
    if not args.stub and not key:
        sys.exit(f"relay: {args.key_env} is not set; give it, or --stub")
    relay = Relay(args, key)
    server = ThreadingHTTPServer((args.host, args.port), handler(relay))
    server.daemon_threads = True
    print(f"relay on {args.host}:{args.port} to {'the stub' if args.stub else args.upstream}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
