//! A session's name, given by a model once its first reply is in.
//!
//! A session starts out called by the first line of what was asked
//! (`store::name_from`). Once its first reply is in, the front end asks
//! `name` for something better: a model reads the first message and the
//! start of the reply, and offers three short names. A name from the
//! chat's own model is kept as given; one from a quicker model is checked
//! by Jev, where it may be asked, which picks the name that fits best, or
//! finds none fitting and hands on to the next model. Letronna's own Jev
//! checks only a chat on Letronna's gateway, as the chat's words go nowhere
//! else.
//!
//! Which model is a setting, `title_model`. Left empty it is chosen
//! automatically: Letronna's own when the chat is already on Letronna's
//! gateway, asked for as `auto` so its server picks the cheapest it serves
//! by prices only it sees; then the side model, when one is set; then the
//! chat's own. A chat on any other gateway, the user's own key or a
//! company's, is named on that gateway, as its words go nowhere else. No
//! model is named here, as the ones a gateway serves come and go. `chat`
//! asks the chat's own alone, and any other value is a model on the chat's
//! gateway, with the chat's own after it. A model that cannot be reached,
//! or turns the request down, hands on to the next; when none answers the
//! session keeps its first line. Whether a session is named on its own is
//! `auto_title`, which the front ends look at; one the user asks to have
//! named is named whatever it says.
//!
//! What is sent is the first message as typed and the start of the first
//! reply's words, never a tool's output, and, when named again later, the
//! latest message too. Only the name comes back; nothing is kept.

use crate::chat::{Connection, Message};
use serde_json::{Value, json};
use std::time::Duration;

/// Characters of the first message sent: enough to say what it asks.
const FIRST_CHARS: usize = 1_500;

/// Characters of the first reply's words sent: its opening says what it
/// made of the message, and the rest seldom changes what the chat is about.
const REPLY_CHARS: usize = 1_500;

/// Characters of the latest message sent when a session is named again.
const LATEST_CHARS: usize = 500;

/// Characters a name keeps: what the sidebar shows of a title.
const NAME_CHARS: usize = 40;

/// Names asked for, so a checker has more than one to choose between.
const NAMES: usize = 3;

/// How long one model has to answer before the next is asked: a name is
/// not worth a long wait, and the first line stands meanwhile.
const WAIT: Duration = Duration::from_secs(20);

/// Room for the answer: three short names, or a reasoning model's
/// thinking before them.
const ANSWER_TOKENS: u64 = 300;

/// The model Letronna's server is asked for when it is to pick one.
const SERVER_PICKS: &str = "auto";

/// What the model is asked.
const ASK: &str = "Name this chat for a list of chats. Answer with three names, one per \
line, each 2 to 6 words saying what the chat is about, in sentence case and in the language \
the user wrote in, with no quotes, numbering or full stop.";

/// Where a name is asked.
#[derive(Clone, Debug, PartialEq)]
enum Namer {
    /// Letronna's server, which picks the model.
    Letronna,
    /// A model on the chat's gateway: the side model, or the one set for
    /// names.
    Model(String),
    /// The chat's own model.
    Chat,
}

/// A name for the session `messages` hold, from the first model in the
/// order Settings gives that answers; `Err` saying why when nothing has
/// been said yet or no model gave a name.
pub fn name(chat: &Connection, messages: &[Message]) -> Result<String, String> {
    let settings = crate::store::current_settings();
    let Some(said) = said(messages) else {
        return Err("Nothing has been said yet to name the session after.".into());
    };
    let at_letronna = hosted(&chat.endpoint);
    // A chat on this computer is named by its own model, the one loaded.
    let namers = if crate::on_device::at(&chat.endpoint) {
        vec![Namer::Chat]
    } else {
        order(
            &settings.title_model,
            &settings.side_model,
            &chat.model,
            at_letronna,
            crate::auth::signed_in(),
        )
    };
    let mut why = String::from("No model gave a name.");
    for namer in namers {
        // The chat's own model is taken at its word; a quicker one is
        // checked.
        let own = namer == Namer::Chat;
        let connection = match namer {
            Namer::Letronna => on_letronna(chat),
            Namer::Model(model) => Connection {
                model,
                ..chat.clone()
            },
            Namer::Chat => chat.clone(),
        };
        let names = match names(&connection, &said) {
            Ok(names) => names,
            Err(failed) => {
                why = failed;
                continue;
            }
        };
        let judged = (!own)
            .then(|| crate::jev::best_name(chat, &said, &names, at_letronna))
            .flatten();
        match kept(&names, judged) {
            Some(name) => return Ok(name),
            None => why = "None of the names a quick model gave fit the chat.".into(),
        }
    }
    Err(why)
}

