// The link to Letronna's terminal: a server on this computer's loopback
// address alone, and a note in Letronna's data folder saying where it
// listens and the token that lets a terminal in. A terminal that cannot
// give the token is closed before anything is read from it or sent to it.
//
// Messages are JSON, one per line, both ways. Nothing here needs VS Code,
// so it can be tested on its own.

import * as crypto from "crypto";
import * as fs from "fs";
import * as net from "net";
import * as path from "path";

/** What a terminal sends first, to be let in. */
export interface Hello {
  type: "hello";
  token: string;
  /** The terminal's process, and the shell it runs in where it knows it. */
  pid?: number;
  shell?: number;
  folder?: string;
}

/** What a terminal asks for once it is in. */
export type FromTerminal =
  | { type: "open"; path: string; line?: number; preview?: boolean }
  | { type: "diff"; id: string; path: string; before: string; after: string; title?: string }
  | { type: "close_diff"; id: string };

/** What goes to a terminal. */
export type ToTerminal =
  | { type: "welcome"; ide: string; folders: string[] }
  | { type: "add"; path: string; start?: number; end?: number }
  | { type: "selection"; path?: string; start?: number; end?: number };

/** The longest line a terminal may send: a diff carries two copies of a file. */
export const LINE_CHARS = 16 * 1024 * 1024;

/** How long a connection has to give the token before it is closed. */
export const HELLO_MS = 5000;

/** A diff's id becomes part of a document's path, so it is kept plain. */
const PLAIN_ID = /^[A-Za-z0-9_-]{1,128}$/;

export interface LinkOptions {
  /** The folder the note goes in, `<data folder>/ide`. */
  lockDir: string;
  /** The editor's name, as the terminal shows it. */
  ide: string;
  folders: () => string[];
  onMessage: (message: FromTerminal) => void;
  /** Told when a terminal comes or goes. */
  onChange?: () => void;
}

/** One terminal that gave the token. */
interface Linked {
  socket: net.Socket;
  hello: Hello;
}

export class Link {
  /** 32 random bytes: whoever has it can type into the chat. */
  readonly token = crypto.randomBytes(32).toString("hex");
  private linked: Linked[] = [];

  private constructor(
    private readonly server: net.Server,
    readonly port: number,
    private readonly options: LinkOptions,
  ) {}

  static start(options: LinkOptions): Promise<Link> {
    return new Promise((resolve, reject) => {
      const server = net.createServer();
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => {
        const { port } = server.address() as net.AddressInfo;
        const link = new Link(server, port, options);
        server.on("connection", (socket) => link.accept(socket));
        try {
          link.writeLock();
        } catch (error) {
          server.close();
          reject(error);
          return;
        }
        resolve(link);
      });
    });
  }

  /** Where the note for this window is kept. */
  get lockPath(): string {
    return path.join(this.options.lockDir, `${this.port}.json`);
  }

  /** How many terminals are linked now. */
  get count(): number {
    return this.linked.length;
  }

  /** The hello of the terminal that linked last, if any is linked. */
  get newest(): Hello | undefined {
    return this.linked[this.linked.length - 1]?.hello;
  }

  /** Write the note again, as the window's folders change. */
  writeLock(): void {
    fs.mkdirSync(this.options.lockDir, { recursive: true, mode: 0o700 });
    const lock = {
      version: 1,
      pid: process.pid,
      ide: this.options.ide,
      folders: this.options.folders(),
      token: this.token,
    };
    // Only its owner may read it, since the token is in it; written aside
    // and moved in, so a terminal never reads half of it.
    const aside = `${this.lockPath}.${process.pid}.tmp`;
    fs.writeFileSync(aside, JSON.stringify(lock), { mode: 0o600 });
    fs.renameSync(aside, this.lockPath);
  }

  /** Send to every linked terminal, or only the newest; false when none is. */
  send(message: ToTerminal, to: "all" | "newest" = "all"): boolean {
    const line = JSON.stringify(message) + "\n";
    const targets = to === "newest" ? this.linked.slice(-1) : this.linked;
    for (const { socket } of targets) {
      socket.write(line);
    }
    return targets.length > 0;
  }

  /** Stop listening, let every terminal go and take the note away. */
  close(): void {
    for (const { socket } of this.linked) {
      socket.destroy();
    }
    this.linked = [];
    this.server.close();
    try {
      fs.unlinkSync(this.lockPath);
    } catch {
      // Gone already.
    }
  }

  private accept(socket: net.Socket): void {
    // The server listens on loopback alone; this is the same rule again.
    if (!loopback(socket.remoteAddress)) {
      socket.destroy();
      return;
    }
    socket.setEncoding("utf8");
    let buffer = "";
    let mine: Linked | undefined;
    const late = setTimeout(() => mine || socket.destroy(), HELLO_MS);
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      if (buffer.length > LINE_CHARS) {
        socket.destroy();
        return;
      }
      for (let at = buffer.indexOf("\n"); at >= 0; at = buffer.indexOf("\n")) {
        const message = parse(buffer.slice(0, at));
        buffer = buffer.slice(at + 1);
        if (!mine) {
          if (!isHello(message) || !same(message.token, this.token)) {
            socket.destroy();
            return;
          }
          clearTimeout(late);
          mine = { socket, hello: message };
          this.linked.push(mine);
          const welcome: ToTerminal = {
            type: "welcome",
            ide: this.options.ide,
            folders: this.options.folders(),
          };
          socket.write(JSON.stringify(welcome) + "\n");
          this.options.onChange?.();
        } else if (isFromTerminal(message)) {
          this.options.onMessage(message);
        }
      }
    });
    socket.on("error", () => socket.destroy());
    socket.on("close", () => {
      clearTimeout(late);
      if (mine) {
        this.linked = this.linked.filter((linked) => linked !== mine);
        this.options.onChange?.();
      }
    });
  }
}

function loopback(address: string | undefined): boolean {
  return address === "127.0.0.1" || address === "::ffff:127.0.0.1" || address === "::1";
}

function parse(line: string): unknown {
  try {
    return JSON.parse(line);
  } catch {
    return undefined;
  }
}

/** The token compared in a time that says nothing about how much matched. */
function same(given: unknown, token: string): boolean {
  if (typeof given !== "string" || given.length !== token.length) {
    return false;
  }
  return crypto.timingSafeEqual(Buffer.from(given), Buffer.from(token));
}

function isHello(message: unknown): message is Hello {
  return typeof message === "object" && message !== null && (message as Hello).type === "hello";
}

/** Whether `message` is something a terminal may ask for, well formed. */
export function isFromTerminal(message: unknown): message is FromTerminal {
  if (typeof message !== "object" || message === null) {
    return false;
  }
  const m = message as Record<string, unknown>;
  const text = (key: string) => typeof m[key] === "string";
  switch (m.type) {
    case "open":
      return (
        text("path") &&
        (m.line === undefined || Number.isInteger(m.line)) &&
        (m.preview === undefined || typeof m.preview === "boolean")
      );
    case "diff":
      return (
        text("path") &&
        text("before") &&
        text("after") &&
        typeof m.id === "string" &&
        PLAIN_ID.test(m.id) &&
        (m.title === undefined || text("title"))
      );
    case "close_diff":
      return typeof m.id === "string" && PLAIN_ID.test(m.id);
    default:
      return false;
  }
}
