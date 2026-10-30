import * as assert from "assert";
import * as fs from "fs";
import * as net from "net";
import * as os from "os";
import * as path from "path";
import { test } from "node:test";
import { FromTerminal, isFromTerminal, Link } from "../link";
import { dataFolder, reference } from "../place";

/** A terminal stand-in: connects, sends `lines`, and gives every line it hears. */
function terminal(port: number, lines: string[]): Promise<{ heard: string[]; closed: boolean }> {
  return new Promise((resolve) => {
    const heard: string[] = [];
    const socket = net.connect(port, "127.0.0.1", () => {
      for (const line of lines) {
        socket.write(line + "\n");
      }
    });
    socket.setEncoding("utf8");
    let buffer = "";
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      const parts = buffer.split("\n");
      buffer = parts.pop() ?? "";
      heard.push(...parts);
    });
    socket.on("close", () => resolve({ heard, closed: true }));
    setTimeout(() => {
      socket.destroy();
      resolve({ heard, closed: false });
    }, 300);
  });
}

async function started(asked: FromTerminal[] = []): Promise<Link> {
  const lockDir = fs.mkdtempSync(path.join(os.tmpdir(), "letronna-ide-"));
  return Link.start({
    lockDir,
    ide: "Test Code",
    folders: () => ["/work"],
    onMessage: (message) => asked.push(message),
  });
}

test("the note says where to connect, holds the token and is its owner's alone", async () => {
  const link = await started();
  const lock = JSON.parse(fs.readFileSync(link.lockPath, "utf8"));
  assert.deepStrictEqual(
    { pid: lock.pid, ide: lock.ide, folders: lock.folders, token: lock.token },
    { pid: process.pid, ide: "Test Code", folders: ["/work"], token: link.token },
  );
  assert.strictEqual(path.basename(link.lockPath), `${link.port}.json`);
  if (process.platform !== "win32") {
    assert.strictEqual(fs.statSync(link.lockPath).mode & 0o777, 0o600);
  }
  link.close();
  assert.ok(!fs.existsSync(link.lockPath));
});

test("a terminal without the token is closed and hears nothing", async () => {
  const asked: FromTerminal[] = [];
  const link = await started(asked);
  const open = JSON.stringify({ type: "open", path: "/work/a.rs" });
  for (const first of [open, JSON.stringify({ type: "hello", token: "wrong" }), "not json"]) {
    const { heard, closed } = await terminal(link.port, [first, open]);
    assert.deepStrictEqual(heard, []);
    assert.ok(closed);
  }
  assert.deepStrictEqual(asked, []);
  assert.strictEqual(link.count, 0);
  link.close();
});

test("a terminal with the token is welcomed, hears what is added and is listened to", async () => {
  const asked: FromTerminal[] = [];
  const link = await started(asked);
  const hello = JSON.stringify({ type: "hello", token: link.token, pid: 1, shell: 2 });
  const open = JSON.stringify({ type: "open", path: "/work/a.rs", line: 3 });
  const listening = terminal(link.port, [hello, open]);
  await new Promise((done) => setTimeout(done, 100));
  assert.strictEqual(link.newest?.shell, 2);
  assert.ok(link.send({ type: "add", path: "/work/a.rs", start: 1, end: 2 }, "newest"));
  const { heard } = await listening;
  assert.deepStrictEqual(heard.map((line) => JSON.parse(line)), [
    { type: "welcome", ide: "Test Code", folders: ["/work"] },
    { type: "add", path: "/work/a.rs", start: 1, end: 2 },
  ]);
  assert.deepStrictEqual(asked, [{ type: "open", path: "/work/a.rs", line: 3 }]);
  link.close();
});

test("the link listens on this computer's loopback address alone", async () => {
  const link = await started();
  const address = (link as unknown as { server: net.Server }).server.address() as net.AddressInfo;
  assert.strictEqual(address.address, "127.0.0.1");
  link.close();
});

test("only well-formed asks are taken, and a diff's id stays plain", () => {
  assert.ok(isFromTerminal({ type: "diff", id: "call_1", path: "/a", before: "", after: "x" }));
  assert.ok(!isFromTerminal({ type: "diff", id: "../x", path: "/a", before: "", after: "x" }));
  assert.ok(!isFromTerminal({ type: "open" }));
  assert.ok(isFromTerminal({ type: "open", path: "/a", line: 2, preview: true }));
  assert.ok(!isFromTerminal({ type: "open", path: "/a", preview: "yes" }));
  assert.ok(!isFromTerminal({ type: "run", command: "rm -rf /" }));
});

test("references are written as the terminal writes them", () => {
  assert.strictEqual(reference("/work/src/a b.rs", ["/work"], [10, 20]), "@src/a\\ b.rs#L10-20");
  assert.strictEqual(reference("/work/src/a.rs", ["/work"], [4, 4]), "@src/a.rs#L4");
  assert.strictEqual(reference("/work/docs", ["/work"], undefined, true), "@docs/");
  assert.strictEqual(reference("/elsewhere/a.rs", ["/work"]), "@/elsewhere/a.rs");
});

test("the data folder is found as Letronna finds it", () => {
  assert.strictEqual(dataFolder({ AGENT_DATA_DIR: "/scratch" }, "darwin"), "/scratch");
  assert.strictEqual(
    dataFolder({ HOME: "/Users/a" }, "darwin"),
    "/Users/a/Library/Application Support/Letronna",
  );
  assert.strictEqual(dataFolder({ HOME: "/home/a" }, "linux"), "/home/a/.local/share/Letronna");
});
