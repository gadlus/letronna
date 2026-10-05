//! A message posted now, at the user's word, through the chat apps a
//! scheduled run reports to.
//!
//! Letronna already holds those apps' connections, so "send this to my
//! Discord" needs no webhook address, token or curl from anywhere else.
//! Each message goes only with a yes on a card that shows its words and
//! where they go, in every mode, so nothing a page or a file wrote reaches
//! the user's accounts unread; and nothing here says which webhook, token,
//! address or ID a place uses.

use super::{Apis, Config, Place};
use crate::chat::Connection;
use crate::redact::Shapes;
use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::sync::Arc;

pub const NOTIFY: &str = "notify";

/// The longest message, in the UTF-16 units Discord counts: under its
/// 2,000, the least any of the apps takes, by more than the room a part
/// keeps for a code block's fences, so each takes it whole, as one message.
const MAX_UNITS: usize = 1_900;

/// What a call is told with nobody there to say yes to it: in a run nobody
/// watches, or a scheduled one, whose answer is posted as it ends.
pub(crate) const UNANSWERED: &str = "Refused: a message goes to the chat apps only with the \
     user's yes, and nobody is here to give it. Do not send it another way; list it in your \
     answer as pending, with its words and where it would go.";

/// A message a call asks for: its words and the places they go.
pub(crate) struct Draft {
    text: String,
    places: Vec<Place>,
}

/// The message `args` ask for, with the chat apps as `config` has them, or
/// why it cannot go.
pub(crate) fn drafted(args: &Value, config: &Config) -> Result<Draft, String> {
    let text = args["text"].as_str().map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err("`text` is required: the message as it should arrive.".into());
    }
    let units = text.encode_utf16().count();
    if units > MAX_UNITS {
        return Err(format!(
            "The message is {units} characters long, and one holds at most {MAX_UNITS}: \
             shorten it."
        ));
    }
    let ready = super::places(config);
    if ready.is_empty() {
        return Err(
            "No chat app is set up to post to. The user connects one on the window's \
             Messaging page."
                .into(),
        );
    }
    let Some(to) = args["to"]
        .as_str()
        .map(str::trim)
        .filter(|to| !to.is_empty())
    else {
        return Ok(Draft {
            text: text.to_owned(),
            places: ready,
        });
    };
    match ready
        .iter()
        .find(|place| place.name().eq_ignore_ascii_case(to))
    {
        Some(&place) => Ok(Draft {
            text: text.to_owned(),
            places: vec![place],
        }),
        None => Err(format!(
            "`to` names none of the places set up to post to: {}. Name one of them, or \
             leave `to` out for all.",
            names(&ready)
        )),
    }
}

/// `places` by name, as the system prompt lists them.
fn names(places: &[Place]) -> String {
    let names: Vec<&str> = places.iter().map(|place| place.name()).collect();
    names.join(", ")
}

/// What the card shows of `draft`: where it goes, then its words as they
/// will arrive.
fn card(draft: &Draft) -> String {
    format!("Post to {}:\n\n{}", names(&draft.places), draft.text)
}

/// What posting the message `args` ask for came to, through the apps at
/// `apis` and mail over `tls`: where it went and why not elsewhere, as an
/// error only when it went nowhere.
fn sent(
    apis: Apis,
    tls: Option<&Arc<rustls::ClientConfig>>,
    config: &Config,
    args: &Value,
) -> Result<String, String> {
    let draft = drafted(args, config)?;
    let mut posted = Vec::new();
    let mut failed = Vec::new();
    for &place in &draft.places {
        match super::post_via(apis, tls, config, place, None, &draft.text) {
            Ok(()) => posted.push(place),
            Err(why) => failed.push(format!(
                "Not posted to {}: {}",
                place.name(),
                unnamed(config, &why)
            )),
        }
    }
    let failed = failed.join(" ");
    match (posted.is_empty(), failed.is_empty()) {
        (true, _) => Err(failed),
        (false, true) => Ok(format!("Posted to {}.", names(&posted))),
        (false, false) => Ok(format!("Posted to {}. {failed}", names(&posted))),
    }
}

