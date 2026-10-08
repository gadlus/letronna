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

Manage gateways in Settings › Gateways, or with `/gateways`.

## Hosted models

When you are signed in, the Letronna gateway serves chat models from several providers with no key. Each request is paid from your credit at the prices on the [models page](https://letronna.ai/dashboard/models).

- **Credit**: bought from $1, and it never expires. Once it runs out, hosted requests are refused, but your own keys keep working.
- **Pro** ($20/month): $20 of credit each month, lower prices, higher rate limits, and Letronna Cloud.
- **Team** ($18 per seat per month, 2–5 seats): a shared credit pool, per-member spending limits and an audit log. Each machine picks who pays with `/team`.

Spending is shown per day and per model on the [usage page](https://letronna.ai/dashboard/usage).
