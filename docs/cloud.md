# Letronna Cloud

Letronna Cloud runs the same agent on Letronna's servers. A task keeps going after you close your laptop, and what it builds can stay up.

- **Who can use it.** It comes with Pro and is paid from your credit.
- **On the Free plan.** Tasks run from your own credit too, on the light tier, one at a time.

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
- **GitHub repositories**: connect the Letronna GitHub App to the repositories you choose (from the phone app). A task on one of them can be private.
  - **Its branch.** The task works on a branch of its own, `letronna/<thread>`. What it commits is pushed there and opened as a pull request, and nothing is ever pushed to another branch.
  - **No token in the task.** The GitHub token stays out of the task's container. Each git step gets one for that repository alone, and it is revoked as soon as the step ends.
  - **Protect your main branch.** Branch protection on your main branch is still the real guard.
  - **Letronna as co-author.** Each commit ends with `Co-authored-by: Letronna`, and the pull request with Letronna's mark. Turn this off under Settings › Privacy › Commits and pull requests (In Letronna Cloud), or in the phone app's GitHub settings: it is your account's choice.
  - **Other repositories.** An https repository that is not connected must be public, and nothing is pushed to it.
- **Previews**: a page a task makes gets a private preview link.

## Services

Say "keep it running" and the result stays up as a worker. That fits a scheduler, a webhook receiver, a watcher or a chat bot.

- **Restarts**: a worker is restarted if it stops, and stopped for good after five crashes in ten minutes.
- **Size**: 256 MB by default, anywhere from 128 to 512 MB. An account can keep three running.
- **Billing**: by the hour. A worker stops when your credit runs out. A 256 MB service running all month costs about $1.50.
- **Secrets**: put them in `/work/.env`, not in the code.
- **Commands**: `letronna cloud apps`, `logs`, `down` and `up` manage workers. You can also ask in the chat.

## A Discord bot, day and night

Your own Discord bot can be answered from the cloud, in one server, whether your computer is on or not. Tag the bot, or reply to it, and it does the task and answers in the channel.

```sh
letronna cloud relay discord on --server <server-id> [--role <role-id>]... [--admins]
letronna cloud relay discord          # how it is
letronna cloud relay discord off
```

- **The token**: the one you gave Letronna under Messaging. It is never typed into a chat.
- **Who it hears**: members with a role you name (`--role`, once for each), the server's owner and its Administrators (`--admins`), or both. Everyone else is passed over, and so are DMs.
- **Apart from your account**: whoever tags the bot is not you, so its tasks run in a thread of their own with none of your account's variables, connectors or skills, and nothing can be left running from it. Set what the bot needs for that thread alone. Anyone the bot hears can have it use what you set there.
- **What it costs**: its tasks are paid from your credit, on the light tier unless you say `--tier normal`. It takes three at a time (one a member) and 200 a day.
- **This computer**: while the cloud answers the bot, Letronna here leaves it alone, or both would answer. `off` gives it back.
- **In Discord**: add the bot to the server with View Channels, Send Messages and Read Message History; the command prints the link when it is not there yet. Message Content can stay off: the bot then reads less of the message a reply answers.

## Web apps

A web app gets its own address, `https://lc-<id>.letronna.si`.

- It wakes on a visit and sleeps after ten minutes without one.
- It is awake at most eight hours a day.
- You pay only while it is awake.

## Containers

A task can also run any public image, from Docker Hub, ghcr.io or quay.io. Ask for one ("host a Minecraft server for my friends", "run Postgres for this app") and the task sets it up and tests it.

- **Ports.** A web port gets the app's address. A game or database port (tcp or udp) gets a public address such as `play.letronna.si:20001`, which the task's answer and the Cloud page show.
- **Sleeping.** A container sleeps when nobody is connected and wakes on the first connection. A sleeping Minecraft server shows "Asleep. Join to wake it." in the server list, and the first player to join is asked to join again while it starts, in about 30 seconds. To keep one up all the time, ask for it to stay online. It's then billed by the hour like a service.
- **Size.** Memory as for web apps and services, 0.25 to 2 CPUs, and up to 10 GB of disk for its files. A container that outgrows its disk is stopped, with a message saying so.
- **Your files stay.** The thread's folder is the container's data folder (`/data`), so a world or a database is kept between sleeps. Its `.env` holds the container's settings.
- **Isolation.** Each container runs in a sandbox with a kernel of its own (gVisor), never as root, and can't reach other containers or Letronna's servers.
- **Known setups.** Minecraft Java (Paper or Fabric; you accept Minecraft's EULA first), open.mp and SA-MP servers, Postgres and Discord bots.

## Pricing

Compute costs about $6 per GB-month, charged by the second. A container's files are free up to 1 GB; past that, storage is $0.10 per GB-month, charged by the hour. Model calls are billed like any hosted request. Every charge appears under Cloud on the [usage page](https://letronna.ai/dashboard/usage).

## Limits and data

- **Not allowed**: mining, flooding, sending mail, and attacking or scanning others. Only web traffic and a container's own public ports reach the cloud, with limits on connections per address. An abuse guard watches every container.
- **Data**: a thread's data is deleted after 14 days unused, unless a worker in it is still running.
- **Support access**: support can read a run only if you allow it with `letronna cloud support <run> on`, and every look is logged.
