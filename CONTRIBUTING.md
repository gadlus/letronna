# Contributing

Thanks for looking. Letronna is early, so the bar is simple: keep it small, keep it native.

## Ground rules

- No web views, no Electron, no JavaScript. The window renders through GPUI; the terminal writes escape sequences by hand.
- Prefer what `gpui-component` already ships over a new widget.
- Use theme tokens (`cx.theme().…`), not hard-coded colours.
- Icons come from Phosphor Light via `scripts/icons.sh`. Add the name to the map there; do not hand-draw SVGs.
- One change per pull request. Say what changed and why in a few lines.

## Where things live

Three crates in one workspace. The split is the point: anything that is not
about drawing belongs in `core`, so the window and the terminal behave the
same way.

```
crates/core   the agent, and everything both front ends need
  agent.rs    one turn as a state machine: ask, run a tool, ask the user, done
  chat.rs     the request, streaming, the system prompt, model lists
  tools.rs    every tool the model can call, one struct each
  store.rs    sessions on disk: append-only JSONL, one file per session
  jobs.rs     commands left running in the background
  skills.rs   saved instructions the model can be given
  gateway.rs  the providers and which of them need a key
  browser.rs  the browse and use_page tools, and what each action does
  page.rs     a page as the model reads it: lines with refs to act on
  cdp.rs      starting the installed browser and talking DevTools to it
  watch.rs    the browser in the window: its pictures, the pointer, the user's input

crates/gui    the window (binary `letronna-gui`)
  app.rs      state and actions on `Chat`
  ui/*.rs     one file per surface, each an `impl Chat` block
  platform/   install, tray, and the things that differ per OS

crates/cli    the terminal (binary `letronna`)
  main.rs     start up, then the loop: read a line, answer it
  args.rs     what was asked for on the command line
  commands.rs the slash commands
  setup.rs    the first-run questions the install scripts hand over to
  install.rs  putting the binaries in place and on PATH
  bar.rs      the input box: keys, the menu, questions, what it looks like
  edit.rs     the line being typed, with no terminal attached
  screen.rs   the window we draw in, scrolling, selecting, copying
  render.rs   colour, wrapping, the marks either side of the conversation
  session.rs  driving a turn, showing tools, asking for approval
  diff.rs     a change shown before it happens
```

A new tool is a struct in `tools.rs` implementing `Tool` plus a line in
`all()`. A new slash command is an entry in `bar::COMMANDS` plus an arm in
`commands::command`. Neither should need anything else.

## Conventions

- Comments say **why**, not what. If the code needs a comment to say what it
  does, the code is wrong. A comment earns its place by recording the thing
  that is not in the code: the bug it avoids, the terminal that does it
  differently, the trade that was made.
- Tests live beside the code, at the bottom of the file, and test the parts
  that do not need a terminal or a network: wrapping, line editing, diffing,
  argument parsing, the agent's state machine.
- Name things as you would say them aloud. `fold_burst`, not `process_input`.

## Before you push

```powershell
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --release
cargo build --release
```

## Changing something you can see

Run it and look at it. A passing test suite says nothing about a layout, and
this is a program where almost every bug so far has been visible and none of
them were caught by a test.

For the terminal, `scripts/shot.ps1` captures the Letronna window alone —
never the whole desktop, which is someone's private screen. Check the things
that have broken before: maximise and restore, scroll back, paste a paragraph,
drag to select.

For the window, `scripts/verify-ui.ps1 -Action capture|click|send`.

Some things cannot be checked with injected input: a synthetic mouse sends no
drag events, so a live highlight has to be looked at by hand. Say plainly in
the pull request what you did not verify.

## Testing the installer

`scripts/install.ps1` and `letronna setup` can be driven without a keyboard:
pipe the answers in, and point `LETRONNA_DOWNLOAD` at a local zip and
`AGENT_DATA_DIR` / `LOCALAPPDATA` at a scratch folder. One thing a scratch
folder does not isolate: the PATH step writes your real user PATH. Take the
scratch entry back out afterwards.

## Releasing

A release is one version for the window and the terminal on every platform.

1. Set `version` in `Cargo.toml`, move **Unreleased** in `CHANGELOG.md` under
   it, and merge that to `main`.
2. Tag it and push the tag: `git tag v0.1.0-alpha.2` and
   `git push origin v0.1.0-alpha.2`. CI builds Windows, Linux and macOS into a
   **draft** release, after checking that the tag is the version in
   `Cargo.toml` and that each binary says so. While Actions cannot run,
   `scripts/release.ps1 build` makes the same draft here, with the Windows
   files only.
3. `scripts/release.ps1 sign v0.1.0-alpha.2` on the machine that holds the
   release key. It downloads the draft, runs the Windows binaries, writes
   `latest.json` with every file's SHA-256 and size, signs it, shows it, and
   publishes once you say yes. Letronna installs an update only when that
   signature is from a key in `RELEASE_KEYS` in `crates/core/src/update.rs`.
   The key never goes near GitHub.

A draft reaches nobody: the account API only serves published releases. To
take a bad release back, `gh release edit <tag> --draft=true` makes the one
before it the latest again. Copies that already updated keep it, because
Letronna never moves back a version, so the fix is a newer release.

Keep a second key offline. Its public half sits in `RELEASE_KEYS` beside the
first, so if the one in use is lost or leaks, releases signed with the backup
still install and the next release can drop the bad key.

### Code signing

Separate from the release key: this is what stops Windows SmartScreen and
macOS Gatekeeper warning about an unknown publisher. CI signs when these
repository secrets are set, and builds unsigned (as before) when they are not.

- **Windows**, with Azure Artifact Signing: `AZURE_TENANT_ID`,
  `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` (an app registration with the
  signing role on the account), `AZURE_SIGNING_ENDPOINT` (the account's
  region endpoint, such as `https://eus.codesigning.azure.net/`),
  `AZURE_SIGNING_ACCOUNT` and `AZURE_CERTIFICATE_PROFILE`. Both `.exe` files
  are signed before they are packaged.
- **macOS**, with an Apple Developer ID: `MACOS_CERTIFICATE` (the
  "Developer ID Application" certificate and key as a `.p12`, base64) and
  `MACOS_CERTIFICATE_PASSWORD`; for notarization, an App Store Connect API
  key: `APPLE_API_KEY` (the `.p8`, base64), `APPLE_API_KEY_ID` and
  `APPLE_API_ISSUER`. The app is signed with the hardened runtime, the disk
  image is notarized and stapled, and the bare terminal binary is signed.

## Reporting bugs

Open an issue with the gateway you used, the model, and what you expected.
Never paste an API key.
