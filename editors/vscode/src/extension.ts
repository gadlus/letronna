// Letronna in VS Code: open its terminal, hand it the code you select, and
// see the change it asks to make as a diff beside the question.
//
// The terminal finds this window through the note `link.ts` leaves in
// Letronna's data folder, or the port set in this window's terminals.

import * as path from "path";
import * as vscode from "vscode";
import { FromTerminal, Link } from "./link";
import { dataFolder, reference } from "./place";

/** The documents a diff shows, which exist only here. */
const DIFF_SCHEME = "letronna-diff";

/** The terminal this extension opens Letronna in. */
const TERMINAL_NAME = "Letronna";

/** How long the selection settles before the terminal is told. */
const SETTLE_MS = 150;

let link: Link | undefined;
/** A diff's two sides, by their document's path. */
const sides = new Map<string, string>();
let settling: NodeJS.Timeout | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(DIFF_SCHEME, {
      provideTextDocumentContent: (uri) => sides.get(uri.path) ?? "",
    }),
    vscode.commands.registerCommand("letronna.open", openTerminal),
    vscode.commands.registerCommand("letronna.addSelection", addSelection),
    vscode.commands.registerCommand("letronna.addFile", (uri?: vscode.Uri) => addFile(uri)),
    vscode.window.onDidChangeTextEditorSelection(settle),
    vscode.window.onDidChangeActiveTextEditor(settle),
    vscode.workspace.onDidChangeWorkspaceFolders(() => link?.writeLock()),
  );
  try {
    link = await Link.start({
      lockDir: path.join(dataFolder(), "ide"),
      ide: vscode.env.appName,
      folders,
      onMessage: (message) => void handle(message),
      onChange: tellSelection,
    });
  } catch (error) {
    void vscode.window.showWarningMessage(`Letronna could not listen for its terminal: ${error}`);
    return;
  }
  // Every terminal this window opens is told where to find it.
  context.environmentVariableCollection.persistent = false;
  context.environmentVariableCollection.replace("LETRONNA_IDE_PORT", String(link.port));
  context.subscriptions.push({ dispose: () => link?.close() });
}

export function deactivate(): void {
  link?.close();
  link = undefined;
}

function folders(): string[] {
  return (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath);
}

/** Start Letronna in a terminal of its own, or show the one it runs in. */
function openTerminal(): void {
  const open = letronnaTerminal();
  if (open) {
    open.show();
    return;
  }
  const active = vscode.window.activeTextEditor?.document.uri;
  const cwd = (active && vscode.workspace.getWorkspaceFolder(active)?.uri.fsPath) ?? folders()[0];
  const terminal = vscode.window.createTerminal({
    name: TERMINAL_NAME,
    cwd,
    env: link ? { LETRONNA_IDE_PORT: String(link.port) } : undefined,
  });
  terminal.show();
  const command = vscode.workspace.getConfiguration("letronna").get<string>("command");
  terminal.sendText(command || "letronna");
}

function letronnaTerminal(): vscode.Terminal | undefined {
  return vscode.window.terminals.find(
    (terminal) => terminal.name === TERMINAL_NAME && terminal.exitStatus === undefined,
  );
}

/** The lines selected, first and last counted from 1, or none for a bare cursor. */
function selected(editor: vscode.TextEditor): [number, number] | undefined {
  const selection = editor.selection;
  if (selection.isEmpty) {
    return undefined;
  }
  // A selection that ends where a line starts does not take that line.
  let end = selection.end.line;
  if (selection.end.character === 0 && end > selection.start.line) {
    end -= 1;
  }
  return [selection.start.line + 1, end + 1];
}

async function addSelection(): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.uri.scheme !== "file") {
    void vscode.window.showInformationMessage("Open a saved file to add from it.");
    return;
  }
  await add(editor.document.uri.fsPath, selected(editor), false);
}

async function addFile(uri?: vscode.Uri): Promise<void> {
  const target = uri ?? vscode.window.activeTextEditor?.document.uri;
  if (!target || target.scheme !== "file") {
    void vscode.window.showInformationMessage("Pick a saved file or folder to add.");
    return;
  }
  const stat = await vscode.workspace.fs.stat(target);
  await add(target.fsPath, undefined, (stat.type & vscode.FileType.Directory) !== 0);
}

/** Put a reference to `file` in the box of the terminal that linked last. */
async function add(file: string, lines: [number, number] | undefined, folder: boolean): Promise<void> {
  const sent = link?.send(
    { type: "add", path: file, ...(lines && { start: lines[0], end: lines[1] }) },
    "newest",
  );
  if (sent) {
    await showLinked();
    return;
  }
  // Not linked, as an older Letronna is not: type it in, as a person would.
  const terminal = letronnaTerminal();
  if (terminal) {
    terminal.sendText(reference(file, folders(), lines, folder) + " ", false);
    terminal.show();
    return;
  }
  const start = "Open in terminal";
  if ((await vscode.window.showInformationMessage("Letronna is not running here yet.", start)) === start) {
    openTerminal();
  }
}

/** Bring forward the terminal the newest link came from, found by its shell. */
async function showLinked(): Promise<void> {
  const shell = link?.newest?.shell;
  for (const terminal of vscode.window.terminals) {
    if (shell !== undefined && (await terminal.processId) === shell) {
      terminal.show();
      return;
    }
  }
}

function settle(): void {
  clearTimeout(settling);
  settling = setTimeout(tellSelection, SETTLE_MS);
}

/** Tell the terminals what file is open and which lines are selected. */
function tellSelection(): void {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.uri.scheme !== "file") {
    link?.send({ type: "selection" });
    return;
  }
  const lines = selected(editor);
  link?.send({
    type: "selection",
    path: editor.document.uri.fsPath,
    ...(lines && { start: lines[0], end: lines[1] }),
  });
}

async function handle(message: FromTerminal): Promise<void> {
  try {
    switch (message.type) {
      case "open": {
        const line = Math.max((message.line ?? 1) - 1, 0);
        const at = new vscode.Position(line, 0);
        // A file opened after a change goes ahead takes the preview tab, so
        // a run of edits does not leave a tab for each.
        await vscode.window.showTextDocument(vscode.Uri.file(message.path), {
          selection: new vscode.Range(at, at),
          preview: message.preview ?? false,
          preserveFocus: true,
        });
        break;
      }
      case "diff": {
        const name = path.basename(message.path);
        const side = (which: string) =>
          vscode.Uri.from({ scheme: DIFF_SCHEME, path: `/${message.id}/${which}/${name}` });
        const [before, after] = [side("before"), side("after")];
        sides.set(before.path, message.before);
        sides.set(after.path, message.after);
        const title = message.title ?? `${name}: the change Letronna asks to make`;
        // The terminal keeps the keys: the question is answered there.
        await vscode.commands.executeCommand("vscode.diff", before, after, title, {
          preview: true,
          preserveFocus: true,
        });
        break;
      }
      case "close_diff":
        closeDiff(message.id);
        break;
    }
  } catch (error) {
    void vscode.window.showWarningMessage(`Letronna: ${error}`);
  }
}

function closeDiff(id: string): void {
  const prefix = `/${id}/`;
  for (const group of vscode.window.tabGroups.all) {
    for (const tab of group.tabs) {
      const input = tab.input;
      if (
        input instanceof vscode.TabInputTextDiff &&
        input.original.scheme === DIFF_SCHEME &&
        input.original.path.startsWith(prefix)
      ) {
        void vscode.window.tabGroups.close(tab);
      }
    }
  }
  for (const key of [...sides.keys()]) {
    if (key.startsWith(prefix)) {
      sides.delete(key);
    }
  }
}
