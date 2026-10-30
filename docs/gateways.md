# Gateways and models

A gateway is a model provider. You can set up as many as you like. The model menu lists all of their models, and each request goes to the gateway of the selected model.

| Gateway | Base URL | Key |
|---|---|---|
| Letronna | `https://api.letronna.ai/v1` | Sign in |
| OpenAI | `https://api.openai.com/v1` | Required |
| ChatGPT | `https://chatgpt.com/backend-api/codex` | Sign in (plugin) |
| Claude | `https://api.anthropic.com/v1` | Sign in (plugin) |
| Anthropic | `https://api.anthropic.com/v1` | Required |
| Gemini | `https://generativelanguage.googleapis.com/v1beta/openai` | Required |
| xAI | `https://api.x.ai/v1` | Required |
| OpenRouter | `https://openrouter.ai/api/v1` | Required |
| OpenCode Go/Zen | `https://opencode.ai/inference/…`, one address per format (see below) | Required |
| This computer | Runs here (see below) | None |
| Ollama | `http://localhost:11434/v1` | None |
| Custom | Any OpenAI-compatible server, or one that speaks Google's own format (see below) | Optional |

- **Model lists** come from `GET {base}/models`.
- **Streaming** uses SSE chat completions.
- **Formats**: every request is built as an OpenAI chat completion and goes out in the format its gateway serves the model in: chat completions, Anthropic's Messages, OpenAI's Responses API, or Google's own `generateContent`. A Custom gateway whose base ends in `/v1beta`, such as `https://generativelanguage.googleapis.com/v1beta`, is spoken to in Google's format, its key sent as `x-goog-api-key`. The Gemini gateway itself uses Google's OpenAI-compatible endpoint, which keeps Gemini's thought signatures as well.
- **Context limits**: Letronna knows each model's context window and output limit. When a conversation outgrows the window, older turns are summarised instead of dropped.

Manage them in Settings › Accounts, or with `/connect`. The page is built to fit on about one screen:

- **Connected**: each account on one line, with its status and the plan window nearest its limit ("Weekly · 69% left", or "5-hour · back at 14:20"). A click opens it in place, with every window, **Use first**, and its key or **Sign out**; another click closes it.
- **Add an account**: a tile for each provider not yet set up, saying how it is paid (Plan · sign in, Plan · API key, API key, Runs on this computer). A click opens its sign-in or key field under the tiles. **Your own server** adds a gateway of your own, and a provider that comes in a plugin that is off says **Turn on in Plugins**.
- **More options**, folded: the order for when one runs out, and the local endpoint.

## More than one account

A provider you pay with a key or a sign-in takes more than one account: a work ChatGPT plan beside a personal one, a second Claude subscription, another OpenAI or OpenCode Go/Zen key. In Settings › Accounts, **Add another ChatGPT account** under the provider's last account asks for a name, such as Work, then the same key or sign-in as the first. In the terminal, `/connect` offers the same. Letronna, This computer, Ollama and Custom take one each; a second server is a gateway of your own.

- Each extra account is a gateway called "ChatGPT · Work", listed under its provider in the order you give (the arrow, or **Use first** in `/connect`). Its key or sign-in is kept apart from the first account's, and `--gateway "ChatGPT · Work"` picks it.
- The model menu and `/model` list a model once for all of a provider's accounts, on the first one in your order that has room.
- **When one runs out**: with **Go on with my next account of the same provider first** on (Settings › Accounts › More options › When one runs out; on by default), a request that an account turns down for a full plan or a rate limit goes on with the same model on the next account of that provider, before any other gateway. The switch line says why: "Switched to ChatGPT · Work: ChatGPT reached its limit. It is back at 14:20." An account whose plan is full is passed over until its window starts again. Some providers' terms do not allow using more accounts to get past a plan's limits.

## Plan usage

An account opened in Settings › Accounts shows a bar for each window of its plan, with how much is left and when it resets (closed, it shows the one nearest its limit): ChatGPT's 5-hour and weekly windows, Claude's 5-hour, weekly and any model's own weekly window, and OpenCode Go's 5-hour, weekly and monthly ones. `/limits` shows the same in the terminal.