/// Where a name is asked, in order, for the `title_model` setting, the
/// `side` model and the chat's own `model`, and whether the chat is on
/// Letronna's gateway (`at_letronna`) while the user is `signed_in`.
fn order(
    title_model: &str,
    side: &str,
    model: &str,
    at_letronna: bool,
    signed_in: bool,
) -> Vec<Namer> {
    // A model named that is the chat's own is the chat's, asked once.
    let other = |name: &str| {
        Some(name.trim())
            .filter(|name| !name.is_empty() && *name != model.trim())
            .map(|name| Namer::Model(name.to_owned()))
    };
    match title_model.trim() {
        "chat" => vec![Namer::Chat],
        // A chat's words go only to its own gateway: one on the user's key
        // or a company's is not sent to Letronna to be named.
        "" => (at_letronna && signed_in)
            .then_some(Namer::Letronna)
            .into_iter()
            .chain(other(side))
            .chain([Namer::Chat])
            .collect(),
        set => other(set).into_iter().chain([Namer::Chat]).collect(),
    }
}

/// The name kept from `names`, with what Jev made of them, `judged`:
/// the one it picked, none when it found none fitting, and the first
/// when it was not asked or said nothing that could be used.
fn kept(names: &[String], judged: Option<Option<usize>>) -> Option<String> {
    match judged {
        Some(Some(n)) => names.get(n).cloned(),
        Some(None) => None,
        None => names.first().cloned(),
    }
}

/// Whether `endpoint`, with or without the chat route, is Letronna's own
/// hosted gateway.
fn hosted(endpoint: &str) -> bool {
    let base = endpoint.trim().trim_end_matches('/');
    base.strip_suffix("/chat/completions").unwrap_or(base) == crate::auth::hosted_base()
}

/// `chat` sent to Letronna's server instead, to pick the model there.
fn on_letronna(chat: &Connection) -> Connection {
    let (endpoint, key) = crate::gateway::resolve(crate::gateway::LETRONNA, "", "");
    Connection {
        endpoint,
        key,
        model: SERVER_PICKS.into(),
        ..chat.clone()
    }
}

/// What a namer reads: the first message as typed, the start of the first
/// reply's words, and the latest message when it is another; `None` when
/// nothing has been said.
fn said(messages: &[Message]) -> Option<String> {
    let mut asked =
        (messages.iter().enumerate()).filter(|(_, m)| crate::chat::opens_turn(m) && !m.notice);
    let (at, first) = asked.next()?;
    let typed = as_typed(&first.content);
    if typed.trim().is_empty() {
        return None;
    }
    let mut said = format!("The first message:\n{}", cut(typed, FIRST_CHARS));
    let reply: Vec<&str> = messages[at + 1..]
        .iter()
        .take_while(|m| m.role != "user")
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.trim())
        .filter(|words| !words.is_empty())
        .collect();
    if !reply.is_empty() {
        said.push_str("\n\nThe start of the reply:\n");
        said.push_str(&cut(&reply.join("\n"), REPLY_CHARS));
    }
    if let Some((_, latest)) = asked.next_back() {
        said.push_str("\n\nThe latest message:\n");
        said.push_str(&cut(as_typed(&latest.content), LATEST_CHARS));
    }
    Some(said)
}

/// A message as it was typed: `/init` as the command, without what an `@`
/// reference attached.
fn as_typed(content: &str) -> &str {
    let content = crate::references::typed(content);
    crate::init::typed(content).unwrap_or(content)
}

/// `text`'s first `chars` characters.
fn cut(text: &str, chars: usize) -> String {
    text.trim().chars().take(chars).collect()
}

