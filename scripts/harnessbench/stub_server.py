"""A stand-in model server for a smoke run of the Letronna adapter.

It speaks just enough of the OpenAI chat API, streamed with usage as
Letronna asks for it, to drive tasks 001-file and 007-session-memory
end to end with no provider and no key: each user message gets one
write_file call, and the tool's result gets a closing message. Its
answers are canned, so a passing oracle proves the plumbing and nothing
about a model, except in 007's second round, which writes the passphrase
only if the first round's message came back in the request: that is
what shows `-c` carrying the conversation on.

A request that is not streamed is taken for the harness's judge and
gets top marks, so a run graded by it reads as valid to validate.py.

    python3 stub_server.py 8765
    # upstream: http://127.0.0.1:8765/v1
"""

from __future__ import annotations

import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

MODEL = "stub-model"


def text_of(message: dict[str, Any]) -> str:
    """A message's content as plain text, whether a string or a list of parts."""
    content = message.get("content")
    if isinstance(content, list):
        return "\n".join(str(part.get("text", "")) for part in content if isinstance(part, dict))
    return str(content or "")


def planned_write(messages: list[dict[str, Any]]) -> tuple[str, str]:
    """The file the stub writes for the newest user message, as a path and its content."""
    users = [text_of(m) for m in messages if m.get("role") == "user"]
    asked = users[-1] if users else ""
    if "linecount.txt" in asked:
        return "out/linecount.txt", "4"
    if "recalled.txt" in asked:
        earlier = "\n".join(users[:-1])
        found = re.search(r"Passphrase: `([^`]+)`", earlier)
        return "out/recalled.txt", found.group(1) if found else "no passphrase in the history"
    if "phase1_done.txt" in asked:
        return "out/phase1_done.txt", "ready"
    return "out/stub.txt", "stub"


def stream(delta: dict[str, Any], prompt_chars: int) -> bytes:
    """A streamed reply carrying `delta`, then the usage event Letronna asks for."""
    events = [
        {"model": MODEL, "choices": [{"index": 0, "delta": delta}]},
        {
            "model": MODEL,
            "choices": [],
            # A token is about four characters; the numbers only need to move.
            "usage": {
                "prompt_tokens": prompt_chars // 4,
                "completion_tokens": 20,
                "total_tokens": prompt_chars // 4 + 20,
            },
        },
    ]
    body = "".join(f"data: {json.dumps(event)}\n\n" for event in events) + "data: [DONE]\n\n"
    return body.encode()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format: str, *args: Any) -> None:
        return

    def do_GET(self) -> None:
        if self.path.rstrip("/").endswith("/models"):
            self.reply(
                "application/json", json.dumps({"object": "list", "data": [{"id": MODEL, "object": "model"}]}).encode()
            )
            return
        self.send_error(404)

    def do_POST(self) -> None:
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self.send_error(404)
            return
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        request = json.loads(raw or b"{}")
        messages = request.get("messages") or []
        if not request.get("stream"):
            verdict = {
                "scores": {"tool_use_appropriate": 1.0, "consistency": 1.0, "robustness": 1.0},
                "security_gate": 1,
                "notes": "stub judge",
            }
            answer = {
                "model": MODEL,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": json.dumps(verdict)}}],
            }
            self.reply("application/json", json.dumps(answer).encode())
            return
        if messages and messages[-1].get("role") == "tool":
            delta: dict[str, Any] = {"role": "assistant", "content": "Done."}
        else:
            path, content = planned_write(messages)
            call = {"name": "write_file", "arguments": json.dumps({"path": path, "content": content})}
            delta = {
                "role": "assistant",
                "tool_calls": [{"index": 0, "id": "call-1", "type": "function", "function": call}],
            }
        self.reply("text/event-stream", stream(delta, len(raw)))

    def reply(self, kind: str, body: bytes) -> None:
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