- Each reply on a plan says where its windows stand, and Letronna also asks the plan's own usage page (ChatGPT's `wham/usage`, Claude's `oauth/usage`, OpenCode Go's `usage`) when the page is opened, `/limits` is typed or **Check now** is pressed: at most every 5 minutes on its own, or 30 seconds when you ask.
- What is learned is kept in `usage.json` in the data folder, so the window, the terminal and a paired phone see the same. It holds percentages and times, never a key.

## OpenCode Go/Zen

One key from the [OpenCode console](https://opencode.ai/auth) reaches both of OpenCode's offers, so they are one gateway:

- **[Go](https://opencode.ai/docs/go/)**, the monthly plan for open coding models (GLM, Kimi, Qwen, MiniMax, DeepSeek, Gemini and more). With a Go plan on the key, the models Go lists go to Go's addresses, under `https://opencode.ai/inference/go/`, and count against the plan.
- **[Zen](https://opencode.ai/docs/zen/)**, pay-as-you-go from your OpenCode balance, for every other model OpenCode serves (Claude, GPT, Gemini, Qwen, Kimi, MiniMax and more) under `https://opencode.ai/inference/`. A key with no Go plan uses Zen alone.

How it is sent:

- Each format has its own address, as OpenCode lists them: `openai/v1` for chat completions and the Responses API, `anthropic/v1` for Messages, `google/v1beta` for Google's format, the same under `go/` for Go. The lists of models are at `inference/v1/models` and `inference/go/v1/models`.
- Most models answer on chat completions. Those served only in another format go there: Anthropic's Messages for Claude and Qwen 3.x models (on Zen, all but the Max) and for MiniMax (on Go, M3 and M2; on Zen, the free ones), the Responses API for GPT, Grok and Muse Spark, and Google's own format for Gemini, its thought signatures kept with each tool call and sent back with it. Jev, served only in a format of its own (SystemOne), is left out.
- Every request carries the `x-opencode-session` header OpenCode asks every client for, the same for a whole session, and names Letronna as its client.
- A refusal for a full Go window (`GoUsageLimitError`) is a rate limit: the request goes on with another account, and Go's models on that key are passed over until the time it gave. They never move to Zen on their own; whether a full plan goes on from your balance is Go's own **Extra usage** setting. Zen's models are not held back by the plan.
- Usage shows the Go plan's windows (Go 5-hour, Go weekly, Go monthly). For a key with no Go plan it says so: "No Go plan on this key: pay-as-you-go on Zen".

## From a phone

A paired phone (Settings › Computers › your computer, in the Letronna app) lists this computer's accounts with how much of each plan is left, adds a key or a sign-in, orders them and turns the switch above on or off. The accounts stay on the computer: a key goes from the phone to the computer once and is checked there by listing its models; nothing is sent back but names, plans and usage. A plan is signed in to on the phone: ChatGPT with a code typed on OpenAI's page, Claude by pasting back the code its page shows. `/connect` in a terminal over SSH offers the same **Sign in with a code**.

## When one runs out

When a request fails in a way another gateway could answer (out of credit, a model your Free plan does not include, rate limited, overloaded, or unreachable), it goes on elsewhere, and a line says where:

1. The same model on your other gateways that list it, in the order set in Settings › Accounts › When one runs out. Gateways at the session's tier or later come first, then the earlier ones. Your own gateways, Custom, and gateways added by plugins count as API keys.
2. The models in your fallbacks list.
3. Your default model, if it is on a different gateway.
4. When the gateway could not be reached at all, the best model downloaded to this computer, if one is (see below). Turn this off in Settings › On this computer.

A model that failed is skipped for five minutes, once another one has answered in its place. If they all fail, you see the error from the session's own model.

A chat with a model on this computer never moves to another gateway.

## On this computer

Settings › On this computer downloads models that then run on this computer, with no internet and no key. The page says what this computer has (memory, graphics, the backend) and, for each model, whether it runs well here and about how fast, before anything is downloaded; it marks the one that suits it best. First-run setup offers that one too.

| Model | Maker | Download |
|---|---|---|
| Ternary Bonsai 2 · 4B | PrismML | 1.2 GB |
| Gemma 4 · E4B | Google | 4.6 GB |
| Ternary Bonsai 2 · 9B | PrismML | 2.6 GB |
| Qwen3.5 · 9B | Qwen | 5.7 GB |
| Ternary Bonsai 2 · 27B | PrismML | 7.6 GB |
| Qwen3.6 · 35B-A3B | Qwen | 18 or 22 GB |
| Qwen3.8 · 27B | Qwen | 18 GB |
| Qwen3-Coder-Next | Qwen | 38 GB |

- **Downloads** can be paused, resumed (after a quit too) and cancelled. A file is used only once its size and SHA-256 match, and it is kept out of Time Machine.
- **llama.cpp** runs them: its server, build b11809, downloaded with the first model and checked by its SHA-256. It uses Metal on a Mac, Vulkan or CUDA on a graphics card, else the processor, and falls back to the processor once if the card cannot load a model.
- **One model at a time** is loaded, shared by the window and the terminal; a second chat waits its turn and says so. It is let go after ten idle minutes or when memory runs short.
- **Tools**: these models chat. Ternary Bonsai 2 · 4B and 9B, which passed a test of when to search, may also search the web while this computer is online. No other tool is offered.
- **Photos**: the first photo sent to a model asks for its vision pack (about 370–930 MB), downloaded once.
- **Source**: each model links to the Hugging Face page its file comes from, with its model card and licence.

In the model menu they are under **On this computer**; in the terminal, `/model`, or `letronna --gateway "This computer" --model bonsai-2-4b`.

### A model from a Hugging Face link

**Add from Hugging Face**, under the list, takes a link to any public GGUF model: its page (`https://huggingface.co/unsloth/Qwen3.5-9B-GGUF`), a file's `blob` or `resolve` link, `hf.co/owner/model`, or `owner/model:Q4_K_M` as llama.cpp and Ollama take it. Letronna asks the hub what the repository holds and lists its GGUF files, each with its size and whether it runs well here; a 4-bit one is suggested, or the one the link named. The one you pick is downloaded like the models above: pinned to the commit it was read at, and kept only once its size and SHA-256 match what Hugging Face said of it. A vision file (`mmproj`) in the same repository becomes its photo reading.

- Models that are split into several files, or that need a Hugging Face sign-in, can't be added; the page says which.
- What it needs is estimated from its parameters, as nobody has measured it here; once it answers, its speed is timed as the others' is.
- It chats only, without web search, until a model is known to call tools well.
- Deleting it takes it off the list; paste its link again to get it back. The list is `models/added.json`.

In the terminal, `/local` (or `letronna local`) lists every model with its verdict, its download and its Hugging Face page; `/local get <id>` downloads one, `/local add <link>` adds one from a link, offering its files to pick from, and `pause`, `resume`, `cancel` and `remove` take an id too. `letronna local get` and `letronna local add` wait for the download and show how far it has got; Ctrl-C stops it, and `letronna local resume <id>` carries on.

## Hosted models

When you are signed in, the Letronna gateway serves chat models from several providers with no key. Each request is paid from your credit at the prices on the [models page](https://letronna.ai/dashboard/models).

- **Credit**: bought from $1, and it never expires. Once it runs out, hosted requests are refused. Your own keys keep working, and requests move to them as described above. On the Free plan, a refused request offers **Upgrade subscription**, and in the terminal a line links to the upgrade page. **Add credit** opens the credit page instead.
- **Free models**: some hosted models are free each day, as the models page and your dashboard list them. On a Free plan with no credit, those stay usable while the rest are greyed out: the window's picker marks them **Free today**, and the terminal's `/model` marks them "(free today)". Each comes with a daily allowance; once it is used up, the model is greyed out with the others ("free · used up today" in the terminal) until 00:00 UTC, or until you upgrade or add credit.
- **Pro** ($20/month): $20 of credit each month, lower prices, higher rate limits, and Letronna Cloud.
- **Team** ($18 per seat per month, 2–5 seats): a shared credit pool, per-member spending limits and an audit log. Each machine picks who pays with `/team`.

Spending is shown per day and per model on the [usage page](https://letronna.ai/dashboard/usage).
