# Letronna for VS Code

Use Letronna's terminal from VS Code:

- **Letronna: Open in terminal** starts `letronna` in a terminal at the workspace folder, or shows the one it already runs in.
- **Letronna: Add selection to chat** (Cmd+Alt+K on macOS, Ctrl+Alt+K elsewhere, or right-click in an editor) puts `@file#L10-20` in Letronna's box, or `@file` with nothing selected.
- **Letronna: Add file to chat** (right-click a file or folder in the Explorer, or an editor tab) puts `@file` or `@folder/` there.
- While Letronna is linked, its box edge shows the file open here and the lines selected.
- When Letronna asks to change a file, the change opens here as a diff beside the question. The diff closes once you answer in the terminal.

## Install

It isn't on the Marketplace. Build it and install the file:

```sh
cd editors/vscode
npm install
npm run package                      # makes letronna-0.1.0.vsix
code --install-extension letronna-0.1.0.vsix
```

Without the `code` command, use **Extensions › … › Install from VSIX…** in VS Code. `npm test` runs the extension's tests.

## How the terminal finds this window

When VS Code starts, the extension listens on `127.0.0.1` on a free port and writes `<Letronna's data folder>/ide/<port>.json`:
- The note holds the port, VS Code's process id, the workspace folders and a random token.
- Only you can read it (0600).
- On macOS the data folder is `~/Library/Application Support/Letronna`. `AGENT_DATA_DIR` moves it, as it does for Letronna.

Terminals this window opens also get `LETRONNA_IDE_PORT`.

How a terminal links:
- **It links itself** when Letronna is started inside VS Code, or when you type `/ide`. It picks the window whose folder holds its own.
- **It must give the token** before anything else; a connection that doesn't is closed.
- **Nothing outside this computer** can reach the port.

Messages are JSON, one per line:
- **From the terminal:** `hello` with the token, then `open` (a file at a line), `diff` (a file's text now and after the change) and `close_diff`.
- **From VS Code:** `welcome`, `add` (a reference for the box) and `selection` (the open file and selected lines).
