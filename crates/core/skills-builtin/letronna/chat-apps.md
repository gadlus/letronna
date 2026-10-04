# Chat apps

Letronna can be messaged from Telegram, Discord or Slack and answers in the same place. They are set up on the window's Messaging page (in the sidebar, under More); the terminal has no command for it. WhatsApp and Email are listed there as coming, not built.

Only one Letronna hears them at a time: the window, while it is open with an app set up, or `letronna serve`, which runs them and the schedules with no window and stops the runs at Ctrl-C. A second one is told another Letronna already answers the chat apps. Each chat talks in a session of its own, carried on from one message to the next.

## Telegram

1. Message @BotFather on Telegram, send /newbot, and copy the token it gives.
2. Paste it under Bot token on the Telegram page and press Save changes, which checks it with Telegram ("Connected as @name") and starts listening. Listen for messages switches it off and on.
3. Press Pair a chat. A six-digit code shows, good for 10 minutes. Send it to the bot from the chat it should answer; in a group, mention the bot first. Five wrong codes void it, and New code makes another.

Only the paired chat is answered, and everything else is ignored. In a group everyone reads the replies and may ask, but only the member who paired can say yes to a tool or answer what Letronna asks. Telegram is polled, so nothing has to be reachable from the internet. `letronna serve` cannot pair: a chat that is not paired is pointed to the window's Messaging page.

## Discord

1. Create an application in the Discord Developer Portal (https://discord.com/developers/applications), add a bot, and copy its token.
2. Invite the bot to a server you are in: Discord lets a bot DM only people it shares a server with.
3. For your user ID, turn on Developer Mode in Discord, then right-click yourself and Copy User ID.
4. On the Discord page fill in Bot token, Your user ID (the only person the bot answers) and, if wanted, Servers: server IDs where mentioning the bot or replying to it reaches Letronna (right-click a server, Copy Server ID). Empty means DMs only.

There is no code: the user ID is the pairing. A Webhook URL is optional and only posts into a channel. Send a test posts "Letronna is connected." through both.

## Slack

Create an app from scratch at https://api.slack.com/apps, then:

- Socket Mode: turn it on and make an app-level token (xapp-) with connections:write.
- Event Subscriptions: subscribe the bot to message.im, app_mention, message.channels and message.groups.
- OAuth & Permissions: give the bot chat:write, im:history, app_mentions:read, channels:history and groups:history, then install the app and copy its bot token (xoxb-).
- App Home: let people send messages from the Messages tab.

Paste both tokens on the Slack page, switch on Listen for messages, press Pair and send the code to the app as a direct message. From then on it hears that member alone: in the DM, and where they mention it in a channel it was invited to, answering in a thread whose replies reach it without a mention. Slack's connection is opened from here, so nothing has to be reachable from the internet.

## In a chat

- /new starts a new session (Telegram also sends /start when a chat first opens the bot) and /stop stops the reply being written. On Slack type !new and !stop.
- A message sent while a reply is being written goes to that run, which reads it before it answers.
- Let messages use tools is off unless switched on, per app on its page. Off, a message can read and search but not write or run anything, because nobody on the other end can answer an approval card.
- A voice note is transcribed only once that chat has said yes to it, since transcribing costs credit.
- Scheduled jobs report how they ended to Telegram and Slack once paired (Slack in the DM), and on Discord to the bot's DM and the webhook.

The settings are in `messaging.json` in the data folder; on macOS and Windows the tokens are moved to the Keychain or Credential Manager (see `settings-and-data.md`).