/// `error` with the mail server and the addresses `config` mails from and
/// to put in words.
// Only a mail server's reply can quote them back: the other apps' errors
// are Letronna's own words, and the tokens are hidden from every result
// already.
fn unnamed(config: &Config, error: &str) -> String {
    let email = &config.email;
    let server = [email.smtp_host.trim().to_owned()];
    let error = crate::redact::hide(error, &server, Shapes::None, "the mail server");
    let addresses = [&email.username, &email.from, &email.to].map(|address| address.trim());
    let domains = addresses
        .iter()
        .filter_map(|address| address.split_once('@'))
        .map(|(_, domain)| domain);
    let held: Vec<String> = (addresses.iter().copied())
        .chain(domains)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    let error = crate::redact::hide(&error, &held, Shapes::None, "[hidden]");
    // The server's name may have opened the sentence.
    let mut letters = error.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

/// Posts a message the user said yes to through the chat apps set up.
pub struct Notify;

impl Tool for Notify {
    fn name(&self) -> &'static str {
        NOTIFY
    }
    fn description(&self) -> &'static str {
        "Post a message now through the chat apps the user connected to Letronna, where scheduled runs report: to all of them, or to the one in `to`. Letronna sends it with the connections and keys it holds, so use this rather than a webhook, curl or a script of your own. Each message waits for the user's yes."
    }
    fn parameters(&self) -> Value {
        let places: Vec<&str> = Place::ALL.iter().map(|place| place.name()).collect();
        json!({"type": "object", "properties": {
            "text": {"type": "string", "description": "The message as it should arrive, at most 1,900 characters."},
            "to": {"type": "string", "enum": places, "description": "One of the places set up; leave out for all of them."}
        }, "required": ["text"]})
    }
    // Words that leave under the user's name, which nobody can take back:
    // a yes to one message is none to the next.
    fn needs_approval(&self) -> bool {
        true
    }
    fn asked_each_time(&self) -> bool {
        true
    }
    // It changes something past this computer, which plan mode is not for.
    fn mutates(&self) -> bool {
        true
    }
    fn offered(&self, connection: &Connection) -> bool {
        // A scheduled run's answer is posted as it ends, so it is not given
        // a second way to say it; and with no app set up, there is nowhere
        // to post.
        crate::schedule::running(connection.conversation).is_none()
            && !super::places(&super::load()).is_empty()
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        // The gate refuses a call that cannot go before any card is put up.
        drafted(args, &super::load()).map_or_else(|why| why, |draft| card(&draft))
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let telegram = super::api();
        let apis = Apis {
            telegram: &telegram,
            discord: super::DISCORD_API,
            slack: super::SLACK_API,
        };
        sent(apis, super::smtp::tls(), &super::load(), args)
    }
}

#[cfg(test)]
mod tests {
    use super::super::smtp::tests::{mailbox, server, text, trusting};
    use super::super::tests::replies;
    use super::super::{Apis, Config, DISCORD_PART, Place, format, post_via};
    use super::{MAX_UNITS, Notify, card, drafted, sent, unnamed};
    use crate::tools::Tool;
    use serde_json::json;

    /// An address that refuses connections: whatever is sent there fails.
    const NOWHERE: &str = "http://127.0.0.1:1";

    /// Every app set up as far as it takes but the webhook, whose https
    /// address no fake has, with a recognizable value in each field nobody
    /// may read back and the mail going to a fake server on `port`.
    fn set_up(port: u16) -> Config {
        let mut config = Config::default();
        config.discord.enabled = true;
        config.discord.bot_token = "discord-bot-token".into();
        config.discord.owner = "888123456789".into();
        config.slack.enabled = true;
        config.slack.app_token = "xapp-slack-app-token".into();
        config.slack.bot_token = "xoxb-slack-bot-token".into();
        config.slack.owner = "U0SLACKOWNER".into();
        config.telegram.enabled = true;
        config.telegram.token = "7123456789:AAHtelegramtoken".into();
        config.telegram.chat = Some(777123456);
        config.email = mailbox(port);
        config
    }

    /// What nobody may read in a result or a card: each token, address and
    /// ID in `set_up`, and the mail server's name.
    const HELD: [&str; 11] = [
        "discord-bot-token",
        "888123456789",
        "xapp-slack-app-token",
        "xoxb-slack-bot-token",
        "U0SLACKOWNER",
        "AAHtelegramtoken",
        "777123456",
        "127.0.0.1",
        "user_a@example.com",
        "reports@example.com",
        "example.com",
    ];

