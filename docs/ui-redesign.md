# UI redesign: Capabilities, MCP servers and Plugins

A proposal, part built: steps 1 and 2 of the order of work below, and the
sidebar, are in. It comes from running 0.1.0-alpha.5.1 on Linux with five MCP
servers and the four example plugins, and reading what the window drew. The
design canvas that goes with it holds a working prototype, the motion moves,
the annotated screenshots and the brand sheet.

Nothing here replaces the brand in `theme.rs` or the motion in `ui/motion.rs`.
The plan names what is already there, fills three gaps, and rearranges two
screens.

## What gets in the way today

**Capabilities › MCP** (`ui/mcp.rs`, `ui/capabilities.rs`)

1. The add form comes first. Three fields and a paste box fill the top 330 px,
   so the server list starts half way down the window.
2. A row carries up to five controls of the same weight: Tools, Log, Sign in,
   Reconnect and the switch. Nothing says which one fixes the row.
3. `linear stopped.` is the whole error. The cause is behind the Log button.
4. The sign-in error names a terminal command and "Settings › MCP", a place the
   window does not have (`mcp.rs:1933`, `mcp.rs:1982`, `mcp_auth.rs:1457`),
   while Sign in sits on the same row.
5. Servers are listed by name, so one that is off leads and the two that need
   the user are in the middle.
6. A server that gives no hints has every tool under "Destructive", search and
   read included, and the Read only preset is disabled.
7. Twenty-six blue switches in a column. Blue is the colour for what is
   pressed; as an on-state it hides the one switch that is off.
8. Tools open inside the row and add about 1,300 px to the page. The server's
   name scrolls away, and there is no search or filter.
9. "≈ 4.1k tokens per request" decides what to switch off and is the smallest
   text on the page, shown only once the list is open.

**Plugins** (`ui/plugin_page.rs`, `ui/settings.rs`)

10. Each plugin is seven small lines of manifest. "Runs as a Python program
    with your account's access" has the weight of "A status".
11. The same consent sentence in the same box on every plugin. Sandboxed
    JavaScript and unsandboxed Python look alike.
12. The switch is on and blue while the plugin waits for consent.
13. Marketplaces come after every installed plugin, under "Read lists".
14. Three things are called Plugins: the sidebar page, Settings › Plugins with
    another row design, and a family on the Tools tab. "Tools" is both a tab
    and a button on every server.

Worth keeping: both palettes and their contrast tests, the motion vocabulary,
the plain voice, and hairlines with near-square corners in place of shadows.

## Five rules

1. **Blue is for what is pressed.** An on-switch is the only standing blue.
   State is said with a shape and a word.
2. **One fix per row.** A row that needs the user shows the single button that
   fixes it. Everything else is in its details.
3. **Every error ends with what to press.** Say what happened, then give the
   button. No terminal commands in the window.
4. **Lists first, forms on demand.** The page is what the user has. Adding is a
   sheet that goes away.
5. **Order by who needs the user.** Needs you, then working, then off.

## Where things live

| Today | Proposed |
|---|---|
| Capabilities: Skills, Tools, MCP | Capabilities: Skills, Built-in tools, MCP servers, Plugins |
| Plugins, a sidebar page | The fourth Capabilities tab |
| Settings › Plugins (built-in integrations) | A "Built in" tag in the same Plugins list |
| Marketplaces at the foot of Plugins | Plugins › Discover |
| Add a server, a form above the list | A sheet from the Add server button |
| A server's tools and log, inside its row | A details panel at the right: Tools, Log, Setup |

The sidebar loses one entry and Settings loses one page. `Fix::Plugins` in
`toolset.rs` then opens the Plugins tab instead of Settings.

## The screens

**MCP servers.** A summary line (connected, needing the user, tools offered,
tokens added to every request) with Add server at its right. Below it three
groups: Needs you, Connected, Off. A row is a state mark, the name, how it is
reached, and one line saying how it is doing: "26 of 26 tools on · ≈ 4.1k
tokens per request", or the error with its cause. A row that needs the user
has one primary button, Sign in or Retry. The switch stays at the right.

**Server details.** Pressing a row opens a panel at the right, as the browser
panel opens beside the chat. Tools has a filter box, segments with counts
(All, Read, Write, Destructive, Off), the Read only and All on presets, and
groups whose switch shows on, off or some. Tool rows are compact: name, one
line, token weight, a small switch. When a server gives no hints the groups
are read from the tool names and a note says so; approvals still treat every
tool as able to change things. Log and Setup are the other two tabs, and
Reconnect, Edit and Remove live under Setup.

**Add server.** Three steps under the progress line the first-run setup
already draws: paste, check what was read, connect. The third step shows the
server starting and then what it brought, with Review its tools as the way on.

