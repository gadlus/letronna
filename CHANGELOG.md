# Changelog

Every change to Letronna — the app, the terminal, on Windows, macOS and Linux —
by version, newest first. One version number covers all of them.

Versions follow [semantic versioning](https://semver.org): `0.x` until the first
stable release, `-alpha.N` / `-beta.N` while it is early. Changes gather under
**Unreleased** and become a version when it is tagged (`vX.Y.Z`), which builds
the release for every platform. Each line starts with where it applies:
**App**, **Terminal**, **Windows**, **macOS**, **Linux**, or **All**.

This file is also the App tab of https://letronna.ai/changelog.

## [Unreleased]

### Added
- **Terminal** — The box's bottom edge says how full the session's context is once there is a reply, from what the latest request carried, as the window's status bar does: quiet while there is room, amber from 70%, pointing to `/compact` from 85% and red from 90%. `/context` shows what fills it, part by part: the system prompt, tools, MCP, memory, skills and the conversation.
- **Terminal** — The box's top edge names the session and where its folder stands in git (branch, what is changed, how far from upstream), and the window's title leads with the session's name, turns while it works and says "waiting for you" while a card or question waits; the title the terminal had comes back on leaving where the terminal keeps one. Names taken from a prompt's first lines show on one line there and in `/resume`.
- **Terminal** — `/rewind`, `/undo` or Esc twice on an empty box lists every message you sent, newest first and narrowed by typing, and going back to one asks whether to restore the code and the conversation, the conversation alone or the code alone, or to summarize up to it or branch from it there; `/undo` no longer only takes back the last exchange. Putting the code back alone tells the model which files went back, and a rewind further back still puts them back to how they were first.
- **All** — A Claude subscription is a provider you sign in to, beside the Anthropic API key: the models your plan includes, no key to paste.
- **All** — A built-in wallet the agent pays from: make one or bring one in from MetaMask, Phantom or any wallet app, on Base, Ethereum, Solana and any EVM chain. Sites that ask to be paid (x402) are paid in USDC within caps you set, asked each time or on their own below a price you choose; sends always ask. `letronna wallet`, `/wallet`, the Wallet page and Settings › Wallet.
- **Web** — A terminal in the browser: the Cloud page opens Letronna itself, running in Letronna Cloud on a new thread or one you have, to type into as on your own computer.
- **Terminal** — On a machine that is thrown away after (`LETRONNA_DISPOSABLE=1`, as in Letronna Cloud), the terminal takes its server, key and model from `AGENT_BASE_URL`, `AGENT_API_KEY` and `AGENT_MODEL`, as `-p` does, and saves none of it.
- **Web** — Letronna Cloud takes more servers: admins add a VPS from the admin page with one setup command, and runs and 24/7 bots spread across every server.
- **Web** — letronna.ai shows Letronna Cloud on the home page and has a Cloud page in the docs; phone apps for iPhone and Android are marked coming soon.
- **All** — A shorter README, and a full guide in `docs/`: install, the app and terminal, gateways, tools, extending, the cloud, configuration, data, architecture, building and benchmarks.
- **All** — `letronna -p --output json` says which window the run's requests were sized by and where it came from (`"window": {"tokens", "source"}`), and names the model the run used, also when the environment or `--model` chose it.
- **All** — With no search service chosen, search_web searches through the session's gateway where it sells search, Letronna's from the account's credit or a gateway's cheapest search model, and falls back to DuckDuckGo in the same search when the gateway cannot answer; a gateway that could not answer rests for ten minutes, with DuckDuckGo asked first meanwhile, the approval card names every place the query may go and who pays, auto mode leaves a paid search to the user, and a chosen service is still the only one used, DuckDuckGo chosen alone included.
- **All** — write_file can make a new file in the system's temp folder, such as a script that reproduces a bug, and write_file and edit_file can change one the session made there, so scratch work stays out of the project; another program's file there, or one in a folder the session did not make, is still refused, and a command that writes a new file there no longer counts as writing outside the project in a session that read a page.
- **All** — A run nobody watches that goes twelve rounds without changing the project is asked, on the end of that round's last result, to say in one line what it has established, its hypothesis and its next step, keeping on reading being a fine answer; again after twenty-four and thirty-six, the last saying to finish if it cannot name a step, and no more than three times a turn. Only an edit that went through starts the count again, never a command. `LETRONNA_PROGRESS_NOTE_ROUNDS` sets the rounds, with 0 for never, and `letronna -p --output json` reports `progress_notes`, `first_edit_round` and `rounds_after_last_edit`.
- **All** — `letronna -p --output json` says what the cache did over the run: `cache_misses` counts the replies the provider served fewer cached tokens than the one before though nothing sent had changed, with the tokens they lost, drops too small for `/usage` to name included, and `history_rewrites` counts the misses an earlier message changing caused, so a run's cache figures can be read net of the provider's own drops.
- **All** — A `reasoning_replay` setting, or `LETRONNA_REASONING_REPLAY` for one run, set to `chunked` sends DeepSeek and Kimi an empty `reasoning_content` with the turn's older tool calls once their reasoning comes to 8,000 tokens, and then a chunk of that at a time, so the requests in between keep the provider's prompt cache; the newest two calls always carry theirs, the calls before results already cleared go without it in the same rewrite, `/usage` and `letronna -p --output json` count a move of the cut as an earlier message changing, and a route that refuses the empty one is sent every call's reasoning instead, across turns, before the field is given up on. The default, `full`, sends every call of the turn its own, as before.
- **Linux** — Where commands get fewer CPUs than the machine has, as in a container held to a CPU quota, commands and jobs start with `OMP_THREAD_LIMIT`, `OMP_NUM_THREADS`, `OPENBLAS_NUM_THREADS` and `MKL_NUM_THREADS` set to the CPUs they get, unless the user set one of them or another thread-pool size already, and the system prompt says how many they get and names the four when it set them, as `OMP_THREAD_LIMIT` caps even a count a command sets itself. Where the counts match, the prompt and the commands are as before; the log's `machine` event gives both counts.

### Changed
- **Terminal** — Shift+Tab steps through asks first, edits without asking, plan only and auto decides (and bypass once chosen), never onto "allowed calls only", which refuses whatever would ask and stays with `/mode dont-ask`; Alt+M steps too, and each mode says in the footer for a moment what it lets run. The terminal starts in the window's default mode when no flag picks one, a mode switched while it works holds from its next tool call, and `/plan` takes on or off and goes back to the mode it was turned on from.
- **All** — A folder write_file or apply_patch created for a file counts as the session's own, so a session that read a page may delete it without asking, like the files it made, as long as nothing in it was there before it or is a link.
- **All** — A model whose window nothing gives is sized as if it held 64,000 tokens when its server takes a key, as a gateway or a LiteLLM proxy does, instead of the smallest window, so old tool results are cleared far later; a server without a key, such as Ollama, keeps the small window, and `LETRONNA_CONTEXT_WINDOW` sets it for a keyed proxy in front of a small local model.
- **All** — Old tool results are cleared only once a request comes near its budget, and then in chunks of half the window rather than every eight results, so the provider's prompt cache keeps serving the requests in between; results under 600 characters are never cleared, and a cleared one reads as cleared from history, with the call to repeat, rather than to save room.
- **All** — While the provider's prompt cache serves the session, the window holds up to four times the default cap: once the newest three replies' usage says at least half of their input came from the cache, a model whose window holds twice the cap keeps its history and its old tool results up to 256,000 tokens, or three quarters of its window if that is less, since a miss on everything after a cleared result costs more than the tokens it saves. The places the window can start, the chunks results are cleared in and the share of a tool result sent stay as under the cap, so nothing moves to a new place; a run of misses, an overflow, a route whose usage never counts the cache and a `context_cap` the user set keep the cap, and once the cache serves the session again the window and the cleared results stay where the cap left them rather than moving back. The 4,000 tokens kept free for subfolders' instruction files are kept only once a result has touched a folder that has one.
- **All** — `/usage` names a cache miss that cleared results or reasoning no longer sent back caused as "an earlier message changed", and says of one where nothing Letronna sends changed that the provider served it without the cache.
- **All** — web_fetch and search_web say why a request got no answer, such as a name that could not be looked up or a connection turned down, and once one to the web could not connect, other than one a page redirected to, both answer at once that they are unavailable for now, naming the host and why, rather than each trying and waiting to fail: for two minutes in a session someone watches and for the rest of a run nobody does, unless a request to the web gets through; pages on this computer or the local network are still fetched.
- **All** — `letronna -p` sends about 600 fewer tokens of tools and prompt with every request: the helpers' and the background jobs' lists are found through find_tools like the other less used tools, and nothing it is sent names what such a run lacks: helpers in the background, wake_me, list_sessions and /resume, or Letronna's own skill on using Letronna, whose index and skill tool go too when no other skill is on.
- **All** — A run nobody watches, such as `letronna -p`, is told the commands on PATH and what its project folder held at the head of its first message rather than in the system prompt, so runs in folders that hold different things send the same system prompt and tools, and a provider's prompt cache serves them across runs; sessions someone watches are unchanged.
- **All** — Claude models reached over chat completions through any gateway, such as a LiteLLM or Portkey proxy or a company's own, not only OpenRouter and the hosted gateway, mark where Anthropic's prompt cache ends, so each new message is written to the cache once and read from it on the next request instead of paid in full first. A route that turns the request down as bad (400 or 422), in whatever words, is sent it again once without the markers, while the requests beside it keep theirs; if that one is answered, that gateway and model go without them for the rest of the run, if it is turned down too the markers were not at fault and stay, and if it is stopped or never answered nothing is decided.
- **All** — The system prompt's rule on checking a change now says to run the check nearest it first, the reproduction and the one test file that covers the changed code, once, and then not to re-run, re-read or diff what passed, and to name a failure the change did not cause rather than prove it pre-existing; a new rule says a bug reported through one example is fixed at the cause its other inputs and entry points share, and what to do when a report does not reproduce as described; and the rule on making calls in one reply names commands and checks alongside reads and searches. A run nobody watches is also told that, with nobody to ask, the task text and the project folder are what it has, so it decides from them rather than searching the computer or the network for what was meant. The new rows are `shared-cause` and `scope-of-task` to `LETRONNA_PROMPT_RULES`.
- **All** — A call that brings back what an earlier one in the turn brought is told so on the end of its result ("Same result as round 3"), past the spellings that change nothing: a command with its spacing, a `cd` into the project, `2>&1`, `| cat` or a `| head -N` or `| tail -N` changed, a read of the same file starting within the same fifty lines, a search in another case, and a result that differs only in its commit ids, addresses, dates, timings or seeds. A third such result gets the note that already said to change approach, which now counts these too; a run nobody watches is still stopped only by the call spelled the same way five times. Only an edit that went through starts the count again, and a call turned down or made twice in one reply is not counted.
- **All** — When a call that runs on its own, such as an edit or a command, fails, the calls the same reply queued behind it are answered "Not run: an earlier call in this reply failed; resend it if still wanted" rather than run, so a check sent along with an edit never reads as if it had checked the edit that did not land. A command that ran and exited with a code is no failure here, so two suites sent together each report their own; reads and searches run at once and a miss among them stops nothing, nor does a memory save that fails, as one to a full page does, nor a call the user, the gate or a stop kept from running, such as a save the user did not ask for or a call to a tool there is not.
- **All** — read_file says what a page holds (about 7k characters, with `limit` left out) and that reads of several files or regions go in one reply; search says the same of several patterns or folders, and run_command of independent commands, each with its own exit code. Where calls run one at a time, those clauses go as the batching rule does. read_file names its kinds of file in a few words, search no longer spends a sentence on when documents are searched, and search_web's note on snippets is shorter.
- **All** — A run nobody watches, such as `letronna -p`, is told to remove what it made only to test from its outputs' folders before it ends, unless the task asks for it or the project's own build keeps it there, and to write only the answer, in the form asked, to a file meant for one; that leaving inputs alone and not deleting data hold for its own steps, not for what a program it was asked to write should do, which may run on more than the sample in hand; to get a result through the library or tool the task names, calling it correctly when a call fails rather than going around it; and to try a helper that transforms data on a tiny input whose answer it knows, then check the final output with separate code.

### Fixed
- **Terminal** — An approval card or a question from the model that comes up while you type no longer takes your keys: letters go on into the box behind it, an Enter pressed while you are still typing sends your line rather than a yes, and a digit no longer picks "always allow". A line sent while it worked never answers a question asked after it, a secret one included; Esc at the model's own question stops there, and a tall card keeps its choices on screen.
- **Terminal** — A message typed with `//` to start it with a slash is sent as words again by `/retry`, and comes back to the box that way, rather than being run as a command.
- **All** — A model its gateway lists only under another vendor's path, such as `deepseek/deepseek-v4.1-flash` listed as `opencode-go/deepseek-v4.1-flash`, is sized by that listed window instead of the smallest one, so old tool results are no longer cleared after a few thousand tokens.
- **All** — A web fetch or search that never reached its server, as when this computer is offline, no longer counts as having read a page, so a session that carries on from its own files is not asked about, or refused, its later deletes and sends; an error that quotes what a server wrote still counts, and the server's words in a failed secure connection, a redirect's address or a file's kind are now fenced as such.
- **All** — On DeepSeek and Kimi, the reasoning sent back with the tool calls of the turn in progress counts toward the window, so a long turn clears old results before its requests outgrow the model.
- **All** — A gateway that reports what its prompt cache wrote as `prompt_tokens_details.cache_creation_tokens`, as some that serve Claude over chat completions do, is read: a reply whose prompt the cache wrote counts it as input, written to the cache, instead of only the few tokens sent fresh, in the usage line, the window's fullness, `/usage` and `letronna -p --output json`.

## [0.1.0-alpha.3] - 2026-10-06

### Highlights
- **All** — Helpers that work side by side, fork, wait for more work and share a task list, plus workflows that run many agents from a script you approve.
- **All** — Auto mode: a judge on your own model decides what would otherwise ask, and on macOS and Linux commands can run confined by the OS instead.
- **All** — MCP servers over HTTP and OAuth, plugins from a git repository, and Zed or JetBrains running Letronna over ACP.
- **App** — The browser opens beside the chat at half the room; long output rests as short previews; live work sits in one line over the box; Files, Changes and Jobs tabs, a Ctrl+K palette, and the whole window reachable from the keyboard.
- **Chat apps** — Slack, pairing by code, files and voice notes both ways, and `letronna serve` to answer with no window open.
- **All** — Runs nobody watches finish what they were asked: they keep going after cut-offs and empty replies, finish their checklist, read their inputs, check the files they were asked to make, and are told nobody can answer. `letronna -p` takes a deadline, a JSON Schema for its answer and its server from the environment.
- **All** — Schedules in a mode of their own, memory you can review and edit, and a window that uses far less memory and processor while it works or sits idle.

### Added
- **All** — Letronna Cloud is asked for in the chat: say to run something there or keep a bot online 24/7, ask how it is doing, or to stop or restart it. `/cloud` is gone.
- **Web** — A Cloud page on letronna.ai shows what you host (bots and apps) with their output, Stop, Start, Restart and what each has cost, and the runs you sent.
- **Web** — Usage shows what text, media and the cloud cost, thirty days day by day, each model's published price beside what you paid, and how much you saved.
- **All** — Letronna Cloud runs bots and other always-on programs 24/7 when you ask for them to stay online: restarted if they stop, stopped after a crash loop or when credit runs out, billed by the hour. `letronna cloud apps`, `logs`, `down` and `up` manage them. Cloud compute now costs about $6 a GB-month, so a small bot online all month is about $0.75 of credit.
- **All** — Letronna Cloud keeps apps running: a cloud run can leave any server (Next.js, a Rust API, anything on a port) at its own address; it wakes on a visit, sleeps after 10 minutes without one, and is billed only while awake. Threads unused for 14 days are cleaned up.
- **All** — A cloud run that makes a page gives a private live link to it, served with nothing running; `letronna cloud relay on` has the cloud answer your Telegram chat while this computer is off; `letronna cloud support <run> on` lets support read a run, and each look is logged.
- **Web** — Cloud admin at letronna.ai/dashboard/admin, signed in with your account: the pool, running work, usage and the audit log.
- **All** — The agent picks where to build what you ask: one self-contained page, chart or small app as a file you preview first, then the project on this computer, and Letronna Cloud only when this computer cannot (with your yes, as it spends credit).
- **App** — Enter and Escape answer a card from the composer after a send; Tab moves past each switch once and Space turns it; Cmd+Q quits Letronna on macOS from anywhere in the window.
- **All** — Letronna Cloud (Pro): `letronna cloud "task"` in the terminal and `/cloud <task>` in the window run a task on Letronna's servers, in a repository you name or none, and bring the answer back; a thread carries the work on, asleep on the server between messages. Billed from your credit.
- **All** — `letronna doctor` checks the setup, and a local log and crash files help find out what went wrong.
- **App** — The window wears letronna.ai's colours; Settings choose its colours and whether it shows film grain.
- **All** — Letronna knows each model's context window and output limit.
- **All** — Helpers can answer in a checked shape.
- **All** — The model knows today's date.
- **All** — Send more while Letronna works: it reaches the model at the reply's next step, in the window and the terminal.
- **All** — Helper types, built in and from your own files, share one token budget across a whole tree of helpers.
- **All** — Helpers' conversations are kept beside their session, out of the session list.
- **All** — See, steer and stop helpers at work, and answer their approvals, in the window and the terminal.
- **All** — Fork a helper from the conversation, and ask something aside with `/btw`.
- **All** — Kept helpers wait idle for more work and wake on a message.
- **All** — A session's agents share a task list with owners and blockers.
- **All** — Helpers can answer through a checked schema, even at their round limit.
- **All** — Workflow runs keep a journal and replay what already finished.
- **All** — Letronna keeps notes on each project in a memory of its own, which refuses keys and planted instructions.
- **All** — `/init` has the model write the project's AGENTS.md, and AGENTS.md or CLAUDE.md can pull in other files with `@path`.
- **All** — `/usage` adds up tokens and cost per session, day and model; the usage line says how full the model's window was.
- **All** — Each session keeps its own model and effort.
- **All** — Your own hook commands can block a call or keep a turn going.
- **All** — The model sees pictures you attach and those its tools open.
- **All** — MCP over HTTP; servers take variables, tool filters and their own start time, restart after they stop, and reload when mcp.json changes. `letronna mcp` adds them.
- **All** — Many MCP tools are looked up on demand instead of sent with every request.
- **All** — Any tool can be switched off, and tools that cannot work are not offered.
- **Chat apps** — Talk to Letronna from Slack. A Telegram chat pairs by a code, and only its owner answers.
- **Chat apps** — Whole, formatted replies with Allow/Deny buttons; files, photos and voice notes go both ways.
- **Terminal** — Notices show and are taken mid-turn and between prompts; lines typed while it works show as queued and can be taken back.
- **All** — Send now: a queued message goes in without waiting for the step.
- **App** — Point mode in the browser panel: click a spot on the page, comment on it, and the reference goes in the composer.
- **All** — Workflows: scripts run in a sandboxed JavaScript engine, Letronna can run one it wrote once you approve it, and Crew (`@crew` or a switch) opts a message into orchestration.
- **All** — Helpers can work in a git worktree of their own.
- **All** — `/loop` and `/heartbeat` keep one session going; a session can keep working toward a goal until a check says stop.
- **All** — Run your own prompts and skills as slash commands, and let the model save, patch and undo skills it learned.
- **All** — Attach files, line ranges and folders by typing `@path`; the composer offers commands after `/` and references after `@`.
- **All** — Choose a reply style, or write your own.
- **All** — Replies can be read aloud as they stream; voice input records to text at the cursor.
- **All** — Pictures you attach can be edited through the chat's image gateway, and long video renders run as background jobs.
- **All** — Computer use asks once for each app, on macOS and on Linux under X11.
- **All** — The browser uploads project files, keeps downloads, saves a page as a PDF, shoots the whole page, hovers, drags, waits for words, acts in frames from other sites and runs steps in a batch.
- **All** — Fetched pages keep their links, and a PDF is saved as a file; web searches can keep to some sites, skip others or stay recent.
- **All** — MCP: read resources, run prompts as commands, sign in to servers that use OAuth, and tools get unique names and clean schemas.
- **All** — Plugins bring skills and MCP servers from a git repository.
- **All** — Other agents can use Letronna's browser and media tools over MCP, and Zed and JetBrains editors can run Letronna over ACP.
- **All** — OpenAI models change several files in one patch and one yes.
- **All** — New errors from a language server reach the model after edits, and a turn that changed files is asked once to build or test them.
- **All** — A task carries on with a fallback model when its own fails.
- **All** — A subfolder's AGENTS.md reaches the model when it works there.
- **All** — `/insights` shows the last weeks by day, hour, tool and model; `/diff` shows what changed in the project and in this session.
- **All** — Notifications call you back when a run ends or needs you.
- **All** — Continue a Claude Code or Codex session, in the terminal or the window.
- **All** — Memory makes several changes in one call, all or none.
- **All** — Keys and tokens move to the Keychain or Credential Manager.
- **Terminal** — Other programs can drive `letronna -p` over stdin as JSON lines, and `--mode accept-edits` edits inside the project.
- **App** — Ctrl+K opens a palette of actions, pages, sessions and models, and a global shortcut opens a quick entry bar from anywhere.
- **App** — Files and Changes tabs beside the chat: browse and preview the project, review and revert this session's edits, and stage, commit and push.
- **App** — Capabilities gains Tools and MCP tabs with per-tool switches, and a Plugins page.
- **App** — A first-run setup page, data-folder profiles, and sidebar filters for session state and project grouping.
- **App** — HTML and SVG artifacts can be previewed, and a failed turn says why in the chat and offers the fix.
- **App** — Set up Slack from the Messaging page and pair a member by code.
- **All** — Auto mode: a judge on your own model decides what would otherwise ask, with a tag on each call and a list of its decisions (`/decisions` in the terminal, a Decisions list in the window).
- **All** — On macOS and Linux, a command can run confined by the OS (no network, writes only in the project) instead of asking, in Accept edits and Auto.
- **All** — Saved workflows become slash commands, and `/review` ships built in; watch, pause, stop, resume, skip and retry workflow runs and their agents, in the terminal and the window, and read one agent's transcript or a run's log.
- **All** — Schedules run from core in whichever Letronna is open, never twice, each in a mode of its own; the model can add, list and remove them, and schedules, chat apps and `/bg` run beside the chat.
- **All** — `letronna serve` answers the chat apps with no window open, and the window takes over from it.
- **All** — Letronna can look back every 10 messages for what to remember; see and edit memory with `/memory` or the Memory page in Settings.
- **All** — Summarize earlier turns on demand, now or up to a chosen turn.
- **All** — Old sessions can be deleted after a number of days, and a rewind leaves nothing of what it took back in the file.
- **All** — A command still running at its time limit keeps running as a background job.
- **App** — Every part of the window can be reached from the keyboard; a run of tool calls folds into one line; a finished turn lists the files it changed; a Jobs tab shows background jobs live; a status-bar popover shows what fills the context.

- **App** — The browser opens beside the chat at half the room, and can take it all; while the agent browses, the page shows small in the corner of the chat.
- **App** — Long output rests as short previews: a command's last lines (a failed one in red, kept open), a file's first lines, an edit as +a −b with its diff, thinking as "Thought for 12s"; pictures open in the window's own viewer.
- **App** — Helpers, workflow runs and jobs still working, and the plan's steps, sit in one line over the box; while the model writes a long call the reply says "Writing app.rs…".
- **App** — Approval cards: Enter allows and Escape denies, and "2 of 3" shows when a reply asks about several calls.
- **App** — The sidebar gives the sessions the room, with the other pages under More; Ctrl+Tab switches between recent sessions; the empty chat lists recent sessions.
- **App** — Picking an image, speech or video model makes what you send with it.
- **All** — When a site's check stops the agent, it waits while you get past it and carries on by itself.
- **All** — The browser works on a site's own dropdowns, scrolls a panel by its ref, types into date, time, colour and range fields, names what a click hit, and waits for what a click set off before reading the page.
- **Terminal** — Skills and MCP prompts no longer crowd the `/` menu: they come up as you type, and under `/skills` and `/mcp`.
- **All** — A run nobody watches (`-p`, schedules, `/bg`, editors, chat apps) keeps going after a reply cut off or an empty one, is sent back once with checklist items still open, and checks the files its request asked for before it ends; `-p --output json` lists what is still wrong.
- **All** — `run_command` takes a timeout of up to half an hour for slow builds and tests.
- **App** — Previews open by themselves unless turned off.
- **Terminal** — Lists are steered with the arrows, and Escape declines.
- **All** — Harness-Bench scripts to score `letronna -p` and compare two builds task by task.
- **Terminal** — `letronna -p --json-schema` asks for the answer as JSON the schema checks; `--output json` names the session, model and cost and counts refused calls.
- **Terminal** — `LETRONNA_DEADLINE` warns a `-p` run to write its outputs at 80% of its time, then stops it; `AGENT_BASE_URL`, `AGENT_API_KEY` and `AGENT_MODEL` point a run at a server with nothing set up.
- **Terminal** — `letronna --dump-config` prints the settings and limits a `-p` run would get.
- **All** — `context_window` and `max_output` in settings, or `LETRONNA_CONTEXT_WINDOW` and `LETRONNA_MAX_OUTPUT`, give an unknown model its real limits; vLLM and Groq model lists set them too.
- **All** — `run_command` takes a `workdir`, and runs in bash where bash is on PATH; a headless run's long command carries on in the background.
- **All** — A schedule can run once at a date and time, and the model knows the computer's time zone.
- **All** — The browser can type a key for each character, and a typed field is read back so a batch stops when the text did not stick.
- **All** — A fetched page shows its forms with their hidden fields, and leaves out the words its markup hides.
- **All** — Setting `parallel_tool_calls` to false runs one tool call at a time; a helper type's effort, background and isolation take effect.
- **All** — `LETRONNA_ALLOW_HTTP=1` lets plain HTTP reach hosts on the local network; `LETRONNA_DISPOSABLE=1` lets an unwatched `--yes` run in a throwaway container delete a folder.
- **All** — Letronna runs as a Harbor agent for Terminal-Bench and SWE-bench, and ClawBench and the A.I.G injection matrix can be run against it.
- **All** — Releases carry a static terminal for x86_64 and aarch64 Linux.
- **All** — A session can work in a git worktree of its own and apply its work back when done.
- **All** — Scheduled runs can report by email, with the app password kept in the vault.
- **All** — A side model in Settings makes summaries, commit suggestions and goal checks on something cheaper.
- **All** — Memory changes can be listed and put back.
- **All** — A folder's `.local` instructions file is read after its shared one.
- **All** — Zed and JetBrains can reopen Letronna sessions and pick the model over ACP.
- **All** — An Anthropic key connects directly to Anthropic's API, with prompt caching and thinking; OpenAI reasoning models keep their reasoning between tool rounds through the Responses API.
- **All** — Memory: choose when a chat saves (only when asked, after a yes, or by itself) and where it keeps it (a project, the chat, or everywhere), from `--memory`, `--memory-scope`, `--private`, `/remember`, the window and chat apps.
- **All** — `read_file` reads Word, Excel, PowerPoint and PDF files as text, and search can look inside documents.
- **All** — A fresh install can search the web with DuckDuckGo, without a key.
- **All** — A background job can be answered at its prompts with `send_job`.
- **All** — Rewinding also undoes what a command changed in the project.
- **All** — A wake-up outlives a restart and can be a day away; a schedule can stay quiet unless something changed.
- **All** — A chat with no project works in Letronna's own folder.
- **All** — An experimental script tool can run many read-only calls in one step.
- **All** — Letronna has a built-in skill that explains its own commands and settings.
- **All** — Core can send a card waiting in the window to the owner's DM in a chat app.
- **All** — Editors over ACP can pick the agent's mode.
- **All** — Hand work to Claude Code, or pick it as a model.
- **All** — `read_file` reads OpenDocument text, spreadsheets and presentations, and EPUB books chapter by chapter; old .doc, .xls, .ppt and .rtf files say how to make them readable.
- **App** — The window shows a card for typing into a background job, fires wake-ups too, and can switch off DuckDuckGo search when no key is set.
- **App** — The Skills page can copy the built-in help skill to edit, and a memory row says where the entry went.
- **All** — Questions name a recommended answer, and the terminal takes free-form ones.
- **All** — Recurring tasks and notifications use Letronna's own schedules and chat apps first, with your yes.
### Changed
- **All** — Cloud runs show what they are doing as it happens: `letronna cloud` prints each step, and the window's `/cloud` shows the step under way, streamed over SSE (with polling if the stream drops). Apps in the cloud can stream to visitors (SSE) for as long as they like.
- **All** — A cloud run uses its own short-lived token that only reaches the models, never your sign-in.
- **All** — A failed model request says what went wrong: the key, credit, a rate limit, a busy provider, a conversation too long, and so on.
- **All** — OpenAI's reasoning models work, and a server that turns down a field is sent the request again without it.
- **All** — The context window counts tool calls, pictures and Chinese, Japanese and Korean text, so long conversations are trimmed where they should be.
- **All** — Following up while Letronna works keeps the turn's original request in view.
- **All** — The checklist stays in view, in the window and the terminal, even in a long task.
- **All** — Approvals you give in one session stay in that session.
- **All** — Tools accept other agents' argument names and quoted values, and `edit_file` finds text through different line endings and spacing.
- **All** — Commands run with no pager or editor to wait on, and colour codes are kept out of what the model reads.
- **App** — The approval card and tool lookups no longer reread files on every frame.
- **App** — The example plugin starts switched off.
- **All** — Batches of calls run at once.
- **All** — Session search finds words in any order, in the sidebar and `/resume`; the terminal lists sessions without reading every file.
- **All** — Earlier turns survive a failed summary, a summary survives a restart, and compacting shows.
- **All** — A project folder inside a repository keeps the repository's rules.
- **All** — Commands and MCP servers start with the login shell's variables.
- **All** — Stopping an MCP call tells its server, and stopping a server ends everything it started.
- **All** — Plan mode refuses tools that change things, however they are asked for.
- **All** — Secret files such as `.env` and private keys stay out of the model's reach; `.env.example` does not.
- **All** — Stop reaches waits and running commands, and ends them.
- **All** — Long replies are no longer cut off at two minutes; only five minutes of silence ends one.
- **All** — Unattended runs never stall waiting on a question.
- **All** — Summaries reuse the chat connection, and a busy background job no longer copies its whole log on every read.
- **All** — Runs without a window (`-p`, helpers, workflows) keep to deny rules and the safety floor, and a yes for the run no longer covers a worse call after it.
- **All** — Unattended runs that repeat one call with the same result are stopped.
- **All** — Helpers and workflow agents go through the whole gate, in their session's mode, with its rules, grants and judge.
- **All** — Jobs are listed, waits match only new output, and a session sees only its own jobs.
- **All** — A command printing past 4 MiB is saved whole for `read_file`.
- **All** — Chat-app, web and media calls reuse one warm connection each; cache misses are named with the part that changed.
- **All** — Inline think blocks stream as reasoning; `read_file` shows notebooks as cells.
- **App** — The window reads a session in only when it is opened, follows what the terminal writes to sessions, and highlights eight languages instead of twenty-seven.
- **App** — Session search skips sessions that cannot hold the words, and a reply that moves the window no longer waits for its summary.
- **App** — The window uses far less memory: pictures are read no larger than shown and let go of after, sessions read in by runs, chat apps or the terminal are let go of too, and switching profile reads only the session it opens.
- **Terminal** — The installed terminal is `letronnacli.exe`, with the logo; `letronna` still starts it.

- **App** — The window uses far less processor: a running reply redraws at 25 frames a second, not 120; a hidden or minimized window and the tray stay quiet; the browser panel decodes only pictures it shows; the approval card and the model list are worked out once.
- **App** — Selecting text no longer leaves 40–80 MB with the window, and video stills and a helper's transcript are let go of once not shown.
- **App** — Night is calmer: a midnight frame around a dark ink page.
- **All** — A command that exits non-zero counts as a failed step, and a web search row shows what was searched for.
- **All** — A workflow's tokens count up while its agents' replies stream.
- **All** — A `-p` run gets 100 tool rounds, retries like a scheduled run, and is told nobody is watching, with the tools it may not use left out.
- **All** — The reminder to check edits covers patches, and only a clean, non-read-only command settles it; edits to .md or .txt files alone do not ask for it. A run nobody watches is told which files it changed after its last passing build or test.
- **All** — Text from files and commands that addresses an AI gets a Letronna note, and the summary of dropped turns is marked as derived text.
- **All** — Tool errors name the next step, and a command that prints nothing says so.
- **All** — `LETRONNA_READ_TIMEOUT` sets how long a reply may stay silent.
- **All** — A run is told before it ends which input files it never read, and a run nobody watches is told what is on PATH and in its folder.
- **All** — Writes and edits name the file they changed; a CSV left with uneven rows gets a note; refused dangerous commands suggest a narrower one.
- **All** — Files, command output, pages and MCP results lose characters nobody can see, with a note saying how many; `edit_file` finds text that differs only by them.
- **All** — After untrusted content, risky calls are checked even under `--yes`; skills the model writes stay off until you switch them on, and installed skills reach the model cleaned and marked.
- **All** — A helper's report comes quoted under a header; a skill being followed stays in view as old results are cleared, and a cleared result that was saved still names its file.
- **All** — Reasoning models get the reply budget as `max_completion_tokens`; a full search page reaches the model whole; search names folders a `.gitignore` above the project hid.
- **All** — Reading every page of an unchanged file counts as reading it whole; a picture too big to send is scaled down; a PDF `web_fetch` saves can be paged with `read_file`.
- **All** — A goal's failing check command keeps the goal from being met, and `/review` covers new files and diffs from where the branch left.
- **Terminal** — An unknown flag is an error, continuing stays in this folder's session, `--resume` under `-p` needs an id, and `-p` warns in Default mode that writes will be refused.
- **All** — The agent is told when the session's mode changes, when you stopped its reply, which MCP servers are not ready, and, on long turns, the rules still in force (off unless chosen).
- **All** — A tool that keeps failing is followed by a note to change approach; the goal check asks for evidence of each part.
- **All** — Suggested commit messages follow the repository's own style.
- **All** — Claude Code hooks Letronna does not run are left out with a notice; a window turn in a scheduled run is offered only what the run may call.
- **Chat apps** — Slack shows bold, headings and links its own way.
- **All** — Memory is saved only when you ask, unless you choose otherwise.
- **All** — An edit that misses shows the closest text and what differs; a write that breaks a file's syntax says so.
- **All** — A long page can be read past its first screen, and web answers come from the page that settles them, with its address.
- **All** — Files a patch wrote show in Outputs and reach editors as edits; files the request says not to modify are named if a run changed them.
- **All** — A blocked goal gets one more try, then pauses instead of being dropped.
- **All** — Plan mode can keep every tool, so switching it keeps the cache (off unless chosen).
- **All** — The gate reads a call's arguments under the names the tool accepts; `run_command` says how to read its result and that it has no keyboard.
- **All** — The checklist tool says when to use it and what done means; a summary keeps your rules word for word and marks what is done. `/init` writes gotchas as short rules, without a tour of the folders.
- **All** — Shorter, clearer instructions to the model: replies lead with the answer, say what counts as done, batch calls, ask first when unsure, and the user wins over a project file; fifteen tools now say each fact once, 490 tokens off every request.
- **All** — Plan mode treats a request as a plan to make; helpers are told what they cannot do and report what they checked; non-Claude models take changing facts from a tool.
- **All** — Text from outside that tries to instruct the agent is named, never copied; the untrusted-text rule is said once.
- **All** — Auto mode allows project installs and local git but not pushes nobody asked for; an unwatched `--yes` run deletes a folder only inside its project; a redirect is judged where it lands.
- **All** — Outputs are held to the values, keys and columns a request lists, and only code a folder can build or test sends a run back to check it.
- **All** — Chat-app replies use lists, not tables; terminal replies use bold lines and lists.
- **All** — Memory guidance is shorter and keeps facts, not orders or failures; saved skills keep steps that worked; a memory or skill change no longer rewrites the cached rules.
- **App** — The window is built on GPUI Kit 0.7.1; panels and new messages ease in (still with reduced motion); Settings opens over the blurred window on X11 and macOS; Tab reaches every switch in Settings.
- **App** — The title bar shows Letronna's name on the left instead of the session's, and About is shorter.
### Fixed
- **All** — A command that uploads a key file (curl, wget, scp, rsync) is refused in every mode, and .gnupg, .pgpass, .vault-token, .azure and gcloud settings count as secret.
- **All** — An edit whose old text is missing says so instead of "already applied", plain text is no longer saved as a .docx, .xlsx, .pptx, .zip or .pdf, and a command that exits non-zero counts as failed.
- **App** — Deleting the last session no longer crashes the window, a new session no longer inherits a deleted one's pictures, and a finished empty reply no longer shows Thinking forever.
- **App** — New session goes to the top, even when it reuses an empty one.
- **App** — Closing settings or the browser panel gives their pictures back to the GPU.
- **App** — New session opens the empty one instead of piling up more, the last session in the sidebar scrolls fully into view, and right-clicking a session no longer draws a black halo.
- **All** — Catastrophic commands, such as deleting your home folder, are refused whatever the approval setting.
- **All** — Two copies of Letronna never mix their writes to one session, and a crash part way through a save costs one line, not the session.
- **All** — Keys, settings and memory files are never silently reset or wiped by a read that failed, and a broken skills.json is never replaced by the defaults.
- **All** — A session deleted in one copy of Letronna stays deleted, and a leftover lock file no longer lists a session twice.
- **All** — Commands never hang on a child process they leave running, and output that is not valid UTF-8 survives.
- **All** — `web_fetch` checks every address and redirect it follows.
- **All** — Streamed tool calls are put together per call.
- **All** — A project's AGENTS.md can no longer hang the app by linking to a device.
- **Terminal** — `letronna -p` no longer keeps a processor core busy while a reply streams.
- **All** — Two copies of Letronna starting at once never write into one session.
- **Terminal** — Background jobs stop when the terminal exits.
- **All** — Follow-ups and notices sent mid-turn keep the turn's request in view.
- **All** — `/btw` shares the session's cache and leaves no stale locks.
- **App** — The window opens again on a session that has messages.
- **Terminal** — A bad update is put back, and a good one is kept.
- **Terminal** — A Ctrl-C of `letronna -p` ends the command it was running.
- **Terminal** — A `-p` run never installs a staged update or puts an old one back, and a SIGTERM or a closed terminal stops it as Ctrl-C does.
- **All** — A command stopped at a terminal prompt is ended and says why, and a page's error answer comes back with what the page said.
- **All** — An image edit on a gateway without the route points to a script instead of failing.
- **Terminal** — `letronna -p` over stream-json no longer hangs on Windows when it starts.
- **App** — Clicking the title bar's search box opens the palette again, instead of starting a window move.
- **All** — A reply the provider cuts off part way is asked for again, and a call whose arguments are not JSON no longer ends the run.
- **All** — Sandboxed commands start on Linux instead of failing to spawn.
- **App** — The rewind question says plainly what goes and what is not undone, and card hints read properly in Paper and narrow windows.
## [0.1.0-alpha.2] - 2026-09-27

### Added
- **App** — Right-click or hover a turn to copy, edit, retry, rewind or fork it.
- **Terminal** — Take back or retry the last exchange, or branch a session.
- **All** — A real browser: Letronna reads pages, clicks, types and takes screenshots. In the app it sits beside the chat, where you can watch it and use it yourself.
- **App** — Each session has a browser of its own, or one shared by all, and it can be a cloud browser or Chrome on another machine.
- **All** — Letronna updates itself: the app finds, checks and installs new versions, and `letronna update` does it in the terminal.
- **All** — Search the web with your own search service.
- **All** — MCP servers reached by URL, with headers such as a key; one-shot questions wait for MCP servers to start.
- **All** — Rewind a session, and it stays rewound.
- **Terminal** — The guide is a click away, from `/help`.
- **All** — Letronna makes images, speech and video, and turns speech into text; pick which model makes each.
- **All** — Letronna remembers: `SOUL.md` says who it is, `USER.md` and `MEMORY.md` hold what it knows about you and its own notes, and a project's `AGENTS.md` is read as its instructions.
- **All** — Tools from MCP servers, set up in `mcp.json`.
- **All** — Read a web page.
- **All** — Search past sessions.
- **Terminal** — `letronna`, the agent in a terminal: the conversation, tools, diffs, `/resume` by name, `/model`, `/effort`, `/thinking`, `/search`, `/plan`, `/jobs` and `/skills`.
- **All** — One Letronna account for the app and the terminal: `/login`, `/whoami`, `/logout`, and an Account page in the app. Signing in happens on letronna.ai.
- **All** — Letronna's hosted models on your account, paid from your credit, with no key to paste.
- **All** — Use every gateway you set up at once: the model list gathers models from Letronna and each gateway with a key, and a message goes to the gateway of the model you pick.
- **All** — Gateways as a status list (signed in, connected, not set up, not running): Settings > Gateways in the app, `/gateways` in the terminal.
- **All** — Teams: bill hosted models to a team's shared pool or to yourself, with `/team` in the terminal or Settings > Account in the app.
- **All** — Effort, thinking and web search, where the gateway supports them.
- **All** — Scheduled jobs, background jobs, a checklist, plan mode, and plugins that add tools.
- **App** — Reach Letronna from Telegram and Discord, and scheduled jobs report to every channel.
- **App** — The Artifacts and Skills pages.
- **macOS** — macOS builds, with native fonts, paths and a proper .app.
- **Linux** — Install and tray on Linux.
- **All** — Install in one line, then a few questions.

### Changed
- **App** — Type while it answers, press Esc to stop, and deleting asks first.
- **App** — The chat reads as a column, with your messages in cards and no flash when switching sessions.
- **All** — Signing in no longer waits forever for the browser, ends on a page that looks like the website, and while waiting offers to open the page again, copy it or cancel.
- **Windows**, **macOS** — Releases can be code signed, so Windows and macOS stop warning about an unknown publisher.
- **All** — Old turns are summarized instead of forgotten when a conversation outgrows the model's window.
- **All** — Model calls reuse one connection, and search skips files without a match before reading them.
- **All** — Letronna is the default gateway, and the only hosted one.
- **All** — Each session is its own file, so two copies of Letronna can run at once.
- **All** — Settings saved on a gateway that is no longer offered move to your Letronna account.
- **Terminal** — Questions are asked inside the input box, above where you type.
- **All** — A turn that fails says why; tools work on gateways that rename them.

### Fixed
- **All** — Names with accents are no longer garbled by signing in.
- **Linux**, **macOS** — Keys and sign-in files are readable by you only.
- **All** — Your keys and sign-in stay out of the model's reach.
- **Terminal** — Sessions keep their pins and todos when the terminal saves them.
- **Windows** — Pastes arrive as one paste, commands run without flashing a console window, and the wheel scrolls.
- **All** — Context no longer lost on large tool results; dangling tool calls are repaired.
- **App** — Asking the Discord bot to read the chat no longer hangs the app.
- **macOS** — The window's buttons stay visible when another app has focus.

## [0.1.0-alpha.1] - 2026-09-18

### Added
- **App** — The first alpha of the Letronna desktop app: chat with an agent that can read, write and run things in your project, with approvals, sessions, and a choice of gateway (OpenAI, OpenRouter, Ollama or any OpenAI-compatible server).
- **Windows** — Installs itself on first run, with a Start Menu entry and a tray icon.
