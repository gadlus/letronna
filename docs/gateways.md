# Gateways and models

A gateway is a model provider. You can set up as many as you like. The model menu lists all of their models, and each request goes to the gateway of the selected model.

| Gateway | Base URL | Key |
|---|---|---|
| Letronna | `https://api.letronna.ai/v1` | Sign in |
| OpenAI | `https://api.openai.com/v1` | Required |
| Anthropic | `https://api.anthropic.com/v1` | Required |
| OpenRouter | `https://openrouter.ai/api/v1` | Required |
| Ollama | `http://localhost:11434/v1` | None |
| Custom | Any OpenAI-compatible server | Optional |

- **Model lists** come from `GET {base}/models`.
- **Streaming** uses SSE chat completions.
- **Context limits**: Letronna knows each model's context window and output limit. When a conversation outgrows the window, older turns are summarised instead of dropped.

Manage them in Settings › Accounts, or with `/connect`.

## When one runs out

When a request fails in a way another gateway could answer (out of credit, a model your Free plan does not include, rate limited, overloaded, or unreachable), it goes on elsewhere, and a line says where:

1. The same model on your other gateways that list it, in the order set in Settings › Accounts › When one runs out. Gateways at the session's tier or later come first, then the earlier ones. Your own gateways, Custom, and gateways added by plugins count as API keys.
2. The models in your fallbacks list.
3. Your default model, if it is on a different gateway.

A model that failed is skipped for five minutes, once another one has answered in its place. If they all fail, you see the error from the session's own model.

## Hosted models

When you are signed in, the Letronna gateway serves chat models from several providers with no key. Each request is paid from your credit at the prices on the [models page](https://letronna.ai/dashboard/models).

- **Credit**: bought from $1, and it never expires. Once it runs out, hosted requests are refused. Your own keys keep working, and requests move to them as described above. On the Free plan, a refused request offers **Upgrade subscription**, and in the terminal a line links to the upgrade page. **Add credit** opens the credit page instead.
- **Free models**: some hosted models are free each day, as the models page and your dashboard list them. On a Free plan with no credit, those stay usable while the rest are greyed out: the window's picker marks them **Free today**, and the terminal's `/model` marks them "(free today)". Each comes with a daily allowance; once it is used up, the model is greyed out with the others ("free · used up today" in the terminal) until 00:00 UTC, or until you upgrade or add credit.
- **Pro** ($20/month): $20 of credit each month, lower prices, higher rate limits, and Letronna Cloud.
- **Team** ($18 per seat per month, 2–5 seats): a shared credit pool, per-member spending limits and an audit log. Each machine picks who pays with `/team`.

Spending is shown per day and per model on the [usage page](https://letronna.ai/dashboard/usage).