/// The names the model `connection` names gives for what was `said`, at
/// least one.
fn names(connection: &Connection, said: &str) -> Result<Vec<String>, String> {
    let endpoint = crate::chat::validate(connection)?;
    // Only while its server is loaded and idle: a name is not worth loading
    // a model for, nor making an answer wait.
    let reached = crate::on_device::reach::reach(connection, endpoint, true)?;
    let endpoint = reached.url.clone();
    let mut body = json!({
        "model": connection.model.trim(),
        "messages": [
            {"role": "system", "content": ASK},
            {"role": "user", "content": said},
        ],
        "stream": false,
        "max_tokens": ANSWER_TOKENS,
    });
    crate::chat::fit_quirks(connection, &mut body);
    let (endpoint, body) =
        crate::chat::to_responses(connection, &endpoint, &body).unwrap_or((endpoint, body));
    let request = crate::chat::client()?.post(endpoint).timeout(WAIT);
    let value: Value = reached
        .sign(crate::chat::carrying(request, connection, &body))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(crate::providers::wire::openai_responses::json)
        .map_err(|e| format!("No name came back: {e}"))?;
    let value =
        crate::providers::wire::openai_responses::reply(crate::chat::as_chat(connection, value));
    let text = crate::chat::unwrap_response(&value)?
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let names = candidates(text);
    if names.is_empty() {
        return Err("The model gave no name.".into());
    }
    Ok(names)
}