    #[test]
    fn a_message_goes_to_every_place_set_up_through_letronnas_own_connections() {
        let (discord, to_discord) = replies(&[
            ("200 OK", r#"{"id": "dm1"}"#),
            ("200 OK", r#"{"id": "m9"}"#),
        ]);
        let (slack, to_slack) = replies(&[("200 OK", r#"{"ok": true}"#)]);
        let (telegram, to_telegram) =
            replies(&[("200 OK", r#"{"ok": true, "result": {"message_id": 7}}"#)]);
        let (port, to_mail) = server(false, true, "235 ok");
        let apis = Apis {
            telegram: &telegram,
            discord: &discord,
            slack: &slack,
        };
        let words = "Good morning.\nKeep going.";
        let said = sent(
            apis,
            Some(&trusting()),
            &set_up(port),
            &json!({"text": words}),
        );
        assert_eq!(
            said.as_deref(),
            Ok("Posted to Discord (bot DM), Slack, Telegram, email.")
        );

        // The bot opens its DM with the owner, then posts the words as
        // they are, with no title over them.
        let discord = to_discord.join().unwrap();
        assert_eq!(discord[0].0, "/users/@me/channels");
        assert_eq!(discord[0].1, json!({"recipient_id": "888123456789"}));
        assert_eq!(discord[1].0, "/channels/dm1/messages");
        assert_eq!(discord[1].1["content"], words);
        let slack = to_slack.join().unwrap();
        assert_eq!(slack[0].0, "/chat.postMessage");
        assert_eq!(slack[0].1["channel"], "U0SLACKOWNER");
        assert_eq!(slack[0].1["text"], words);
        let telegram = to_telegram.join().unwrap();
        assert_eq!(telegram[0].0, "/bot7123456789:AAHtelegramtoken/sendMessage");
        assert_eq!(telegram[0].1["chat_id"], 777123456);
        assert_eq!(telegram[0].1["text"], words);
        let mail = to_mail.join().unwrap();
        assert!(mail.mail.iter().any(|line| line == "Subject: Letronna"));
        assert_eq!(text(&mail.mail), "Good morning.\r\nKeep going.");
    }

    #[test]
    fn the_discord_webhook_gets_the_words_as_written() {
        let (base, heard) = replies(&[("204 No Content", "")]);
        let mut config = Config::default();
        config.discord.enabled = true;
        config.discord.webhook = format!("{base}/api/webhooks/1/t");
        let nowhere = Apis {
            telegram: NOWHERE,
            discord: NOWHERE,
            slack: NOWHERE,
        };
        post_via(
            nowhere,
            None,
            &config,
            Place::Webhook,
            None,
            "Good morning.",
        )
        .unwrap();
        let (path, body) = heard.join().unwrap().remove(0);
        assert_eq!(path, "/api/webhooks/1/t");
        assert_eq!(body, json!({"content": "Good morning."}));
        // At its https address, as one is set up, it is where a message goes.
        config.discord.webhook = "https://discord.com/api/webhooks/1/t".into();
        let draft = drafted(&json!({"text": "hi"}), &config).unwrap();
        assert_eq!(draft.places, [Place::Webhook]);
    }

    #[test]
    fn a_place_named_gets_it_alone_and_one_not_set_up_is_refused() {
        let config = set_up(1);
        let to = |to: &str| drafted(&json!({"text": "hi", "to": to}), &config).map(|d| d.places);
        assert_eq!(to("Telegram"), Ok(vec![Place::Telegram]));
        assert_eq!(to("EMAIL"), Ok(vec![Place::Email]));
        assert_eq!(
            to("Discord (webhook)"),
            Err(
                "`to` names none of the places set up to post to: Discord (bot DM), Slack, \
                 Telegram, email. Name one of them, or leave `to` out for all."
                    .into()
            )
        );
        let all = drafted(&json!({"text": "hi", "to": ""}), &config).unwrap();
        assert_eq!(
            all.places,
            [
                Place::DiscordBot,
                Place::Slack,
                Place::Telegram,
                Place::Email
            ]
        );
        let none = drafted(&json!({"text": "hi"}), &Config::default());
        assert!(none.is_err_and(|why| why.starts_with("No chat app is set up")));
    }

    #[test]
    fn a_message_with_no_words_or_past_one_part_is_refused_and_one_at_the_most_goes_whole() {
        let config = set_up(1);
        let words = |text: &str| drafted(&json!({"text": text}), &config).map(|d| d.text);
        assert!(words("  ").is_err_and(|why| why.starts_with("`text` is required")));
        assert!(drafted(&json!({}), &config).is_err());
        assert_eq!(words("  hi\n").as_deref(), Ok("hi"));
        // Counted as Discord counts, where a face is two.
        let over = "\u{1f600}".repeat(MAX_UNITS / 2 + 1);
        assert_eq!(
            words(&over),
            Err(format!(
                "The message is {} characters long, and one holds at most {MAX_UNITS}: \
                 shorten it.",
                MAX_UNITS + 2
            ))
        );
        // At the most, on one line or on many, each app takes it as one
        // message, its words as they are.
        let line = "x".repeat(MAX_UNITS);
        let lines = vec!["a line of words"; MAX_UNITS / 16].join("\n");
        for most in [line, lines] {
            assert_eq!(words(&most).as_ref(), Ok(&most));
            assert_eq!(
                format::split(&most, DISCORD_PART),
                std::slice::from_ref(&most)
            );
        }
    }

    #[test]
    fn the_card_shows_the_words_whole_and_where_they_go_by_name() {
        let args = json!({"text": "Good morning.\n\nKeep going.", "to": "slack"});
        let draft = drafted(&args, &set_up(1)).unwrap();
        assert_eq!(
            card(&draft),
            "Post to Slack:\n\nGood morning.\n\nKeep going."
        );
    }

    #[test]
    fn no_token_address_or_id_reaches_a_result_or_a_card() {
        let (discord, to_discord) = replies(&[("401 Unauthorized", "")]);
        let (slack, to_slack) = replies(&[("200 OK", r#"{"ok": false, "error": "invalid_auth"}"#)]);
        let (telegram, to_telegram) = replies(&[("401 Unauthorized", "")]);
        // A refusal that quotes the address and the server back.
        let (port, to_mail) = server(
            false,
            true,
            "535 5.7.8 Login for user_a@example.com at 127.0.0.1 refused",
        );
        let mut config = set_up(port);
        // An https address that refuses connections, so it is set up and
        // tried, and nothing leaves this computer.
        config.discord.webhook = "https://127.0.0.1:1/api/webhooks/5551234/hooktoken".into();
        let apis = Apis {
            telegram: &telegram,
            discord: &discord,
            slack: &slack,
        };
        let args = json!({"text": "Good morning."});
        let refused = sent(apis, Some(&trusting()), &config, &args).unwrap_err();
        for heard in [to_discord, to_slack, to_telegram] {
            heard.join().unwrap();
        }
        to_mail.join().unwrap();
        assert_eq!(
            refused,
            "Not posted to Discord (webhook): Cannot reach Discord. \
             Not posted to Discord (bot DM): Discord sent an unreadable reply. \
             Not posted to Slack: Slack did not accept that token. \
             Not posted to Telegram: Telegram rejected the message. \
             Not posted to email: The mail server refused the login (535 5.7.8 Login for \
             [hidden] at the mail server refused). Most providers want an app password rather \
             than the account's own; Outlook and Microsoft 365 take only OAuth, which Letronna \
             cannot do."
        );
        let shown = card(&drafted(&args, &config).unwrap());
        let told = format!("{} {}", Notify.description(), Notify.parameters());
        for held in HELD.iter().chain(&["5551234", "hooktoken", "app-password"]) {
            for text in [&refused, &shown, &told] {
                assert!(!text.contains(held), "{held}: {text}");
            }
        }
    }

    #[test]
    fn a_mail_servers_words_name_no_server_address_or_domain() {
        let config = set_up(1);
        assert_eq!(
            unnamed(
                &config,
                "127.0.0.1 refused the address (550 <reports@example.com> unknown at example.com)."
            ),
            "The mail server refused the address (550 <[hidden]> unknown at [hidden])."
        );
        assert_eq!(
            unnamed(&config, "Discord rejected the message."),
            "Discord rejected the message."
        );
    }

    #[test]
    fn the_tool_is_offered_with_a_place_to_post_and_never_in_a_scheduled_run() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = crate::store::tests::Scratch::new("notify-offered");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let session = 9_700_101;
        let job = crate::schedule::Schedule {
            id: 1,
            name: "Daily".into(),
            prompt: "Say hello.".into(),
            workspace: None,
            trigger: crate::schedule::Trigger::Every { minutes: 60 },
            enabled: true,
            last_run: 0,
            mode: crate::gate::Mode::Default,
            by_model: false,
            quiet: false,
            check: None,
        };
        let session_of = |conversation| crate::chat::Connection {
            conversation,
            ..Default::default()
        };
        // Nothing set up is nowhere to post.
        let bare = Notify.offered(&session_of(1));
        let paired = r#"{"telegram": {"enabled": true, "token": "1:t", "chat": 42}}"#;
        std::fs::write(dir.join("messaging.json"), paired).unwrap();
        let set_up = Notify.offered(&session_of(1));
        let scheduled = {
            let _listed = crate::schedule::Listed::new(session, &job);
            Notify.offered(&session_of(session))
        };
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        assert!(!bare);
        assert!(set_up);
        // A scheduled run's answer is posted as it ends.
        assert!(!scheduled);
    }
}
