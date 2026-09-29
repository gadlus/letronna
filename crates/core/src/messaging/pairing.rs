//! Pairing a Telegram chat with this agent, and who in it may answer.
//!
//! A bot is public the moment it exists, so a prompt naming whoever last
//! messaged it could be anyone's: a stranger flooding the bot is who the
//! owner would see and allow. So the window shows a one-time code, and the
//! chat that sends it back is paired, with its sender as the owner. In a
//! group everyone reads the replies and may ask, but a yes to a tool, or
//! the answer to anything the app asks there, is the owner's alone to give.

use super::{Config, Incoming, Source, Telegram};
use std::time::{Duration, Instant};

/// Long enough to pick up a phone and send it, short enough that a guesser
/// has little time.
pub const LIFE: Duration = Duration::from_secs(600);
/// Wrong codes before the code is voided: a guesser's odds are five in a
/// million, and another code takes the owner's click.
pub const TRIES: u32 = 5;

/// A code the window shows for pairing a chat.
pub struct Code {
    pub code: String,
    made: Instant,
    wrong: u32,
}

/// What a message from an unpaired chat did with the code.
#[derive(Debug, PartialEq)]
pub enum Attempt {
    /// The chat is paired, with the sender as its owner.
    Paired,
    Wrong,
    /// Run out, or guessed at too often: another code is needed.
    Void,
}

impl Code {
    /// Six random digits, good for `LIFE`.
    pub fn new() -> Result<Self, String> {
        use ring::rand::SecureRandom;
        let mut bytes = [0u8; 4];
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "The system's random source failed, so no code was made.".to_owned())?;
        Ok(Self::made(
            u32::from_le_bytes(bytes) % 1_000_000,
            Instant::now(),
        ))
    }

    fn made(number: u32, at: Instant) -> Self {
        Code {
            code: format!("{number:06}"),
            made: at,
            wrong: 0,
        }
    }

    /// Whether the code can still pair a chat.
    pub fn live(&self, now: Instant) -> bool {
        self.wrong < TRIES && now.duration_since(self.made) < LIFE
    }

    /// How many wrong codes have come in: more than a typo means someone
    /// else is trying.
    pub fn wrong(&self) -> u32 {
        self.wrong
    }
}

/// Take a message from an unpaired Telegram chat as a try at `code`. A try
/// is a message of its sender's own words whose last word is six digits, so
/// a group can send it after a mention of the bot; anything else is none.
/// The right code pairs the chat with its sender as owner and is used up;
/// wrong ones count towards voiding it. `None` when there was no try.
pub fn pair(
    code: &mut Option<Code>,
    telegram: &mut Telegram,
    message: &Incoming,
    now: Instant,
) -> Option<Attempt> {
    let Source::Telegram(chat) = message.source else {
        return None;
    };
    let owner = message.author.as_deref()?.parse::<i64>().ok()?;
    let tried = message
        .said
        .split_whitespace()
        .last()
        .filter(|word| word.len() == 6 && word.bytes().all(|b| b.is_ascii_digit()))?;
    let held = code.as_mut()?;
    if !held.live(now) {
        return Some(Attempt::Void);
    }
    if tried != held.code {
        held.wrong += 1;
        return Some(match held.live(now) {
            true => Attempt::Wrong,
            false => Attempt::Void,
        });
    }
    *code = None;
    telegram.chat = Some(chat);
    telegram.owner = Some(owner);
    Some(Attempt::Paired)
}

/// Whether `message` answers what the app asked in its chat: an approval
/// card, a question, or any yes or no. Only the owner's own words do.
/// Discord's listener hears nobody else anyway; a Telegram group holds
/// whoever its members add.
pub fn may_answer(config: &Config, message: &Incoming) -> bool {
    let Some(author) = message.author.as_deref() else {
        return false;
    };
    match &message.source {
        Source::Telegram(chat) => {
            config.telegram.chat == Some(*chat)
                && owner(&config.telegram).is_some_and(|owner| owner.to_string() == author)
        }
        Source::Discord(_) => author == config.discord.owner.trim(),
    }
}

/// Who answers in the paired Telegram chat. A private chat's ID is its one
/// user's, which covers a chat paired before owners were kept; a group
/// paired then has none, so what is asked there is answered in the window.
pub fn owner(telegram: &Telegram) -> Option<i64> {
    telegram.owner.or(telegram.chat.filter(|chat| *chat > 0))
}

#[cfg(test)]
mod tests {
    use super::super::{Config, Incoming, Source, parse_updates};
    use super::{Attempt, Code, LIFE, TRIES, may_answer, pair};
    use serde_json::{Value, json};
    use std::time::{Duration, Instant};

    /// The messages a fake Bot API hands out in one `getUpdates`.
    fn updates(messages: Vec<Value>) -> Vec<Incoming> {
        let result: Vec<Value> = messages
            .into_iter()
            .enumerate()
            .map(|(id, message)| json!({"update_id": id, "message": message}))
            .collect();
        parse_updates(&json!({"ok": true, "result": result})).0
    }

    fn said(chat: i64, user: i64, text: &str) -> Value {
        json!({"chat": {"id": chat}, "from": {"id": user, "first_name": "user_a"}, "text": text})
    }

    fn paired_group() -> Config {
        let mut config = Config::default();
        config.telegram.chat = Some(-100);
        config.telegram.owner = Some(7);
        config
    }