**Plugins.** Installed and Discover as segments. Installed is grouped as Needs
your OK, Running, Off. A row has the name and version, one line, how it runs
(a shield for the JavaScript sandbox, an amber warning for Python with the
account's access) and chips counting what it brings. A plugin waiting for
consent has Review and allow, which opens a sheet listing exactly what it
runs, adds, hears and shows; its switch is drawn as waiting, not on.

**Skills and Built-in tools** take the same row, so the four tabs read alike.

## Brand pattern

Kept as it is: Night and Paper, hairlines, 2 px and 4 px corners, no shadows,
film grain on the frame, Phosphor Light.

Added:

- **Type.** Three faces bundled in the binary with `TextSystem::add_fonts`, so
  every system draws the same letters: a serif masthead for the wordmark, page
  and sheet titles and empty states (Newsreader in the prototype), a sans for
  everything read and pressed (Inter), and a mono (JetBrains Mono). Today
  `HEADING_FONT` and `BODY_FONT` name Segoe UI Variable off macOS and
  `WORDMARK_FONT` names Georgia; Linux has neither.
- **A named amber.** "Needs you" was drawn with gpui-component's default
  `warning`, a yellow that reads at 2.4:1 on Paper. It has a number in each
  palette now (`#f0b429` on Night, the existing `OCHRE` on Paper), both in the
  contrast tests.
- **A status language.** Circle and green for connected, triangle and amber
  for needs you, square and red for failed, a pulsing Signal dot for working,
  a ring for off. Always with the word.
- **A row part.** One function draws every server, plugin, skill and tool
  family: state, name, tag, one line, one fix, switch.
- **Pointer feedback.** `theme.rs` holds every hover equal to its base so
  nothing moves under the pointer. Keep that for motion, and let colour answer
  at once: a tint on hover with no transition in and `QUICK` out.

## Motion

Every move uses what `ui/motion.rs` already defines.

| Move | Timing | Where |
|---|---|---|
| Rows arrive | `enter(STANDARD)`, `RISE`, 30 ms apart, six rows at most | A tab opening |
| Tab changes | `STANDARD`; underline grows, content rises | Capabilities, details |
| Details open | `slide(STANDARD)` in, `exit(QUICK)` out | Server details |
| Group switch | `QUICK`, children 20 ms apart | Tool groups |
| Connecting | `PASS` sweep, mark from `MARK_FROM` | A server starting, an install |
| Row changes group | `GENTLE` | Needs you to Connected |
| Sheet | `enter(STANDARD)`, `exit(QUICK)` over the blurred backdrop | Add server, Review and allow |
| Pointer | 0 ms in, `QUICK` out, colour only | Buttons, rows |

All of it draws its end state at once while motion is reduced, as now.

## The sidebar

Eight rows of the same weight read as one list, with the thing done most,
New session, drawn as a page like the rest. Built:

- **New session is edged as a button.** It is the one row that does something
  rather than goes somewhere.
- **Two groups, a gap apart.** What Letronna is doing and has made (Activity,
  Scheduled jobs, Workflows while there are runs, Artifacts), then what it can
  reach (Capabilities, Plugins, Messaging, Wallet). No labels: the gap says it
  and costs 8 px, where two more headings would push the sessions down.
- **What waits is said on the row.** A triangle and a count at the end of
  Capabilities while servers want a sign-in or have failed, and of Plugins
  while plugins wait to be allowed, so neither page has to be opened to find
  out.
- **Rows answer the pointer.** A tint on a page or session row under the
  pointer, at once and without movement.
- **A picked session filter is named.** The four glyphs kept their tooltips;
  the one that is hiding the other sessions now says which it is.

## Order of work

1. **Shared parts.** Done. `ui/parts.rs` has the row, status mark, tag, chip,
   segments and empty state; `theme.rs` has the amber and the hover tint; the
   three families are bundled, brought by `scripts/fonts.sh` and loaded in
   `assets.rs`.
2. **MCP list.** Done. `mcp.rs` lists the servers first, grouped, each row with
   one line and one fix; the adder is a sheet from Add server; the three
   strings in core name the place the window has.
3. **Server details.** A panel tab for a server (`PanelTab` in `ui/panel.rs`),
   the filter and segments, the three-way group switch. In core, say whether a
   tool's kind came from the server or was read from its name.
4. **Plugins.** Move `plugins_page` under Capabilities, group it, add the
   review sheet and the waiting switch, split Discover out, and fold
   `settings.rs::plugins_page` into the same list.
5. **Motion.** The stagger, the row changing group and the details entrance in
   `ui/motion.rs`.

Steps 1 and 2 stand alone and remove findings 1 to 5.

What steps 1 and 2 left as it was, for the steps after them:

- A row opens in place, under itself, until step 3 gives it a panel. Tools and
  Log are segments there, and Reconnect and Sign out sit beside them.
- An opened server's tools are still one group of switches when the server
  gives no hints (findings 6 and 7): the grouping and the filter are step 3.
- Adding a server is one sheet, not three steps. Once it is added the sheet
  goes and the list shows the server starting.
- The tint under the pointer is on rows only. Buttons keep the stillness
  `ui::NoHover` gives them; changing every button in the window is a decision
  of its own.
- The three families are used on every system. `HEADING_FONT`, `BODY_FONT` and
  `MONO_FONT` in `theme.rs` are the one place to name a system's own faces
  again; the bundled files add 1.7 MB to the window's binary.

## Open questions

- Serif masthead or sans? The prototype has a switch for it.
- Reading tool kinds from names is a guess. It only arranges the list; should
  it also be allowed to enable the Read only preset?
- Plugins as a Capabilities tab makes it one step further from the sidebar.

## Seeing the window on Linux without a display

`scripts/shot.ps1` and `scripts/verify-ui.ps1` are for Windows. The
screenshots for this plan were taken with a software renderer:

```sh
sudo apt-get install -y mesa-vulkan-drivers xvfb openbox xdotool imagemagick
Xvfb :99 -screen 0 1440x900x24 & DISPLAY=:99 openbox &
AGENT_DATA_DIR=/tmp/letronna-shots DISPLAY=:99 target/debug/letronna-gui &
DISPLAY=:99 import -window "$(DISPLAY=:99 xdotool search --name '^Letronna$' | tail -1)" shot.png
```