/// The names in a model's `reply`, one a line, without the numbering,
/// bullets, quotes, markdown and full stop models add, or a line that
/// only leads into them, cut to `NAME_CHARS` at a word, and no more than
/// `NAMES` of them.
fn candidates(reply: &str) -> Vec<String> {
    reply
        .lines()
        .map(str::trim)
        .filter(|line| !line.ends_with(':'))
        .map(|line| {
            let line = unnumbered(line.trim_start_matches(['-', '*', '•', '#']).trim_start());
            let quoted = |c: char| "\"'`*_“”‘’".contains(c) || c.is_whitespace();
            let line = line
                .trim_matches(quoted)
                .trim_end_matches('.')
                .trim_matches(quoted);
            fit(&line.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .filter(|name| name.chars().any(char::is_alphanumeric))
        .take(NAMES)
        .collect()
}

/// `line` without a list's number in front, as in `1.` or `2)`; a name
/// that starts with a number, as in `2026 budget`, keeps it.
fn unnumbered(line: &str) -> &str {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    match (digits, line[digits..].chars().next()) {
        (1..=2, Some('.' | ')')) => line[digits + 1..].trim_start(),
        _ => line,
    }
}

/// `name` in at most `NAME_CHARS` characters, cut after a whole word
/// where one ends in time.
fn fit(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= NAME_CHARS {
        return name.to_owned();
    }
    let cut: String = chars[..NAME_CHARS].iter().collect();
    // Already cut between two words.
    if chars[NAME_CHARS] == ' ' {
        return cut;
    }
    match cut.rfind(' ') {
        Some(at) if at > NAME_CHARS / 2 => cut[..at].to_owned(),
        _ => cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said_as(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn candidates_lose_numbering_bullets_quotes_and_the_full_stop() {
        let reply = "Here are three names:\n\
                     1. \"Fixing the login redirect.\"\n\
                     2) **Login redirect loop**\n\
                     - `Auth fix`\n\
                     4. A fourth one";
        assert_eq!(
            candidates(reply),
            [
                "Fixing the login redirect",
                "Login redirect loop",
                "Auth fix"
            ]
        );
        // A name that starts with a number keeps it.
        assert_eq!(candidates("2026 budget review"), ["2026 budget review"]);
        assert!(candidates("\n...\n\"\"").is_empty());
    }

    #[test]
    fn a_name_jev_finds_unfitting_is_not_kept_and_one_it_picks_is() {
        let names: Vec<String> = ["One", "Two", "Three"].map(String::from).into();
        assert_eq!(kept(&names, Some(Some(2))), Some("Three".into()));
        assert_eq!(kept(&names, Some(None)), None);
        // Not asked, or no answer that could be read: the first stands.
        assert_eq!(kept(&names, None), Some("One".into()));
    }

    #[test]
    fn a_long_name_is_cut_after_a_word() {
        let long = "Moving the whole billing service over to the new queue";
        let fitted = &candidates(long)[0];
        assert_eq!(fitted, "Moving the whole billing service over to");
        assert!(fitted.chars().count() <= NAME_CHARS);
    }

    #[test]
    fn automatic_asks_letronna_then_the_side_model_then_the_chat() {
        let model = |name: &str| Namer::Model(name.into());
        assert_eq!(
            order("", "small", "big", true, true),
            [Namer::Letronna, model("small"), Namer::Chat]
        );
        assert_eq!(
            order("", "", "big", true, true),
            [Namer::Letronna, Namer::Chat]
        );
        // Signed out, there is no Letronna to ask.
        assert_eq!(
            order("", "small", "big", true, false),
            [model("small"), Namer::Chat]
        );
        // A side model that is the chat's own is the chat's.
        assert_eq!(order("", "big", "big", true, false), [Namer::Chat]);
    }

    #[test]
    fn a_chat_on_this_computer_is_never_named_by_letronna() {
        assert_eq!(order("", "", "llama", false, true), [Namer::Chat]);
    }

    #[test]
    fn a_signed_in_chat_on_another_gateway_is_never_named_by_letronna() {
        // The user's own OpenAI key, or a company's gateway.
        for endpoint in ["https://api.openai.com/v1", "https://llm.example.com/v1"] {
            assert!(!hosted(endpoint));
        }
        assert_eq!(
            order("", "small", "gpt", false, true),
            [Namer::Model("small".into()), Namer::Chat]
        );
        assert_eq!(order("", "", "gpt", false, true), [Namer::Chat]);
        let letronna = crate::auth::hosted_base();
        assert!(hosted(&letronna));
        assert!(hosted(&format!("{letronna}/chat/completions/")));
    }

    #[test]
    fn a_model_set_for_names_comes_before_the_chat_s_own() {
        assert_eq!(
            order("tiny", "small", "big", true, true),
            [Namer::Model("tiny".into()), Namer::Chat]
        );
        assert_eq!(order("chat", "small", "big", true, true), [Namer::Chat]);
        assert_eq!(order(" big ", "", "big", true, true), [Namer::Chat]);
    }

    #[test]
    fn what_is_sent_is_the_first_message_and_the_start_of_its_reply() {
        let mut messages = vec![
            said_as("user", "fix the login redirect"),
            said_as("assistant", ""),
            said_as("tool", "secret output a tool read"),
            said_as(
                "assistant",
                "The redirect loops because the cookie is unset.",
            ),
        ];
        let first = said(&messages).unwrap();
        assert_eq!(
            first,
            "The first message:\nfix the login redirect\n\n\
             The start of the reply:\nThe redirect loops because the cookie is unset."
        );
        // A notice is Letronna's, not something asked.
        messages.push(Message {
            notice: true,
            ..said_as("user", "[Letronna notice]\n- job #2 exited")
        });
        assert_eq!(said(&messages).unwrap(), first);
        // Named again later, the latest message comes too.
        messages.push(said_as("user", "now the logout too"));
        assert!(
            said(&messages)
                .unwrap()
                .ends_with("The latest message:\nnow the logout too")
        );
        assert_eq!(said(&[said_as("assistant", "hello")]), None);
    }

    #[test]
    fn a_chat_on_this_computer_is_named_by_its_own_model_only_while_it_is_idle() {
        use crate::on_device::reach::tests::{SEAMS, SERVING, shelved};
        use std::sync::{Arc, Mutex};
        let _seams = SEAMS.lock().unwrap_or_else(|e| e.into_inner());
        let (_lock, scratch) = shelved("title-here", &["bonsai-2-4b"]);
        // Settings that would send any other chat's words to other models.
        std::fs::write(
            scratch.join("settings.json"),
            r#"{"endpoint": "", "model": "m", "title_model": "tiny", "side_model": "small"}"#,
        )
        .unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let kept = heard.clone();
        let base = crate::mcp_auth::tests::serve(move |request| {
            let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
            kept.lock().unwrap().push((
                body["model"].as_str().unwrap_or_default().to_owned(),
                request
                    .headers
                    .get("authorization")
                    .cloned()
                    .unwrap_or_default(),
            ));
            let reply = json!({"choices": [{"message": {"content": "Fixing the login page"}}]});
            (
                200,
                vec![("Content-Type", "application/json".to_owned())],
                reply.to_string(),
            )
        });
        let chat = Connection {
            endpoint: crate::on_device::BASE.into(),
            model: "bonsai-2-4b".into(),
            ..Default::default()
        };
        let messages = [
            Message {
                role: "user".into(),
                content: "Fix the login page.".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                content: "Done.".into(),
                ..Default::default()
            },
        ];
        *SERVING.lock().unwrap() = None;
        assert!(name(&chat, &messages).is_err());
        assert!(heard.lock().unwrap().is_empty());
        *SERVING.lock().unwrap() = Some((format!("{base}/v1"), "k-live".into(), 16_384));
        assert_eq!(
            name(&chat, &messages).as_deref(),
            Ok("Fixing the login page")
        );
        assert_eq!(
            *heard.lock().unwrap(),
            [("bonsai-2-4b".to_owned(), "Bearer k-live".to_owned())]
        );
        *SERVING.lock().unwrap() = None;
    }
}
