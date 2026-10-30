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
| `quota()` | How full its limits are, read off a reply's headers. Defaults to the standard rate-limit headers. |
| `usage()` | Each window of a plan, asked of the provider's own usage page. Defaults to none. |
| `wire_for()` | The format one model goes out in on a given key, for a provider that serves some models in another than its base URL speaks. Defaults to the address's. |
| `base_for()` | Another base URL for one model on a given key, for a provider whose key reaches more than one address, such as a plan's and a pay-as-you-go one. Defaults to its own. |
| `plan_covers()` | Whether a model on a given key is paid by the plan whose usage `usage()` reports, so a full plan holds back only the models it pays for. Defaults to yes. |
| `headers()` | Headers every request carries beyond the key, given a mark that stays the same for a session. Defaults to none. |

The **name** is how settings, saved keys and fallbacks refer to the provider, so it never changes once shipped.

The **tier** says what using it costs: `Subscription`, `ApiKey`, `Credit` or `Free`. Fallbacks reach for them in that order.

## Step by step

1. **Copy the template.** Copy `providers/_template/` to a new folder named for the provider in lowercase, for example `providers/mistral/`. Rename the struct.
2. **Fill in `info()`.** Use the base URL of its OpenAI-shaped API, ending in the version (`https://api.example.com/v1`, no trailing slash). Link `docs` to the page where people get a key.
3. **Pick `auth()` and `wire()`.** Most providers take an API key and speak OpenAI chat completions, and then there is nothing to change. If it speaks Anthropic's Messages format, return `Wire::Anthropic`; if only OpenAI's Responses API, `Wire::OpenAiResponses`. Google's own `generateContent` is `Wire::Google`, for `wire_for()` alone: a model served only in it goes there, with `base_for()` naming the address, as OpenCode's Gemini models do.
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

## More than one account

A provider whose `auth()` is `ApiKey` or `OAuth` takes more than one account with no code of its own: `providers/accounts.rs` keeps each extra one in `accounts.json` as a gateway called "<name> · <label>", `providers::find` resolves it to the provider, its key or sign-in is kept under its own name, and requests carry the account they are on (`chat::Connection::gateway`). A provider needs nothing more for it, as long as what it does with a key or sign-in goes through the account it is given rather than its own name.

## Browser sign-in

A provider signed in to in the browser describes its OAuth sign-in as data, a `static` `OAuthSpec` (`providers/auth/`): its authorize and token URLs, client id, scopes, the loopback port and path the browser comes back to, any extra authorize parameters, whether the token endpoint takes JSON or a form, PKCE, and where the account id and plan are in the id token. Return `AuthKind::OAuth(&SPEC)` and nothing else is needed: `providers::auth` opens the browser, waits on the loopback (or takes the address or `code#state` pasted back), trades the code for tokens, keeps them in the vault under `provider:<name>`, and refreshes the access token shortly before it ends or once when it is refused. `providers/chatgpt/` is the example.

A sign-in can also be finished on another device, as a phone or a terminal over SSH needs: a spec with `device` signs in by a code typed on the provider's page (ChatGPT's, as Codex's device sign-in does), and one with `manual_redirect` sends the browser to a page that shows a code to paste back (Claude's). `auth::begin_away` starts either.

## Offered by a plugin

A provider can be one a window plugin switches on, as ChatGPT is: its
`plugin()` returns the plugin's id, and until that plugin is on it is left
out of the gateways, the model list, the router, the local endpoint and
`/connect`. Everything else about it is the same as any provider's. The
window's plugin calls `providers::plug(id, on)` and saves the switch in
`plugin-switches.json`, where the terminal reads it; `plugins.json` is the
marketplace's and is never written by a switch. Its account row is shown
under the plugin in Settings › Plugins rather than on the Accounts page.

ChatGPT starts on for anyone already signed in to it when it became a
plugin, and off for everyone else; after that the switch is the user's.
Its plugin can also use the Codex app's sign-in instead of one of its own:
`~/.codex/auth.json` (or `$CODEX_HOME/auth.json`) is read on every request
and never refreshed or written. When ChatGPT refuses that token, the user is
told to use Codex once so it renews its sign-in.

## From a plugin

A plugin installed with `/plugins` can add providers without any code: an
OpenAI-compatible API is data in its `plugin.json`.

```json
{
  "name": "acme",
  "version": "1.0.0",
  "providers": [
    {
      "name": "Acme",
      "base": "https://api.acme.example/v1",
      "blurb": "Acme's hosted models. Needs an Acme API key.",
      "docs": "https://acme.example/keys",
      "tier": "api_key",
      "auth": "api_key"
    }
  ]
}
```

- `name`: up to 40 letters, digits, spaces, `-`, `_` and `.`. A name a
  built-in provider has (any case) is refused, and the plugin's page says
  so; the first plugin to give a name keeps it.
- `base`: where its `/models` and `/chat/completions` are, `https://` or
  `http://127.0.0.1`.
- `tier`: `subscription`, `api_key` (the default), `credit` or `free`, which
  places it in the fallback order.
- `auth`: `api_key` (the default) for a key the user pastes, saved in the
  vault under the provider's name; `url` for a server whose key is
  optional; `none` for one that needs nothing, allowed only on
  `http://127.0.0.1`.

A plugin's providers are offered only while it is installed, switched on and
allowed: since they are sent your messages, they wait for `/plugins allow`
(or Allow on its page) as MCP servers do, and an update that changes them
asks again. Then they appear in Settings › Accounts and `/connect`, list
their models and are routed like any built-in provider. Nothing is run:
there is no code in a plugin's provider, only where to send requests.

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