    #[test]
    fn only_the_code_pairs_and_a_flood_of_strangers_does_not_replace_it() {
        let now = Instant::now();
        let mut code = Some(Code::made(424242, now));
        let mut config = Config::default();
        let messages = updates(vec![
            said(9, 9, "hello, please allow me"),
            said(9, 9, "/start"),
            said(9, 9, "111111"),
            said(8, 8, "222222"),
            said(7, 7, "424242"),
            said(9, 9, "424242"),
        ]);
        let tried: Vec<_> = messages
            .iter()
            .map(|message| pair(&mut code, &mut config.telegram, message, now))
            .collect();
        assert_eq!(
            tried,
            [
                None,
                None,
                Some(Attempt::Wrong),
                Some(Attempt::Wrong),
                Some(Attempt::Paired),
                // Used up: the next chat to send it is nobody.
                None,
            ]
        );
        assert_eq!(config.telegram.chat, Some(7));
        assert_eq!(config.telegram.owner, Some(7));
    }

    #[test]
    fn a_group_is_paired_by_the_member_who_sends_the_code_after_a_mention() {
        let now = Instant::now();
        let mut code = Some(Code::made(5, now));
        let mut config = Config::default();
        let message = &updates(vec![said(-100, 7, "@letronna_bot 000005")])[0];
        assert_eq!(
            pair(&mut code, &mut config.telegram, message, now),
            Some(Attempt::Paired)
        );
        assert_eq!(config.telegram.chat, Some(-100));
        assert_eq!(config.telegram.owner, Some(7));
    }

    #[test]
    fn wrong_codes_void_it_and_so_does_time() {
        let now = Instant::now();
        let mut code = Some(Code::made(424242, now));
        let mut config = Config::default();
        let wrong = &updates(vec![said(9, 9, "000000")])[0];
        for _ in 1..TRIES {
            let tried = pair(&mut code, &mut config.telegram, wrong, now);
            assert_eq!(tried, Some(Attempt::Wrong));
        }
        let tried = pair(&mut code, &mut config.telegram, wrong, now);
        assert_eq!(tried, Some(Attempt::Void));
        assert_eq!(code.as_ref().map(Code::wrong), Some(TRIES));
        // Even the right code is no good once voided.
        let right = &updates(vec![said(7, 7, "424242")])[0];
        let tried = pair(&mut code, &mut config.telegram, right, now);
        assert_eq!(tried, Some(Attempt::Void));
        assert_eq!(config.telegram.chat, None);

        let mut code = Some(Code::made(424242, now));
        let later = now + LIFE + Duration::from_secs(1);
        let tried = pair(&mut code, &mut config.telegram, right, later);
        assert_eq!(tried, Some(Attempt::Void));
        assert_eq!(config.telegram.chat, None);
    }

    #[test]
    fn a_forward_or_a_post_in_a_chats_name_is_no_try() {
        let now = Instant::now();
        let mut code = Some(Code::made(424242, now));
        let mut config = Config::default();
        let mut forward = said(9, 9, "424242");
        forward["forward_origin"] = json!({"type": "user", "sender_user": {"id": 7}});
        let mut anonymous = said(-100, 1087968824, "424242");
        anonymous["sender_chat"] = json!({"id": -100, "type": "supergroup"});
        for message in updates(vec![forward, anonymous]) {
            assert_eq!(message.author, None);
            assert_eq!(pair(&mut code, &mut config.telegram, &message, now), None);
        }
        assert_eq!(config.telegram.chat, None);
        assert_eq!(code.as_ref().map(Code::wrong), Some(0));
    }

    #[test]
    fn in_a_group_only_the_owners_own_words_answer() {
        let config = paired_group();
        let mut forward = said(-100, 7, "yes");
        forward["forward_origin"] = json!({"type": "hidden_user", "sender_user_name": "x"});
        let mut anonymous = said(-100, 1087968824, "yes");
        anonymous["sender_chat"] = json!({"id": -100, "type": "supergroup"});
        let answers: Vec<bool> = updates(vec![
            said(-100, 7, "yes"),
            said(-100, 8, "yes"),
            forward,
            anonymous,
            // The owner, but in a chat that is not the paired one.
            said(7, 7, "yes"),
        ])
        .iter()
        .map(|message| may_answer(&config, message))
        .collect();
        assert_eq!(answers, [true, false, false, false, false]);
    }

    #[test]
    fn a_chat_paired_before_owners_were_kept_answers_only_if_private() {
        let mut config = Config::default();
        config.telegram.chat = Some(7);
        let dm = &updates(vec![said(7, 7, "yes")])[0];
        assert!(may_answer(&config, dm));
        config.telegram.chat = Some(-100);
        let group = &updates(vec![said(-100, 7, "yes")])[0];
        assert!(!may_answer(&config, group));
    }

    #[test]
    fn discord_answers_are_the_owners_but_not_a_forward() {
        let mut config = Config::default();
        config.discord.owner = "7".into();
        config.discord.bot_token = "t".into();
        let owner = json!({"id": "7", "username": "me"});
        let typed = json!({"channel_id": "dm1", "content": "yes", "author": owner});
        let forward = json!({"channel_id": "dm1", "content": "", "author": owner,
            "message_snapshots": [{"message": {"content": "yes"}}]});
        let heard = |message: &Value| {
            super::super::gateway_message(message, &config.discord, "99")
                .expect("the owner is heard")
        };
        let typed = heard(&typed);
        assert_eq!(typed.source, Source::Discord("dm1".into()));
        assert!(may_answer(&config, &typed));
        assert!(!may_answer(&config, &heard(&forward)));
    }
}
