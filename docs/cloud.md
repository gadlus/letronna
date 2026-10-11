# Letronna Cloud

Letronna Cloud runs the same agent on Letronna's servers. A task keeps going after you close your laptop, and what it builds can stay up. It comes with Pro and is paid from your credit.

## Tasks

Ask in the chat ("run this in the cloud"), or from the terminal:

```sh
letronna cloud "task" [--repo <https-url>] [--tier light|normal|heavy] [--thread <id>]
```

| Tier | Memory | Time limit |
|---|---|---|
| light | 512 MB | 15 min |
| normal | 1 GB | 30 min |
| heavy | 2 GB | 1 h |

- **Isolation**: each task runs in its own container. The cloud sees nothing on your computer, so a task carries its inputs or clones an https repository.
- **Threads**: a thread keeps its folder and history between messages.
- **Previews**: a page a task makes gets a private preview link.

## Services

Say "keep it running" and the result stays up as a worker. That fits a scheduler, a webhook receiver, a watcher or a chat bot.

- **Restarts**: a worker is restarted if it stops, and stopped for good after five crashes in ten minutes.
- **Size**: 256 MB by default, anywhere from 128 to 512 MB. An account can keep three running.
- **Billing**: by the hour. A worker stops when your credit runs out. A 256 MB service running all month costs about $1.50.
- **Secrets**: put them in `/work/.env`, not in the code.
- **Commands**: `letronna cloud apps`, `logs`, `down` and `up` manage workers. You can also ask in the chat.

## Web apps

A web app gets its own address, `https://lc-<id>.letronna.si`.

- It wakes on a visit and sleeps after ten minutes without one.
- It is awake at most eight hours a day.
- You pay only while it is awake.

## Pricing

Compute costs about $6 per GB-month, charged by the second. Model calls are billed like any hosted request. Every charge appears under Cloud on the [usage page](https://letronna.ai/dashboard/usage).

## Limits and data

- **Not allowed**: mining, flooding and open ports. Only web traffic reaches the cloud. An abuse guard watches every container.
- **Data**: a thread's data is deleted after 14 days unused, unless a worker in it is still running.
- **Support access**: support can read a run only if you allow it with `letronna cloud support <run> on`, and every look is logged.
