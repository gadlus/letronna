# Adding a provider

A provider is a service Letronna sends model requests to: OpenAI, Anthropic, OpenRouter, Ollama and so on. Each one is a folder under `crates/core/src/providers/` plus one line in the registry. Everything they share lives outside those folders: building requests, reading replies, retries and keeping keys in the system's vault. So a new provider needs no change to the core.

If you'd like a provider added but don't want to write it yourself, open a [provider request](https://github.com/gadlus/letronna/issues/new?template=new-provider.yml).

## What a provider says

A provider implements the `Provider` trait in `providers/mod.rs`:

| Method | What it gives |
|---|---|
| `info()` | Its name, base URL, one-line blurb, docs link and tier |
| `auth()` | What it needs: `ApiKey`, `None` (runs locally), `Account` (Letronna sign-in), `Url` (the user gives the base URL) or `OAuth(&SPEC)` (a sign-in in the browser) |
| `wire()` | The format its base URL speaks. Defaults to OpenAI chat completions. |
| `models()` | The model ids it serves. Defaults to `GET <base>/models`. |

The **name** is how settings, saved keys and fallbacks refer to the provider, so it never changes once shipped.

The **tier** says what using it costs: `Subscription`, `ApiKey`, `Credit` or `Free`. Fallbacks reach for them in that order.

## Step by step

1. **Copy the template.** Copy `providers/_template/` to a new folder named for the provider in lowercase, for example `providers/mistral/`. Rename the struct.
2. **Fill in `info()`.** Use the base URL of its OpenAI-shaped API, ending in the version (`https://api.example.com/v1`, no trailing slash). Link `docs` to the page where people get a key.
3. **Pick `auth()` and `wire()`.** Most providers take an API key and speak OpenAI chat completions, and then there is nothing to change. If it speaks Anthropic's Messages format, return `Wire::Anthropic`; if only OpenAI's Responses API, `Wire::OpenAiResponses`.
4. **Register it.** In `providers/mod.rs`, add `mod mistral;` and one line to `PROVIDERS`, in the place it should appear in the gateway list.
5. **Run the suite.** `scripts/x.sh test -p letronna-core --lib providers::`

The suite runs the same checks on every provider:
- its name is unique, and it has a blurb and an `https` docs link;
- its base URL is well formed and speaks the format `wire()` names;
- it reads the model list its format sends, from a reply recorded in `providers/fixtures/` and served from your machine.

No real key is needed, locally or in CI.

Override `models()` only if the provider lists its models somewhere other than `GET <base>/models`. If you do, add a recorded reply to `fixtures/` for it.

## One provider per pull request

A provider PR touches only its own folder and its one line in `PROVIDERS`, plus a recorded reply in `fixtures/` if it needs its own. That keeps reviews short, and lets a provider be dropped without touching any other.

Say in the PR which models you tried it with, and how (a real key, or the recorded replies only).

## Keys and tokens

Keys are saved in the system's vault: macOS Keychain or Windows Credential Manager. A provider never reads or writes a key itself; the gateway hands it the one saved under its name. Never log a key, put one in a fixture, or write one to a file.

## Browser sign-in

A provider signed in to in the browser describes its OAuth sign-in as data, a `static` `OAuthSpec` (`providers/auth/`): its authorize and token URLs, client id, scopes, the loopback port and path the browser comes back to, any extra authorize parameters, whether the token endpoint takes JSON or a form, PKCE, and where the account id and plan are in the id token. Return `AuthKind::OAuth(&SPEC)` and nothing else is needed: `providers::auth` opens the browser, waits on the loopback (or takes the address or `code#state` pasted back), trades the code for tokens, keeps them in the vault under `provider:<name>`, and refreshes the access token shortly before it ends or once when it is refused. `providers/chatgpt/` is the example.

## Local endpoint

Settings › Accounts › Local endpoint serves the connected accounts as an
OpenAI-compatible API at `http://127.0.0.1:8642/v1` (`local_port` in
settings.json picks another port). It is off until turned on, listens on
127.0.0.1 only, and refuses any request without
`Authorization: Bearer <token>`; the token is made on this machine, kept in
the system vault, and copied from the same page.

- `GET /v1/models` lists every connected account's models.
- `POST /v1/chat/completions` takes `model`, `messages` and `stream`, and
  sends through the router, so a rate-limited account hands the request to
  the next one as a session's would. Tools are not passed through.

The code is `crates/core/src/providers/local.rs`.
