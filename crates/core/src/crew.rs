//! Crew: a person opting a message or a whole session into multi-agent
//! orchestration.
//!
//! The `workflow` tool is withheld until a session opts in, since one approval
//! can start dozens of helpers. Crew is how a person does that: `@crew` in a
//! message opts that message in, and the Crew switch or `/crew` keeps a session
//! in until it is turned off. Only words a person typed into the composer or
//! the bar count, since a chat app's message, a schedule's prompt or a notice
//! would otherwise let someone else switch it on, and the word counts only
//! written as itself -- not inside code, quotes, tags, paths, addresses or
//! flags that happen to contain it.
//!
//! What `@crew` offers lasts for its turn and the notice and follow-up turns
//! that carry it on, and ends at the next prompt of any other kind -- a new
//! typed message or a schedule's prompt: a run's notice ends in the call that
//! resumes it, so its turn must still find the tool. Agreeing to resume a run
//! Letronna closed on, and a saved workflow's command, offer it the same way
//! through `grant`. Crew keeps what it offers apart from other opt-ins, so
//! neither takes away the other's.
//!
//! The model hears of Crew in notices put before the prompt, never in the
//! system prompt, so the cached prefix stays the same whether it is on or off:
//! the whole reminder when it comes on, a short one every tenth prompt after,
//! and a line when it goes off. The mode is per session and never saved. It
//! offers the tool and lets more helpers run at once, and nothing else: effort
//! stays what the user picked, no budget grows, and every run still shows its
//! launch card.

use crate::budget::short;
use crate::inbox::{self, Notice, When};
use crate::workflow::BUDGET;
use crate::workflow::tool::{self, LeftOpen};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The word that opts a message in.
const KEYWORD: &str = "@crew";

/// Four times the usual 8 helpers at once, for a session that asked to fan
/// out; workflow permits and each provider's gate still bound what really runs
/// side by side.
const AT_ONCE: usize = 32;

/// Prompts between short reminders: often enough that a long session keeps
/// Crew in mind, rarely enough to cost little.
const EVERY: u64 = 10;

/// What may stand before the word and still leave it a word of its own: an
/// opening bracket, a quote that never closes or markdown emphasis.
const OPENING: [char; 5] = ['(', '[', '"', '*', '_'];

/// What may follow the word and still leave it a word of its own: a closing
/// bracket, a stray quote, emphasis, or the end of a sentence or an ellipsis.
const CLOSING: [char; 12] = [
    ')', ']', '"', '*', '_', '.', ',', ';', ':', '!', '?', '\u{2026}',
];

/// Where the words of a message came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Typed into the window's composer.
    Composer,
    /// Typed at the terminal's bar.
    Bar,
    /// A message from a chat app.
    ChatApp,
    /// A schedule's prompt.
    Schedule,
    /// `letronna -p`.
    Print,
    /// A notice folded into the conversation.
    Notice,
    /// A message typed while a turn ran, folded in at its next step.
    FollowUp,
}

/// Where one session stands with Crew.
#[derive(Default, PartialEq)]
struct State {
    /// The Crew switch.
    on: bool,
    /// Whether the model was last told that Crew is on.
    told: bool,
    /// Prompts since the whole reminder.
    since: u64,
    /// Whether `grant` offered the tool for the turn going on, until a prompt
    /// that is not a notice or a follow-up.
    granted: bool,
}

impl State {
    /// Whether Crew offers the `workflow` tool to the session.
    fn offers(&self) -> bool {
        self.on || self.granted
    }
}

/// Sessions with Crew on, told or granted; the rest are left out.
static SESSIONS: Mutex<BTreeMap<u64, State>> = Mutex::new(BTreeMap::new());

fn sessions() -> MutexGuard<'static, BTreeMap<u64, State>> {
    // Nothing that can panic runs under the lock, so a poisoned one is whole.
    SESSIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What Crew tells the model before a prompt.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Say {
    /// The whole reminder, as Crew comes on.
    On,
    /// The short reminder, every tenth prompt while it stays on.
    Still,
    /// That Crew went off.
    Off,
    /// That this message asked for Crew.
    Asked,
}

/// Whether `text` from `source` asks for Crew: `@crew` in any case, as a word
/// of its own, in a message a person typed into the composer or the bar that
/// is not a slash command.
pub fn requested(text: &str, source: Source) -> bool {
    if !matches!(source, Source::Composer | Source::Bar) || text.trim_start().starts_with('/') {
        return false;
    }
    masked(text).split_whitespace().any(|word| {
        let word = word.trim_start_matches(OPENING).trim_end_matches(CLOSING);
        // A possessive leaves it the word: "@crew's plan".
        let word = (word.strip_suffix(['s', 'S']))
            .and_then(|stem| stem.strip_suffix(['\'', '\u{2019}']))
            .unwrap_or(word);
        word.eq_ignore_ascii_case(KEYWORD)
    })
}

/// Turns Crew mode on or off in `session`, offering the `workflow` tool while
/// it is on; the model hears of it with the next prompt.
pub fn set(session: u64, on: bool) {
    let mut sessions = sessions();
    sessions.entry(session).or_default().on = on;
    forget_if_idle(&mut sessions, session);
}

/// Whether Crew mode is on in `session`.
pub fn on(session: u64) -> bool {
    sessions().get(&session).is_some_and(|state| state.on)
}

/// Takes a message `session` got from `source` as it starts or joins a turn,
/// and gives Crew's notices to put before it: words a person typed go to the
/// runs the session starts, and reminders come due. Any prompt but a notice
/// or a follow-up ends what `grant` offered, and `@crew` in it offers the
/// `workflow` tool again (see `requested`); a notice or a follow-up carries
/// the turn on, so a run's notice can still be answered with its resume.
/// Nothing waiting in the inbox changes that, so it may be called before or
/// after the inbox is taken.
pub fn prompt(session: u64, text: &str, source: Source) -> Vec<Notice> {
    if matches!(source, Source::Composer | Source::Bar | Source::FollowUp) {
        tool::typed(session, text);
    }
    if matches!(source, Source::Notice | Source::FollowUp) {
        return Vec::new();
    }
    let asked = requested(text, source);
    let said = {
        let mut sessions = sessions();
        let state = sessions.entry(session).or_default();
        state.granted = asked;
        let mut said = Vec::new();
        match (state.on, state.told) {
            (true, false) => {
                state.told = true;
                state.since = 0;
                said.push(Say::On);
            }
            // A schedule's prompt is nobody's, so it is not counted.
            (true, true) if source != Source::Schedule => {
                state.since += 1;
                if state.since.is_multiple_of(EVERY) {
                    said.push(Say::Still);
                }
            }
            (false, true) => {
                state.told = false;
                state.since = 0;
                said.push(Say::Off);
            }
            _ => {}
        }
        if asked {
            said.push(Say::Asked);
        }
        forget_if_idle(&mut sessions, session);
        said
    };
    if said.is_empty() {
        return Vec::new();
    }
    // Read only when something is said, since it is a file read.
    let size = crate::store::settings().0.crew;
    said.into_iter()
        .map(|say| Notice {
            from: "Crew".into(),
            when: When::Next,
            text: words(say, &size),
            files: Vec::new(),
        })
        .collect()
}

/// Offers the `workflow` tool to `session` for the turn starting now and the
/// notice and follow-up turns that carry it on, as `@crew` does; a saved
/// workflow's command calls it after `prompt` took the command, which would
/// otherwise end it.
pub fn grant(session: u64) {
    sessions().entry(session).or_default().granted = true;
}

/// Whether Crew offers the `workflow` tool to `session`: its switch is on, or
/// `grant` offered it for the turn going on.
pub(crate) fn offers(session: u64) -> bool {
    sessions().get(&session).is_some_and(State::offers)
}

/// Offers the `workflow` tool to the session of `run`, one `tool::left_open`
/// gave, as its person agreed to resume it, and leaves the session a notice
/// asking for the resume, whose turn finds the tool there (see `grant`).
pub fn resume(run: &LeftOpen) {
    grant(run.session);
    let call = json!({"resume": run.name});
    let text = format!(
        "The user chose to resume workflow {} \"{}\", which stopped at {}/{} when Letronna \
         closed: call workflow {call}.",
        run.name, run.title, run.done, run.seen
    );
    let notice = Notice {
        from: "Crew".into(),
        when: When::Next,
        text,
        files: Vec::new(),
    };
    inbox::push(run.session, notice);
}

/// How many helpers may run at once for `session` when Settings allows
/// `setting`: at least 32 while Crew is on there.
pub(crate) fn helpers_at_once(session: u64, setting: usize) -> usize {
    if on(session) {
        setting.max(AT_ONCE)
    } else {
        setting
    }
}

/// Drops `session`'s state once it holds nothing, so the map stays as small
/// as the sessions using Crew.
fn forget_if_idle(sessions: &mut BTreeMap<u64, State>, session: u64) {
    if sessions.get(&session) == Some(&State::default()) {
        sessions.remove(&session);
    }
}

/// The size guideline Settings' `crew` names, as its word and about how many
/// agents it means; medium for anything else, empty included.
fn size(setting: &str) -> (&'static str, u32) {
    match setting {
        "small" => ("small", 5),
        "large" => ("large", 50),
        _ => ("medium", 10),
    }
}

/// What `say` tells the model, with the size guideline `setting` names.
fn words(say: Say, setting: &str) -> String {
    let (word, agents) = size(setting);
    let guideline = format!("Size guideline: {word} (about {agents} agents).");
    let budget = short(BUDGET);
    match say {
        Say::On => format!(
            "Crew is on: aim for the most thorough, correct result. For substantial tasks, write \
             a workflow that splits the work, verifies findings independently and synthesises \
             the result. {guideline} Stay within the budget of {budget} tokens and say what a \
             workflow will cost before starting one."
        ),
        Say::Still => format!(
            "Crew is still on: for substantial tasks, write a workflow that splits the work and \
             verifies it independently. {guideline} Budget: {budget} tokens."
        ),
        Say::Off => "Crew is off: use the workflow tool only when the user asks.".into(),
        Say::Asked => format!(
            "The user wrote @crew: this message opts into multi-agent orchestration; use the \
             workflow tool for it. {guideline}"
        ),
    }
}

/// `text` with what it quotes blanked out -- code in backticks, quoted
/// strings, tags with what they enclose, and quoted lines -- so a word in
/// them is never taken for the user's own.
fn masked(text: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    // What was looked for to its end and not found: it will not be found
    // from any later place either, so a paste thick with unclosed marks is
    // still read in one pass.
    let mut unclosed = HashSet::new();
    let mut line_start = true;
    let mut at = 0;
    while at < chars.len() {
        let end = if chars[at] == '`' {
            code(&chars, at, &mut unclosed)
        } else if chars[at] == '<' {
            tag(&chars, at, &mut unclosed)
        } else if chars[at] == '>' && line_start {
            Some(line_end(&chars, at))
        } else {
            quote(&chars, at, &mut unclosed)
        };
        let Some(end) = end else {
            line_start = chars[at] == '\n' || (line_start && chars[at].is_whitespace());
            at += 1;
            continue;
        };
        for c in &mut chars[at..end] {
            // Line breaks stay, so a quoted line ends where it did.
            if *c != '\n' {
                *c = ' ';
            }
        }
        line_start = false;
        at = end.max(at + 1);
    }
    chars.into_iter().collect()
}

/// Where the code span or block opening with the backticks at `at` ends; a
/// fence of three or more that never closes runs to the end, and a shorter
/// run that never closes is only backticks.
fn code(chars: &[char], at: usize, unclosed: &mut HashSet<Vec<char>>) -> Option<usize> {
    let run = chars[at..].iter().take_while(|c| **c == '`').count();
    let fence = vec!['`'; run];
    let mut from = at + run;
    while let Some(close) = find(chars, from, &fence, unclosed) {
        let end = close + run;
        // A longer run does not close a shorter one.
        if chars.get(end) != Some(&'`') && chars.get(close.wrapping_sub(1)) != Some(&'`') {
            return Some(end);
        }
        from = end + chars[end..].iter().take_while(|c| **c == '`').count();
    }
    if run >= 3 {
        return Some(chars.len());
    }
    Some(at + run)
}

/// Where the tag at `at` ends, and with it what it encloses up to the first
/// closing tag of its name, or None when `<` opens no tag.
fn tag(chars: &[char], at: usize, unclosed: &mut HashSet<Vec<char>>) -> Option<usize> {
    let closing = chars.get(at + 1) == Some(&'/');
    let name_at = at + 1 + usize::from(closing);
    if !chars.get(name_at).is_some_and(char::is_ascii_alphabetic) {
        return None;
    }
    let name_end = name_at + chars[name_at..].iter().take_while(|c| in_name(c)).count();
    let rest = chars[name_end..]
        .iter()
        .position(|c| matches!(c, '>' | '<' | '\n'))?;
    let end = name_end + rest;
    if chars[end] != '>' {
        return None;
    }
    if closing {
        return Some(end + 1);
    }
    let mut close = vec!['<', '/'];
    close.extend_from_slice(&chars[name_at..name_end]);
    let mut from = end + 1;
    loop {
        let Some(found) = find(chars, from, &close, unclosed) else {
            return Some(end + 1);
        };
        from = found + close.len();
        // `</bx>` does not close `<b>`.
        if chars.get(from).is_some_and(in_name) {
            continue;
        }
        return Some(match chars[from..].iter().position(|c| *c == '>') {
            Some(gt) => from + gt + 1,
            None => chars.len(),
        });
    }
}

/// Whether `c` can be part of a tag's name.
fn in_name(c: &char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')
}

/// Where the quoted string opening at `at` ends, or None when nothing opens a
/// quote there or it never closes. Single quotes open only at the start of a
/// word and close only at its end, so apostrophes are left alone.
fn quote(chars: &[char], at: usize, unclosed: &mut HashSet<Vec<char>>) -> Option<usize> {
    let (single, close) = match chars[at] {
        '"' => (false, '"'),
        '\u{201c}' => (false, '\u{201d}'),
        '\'' => (true, '\''),
        '\u{2018}' => (true, '\u{2019}'),
        _ => return None,
    };
    let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    let before = at.checked_sub(1).and_then(|i| chars.get(i));
    let next = chars.get(at + 1);
    if single && (word(before) || next.is_none_or(|c| c.is_whitespace())) {
        return None;
    }
    let mut from = at + 1;
    loop {
        let found = find(chars, from, &[close], unclosed)?;
        if !single || !word(chars.get(found + 1)) {
            return Some(found + 1);
        }
        from = found + 1;
    }
}

/// Where the line holding `at` ends, before its line break.
fn line_end(chars: &[char], at: usize) -> usize {
    (chars[at..].iter().position(|c| *c == '\n')).map_or(chars.len(), |n| at + n)
}

/// Where `needle` is next found in `chars` from `from` on, remembering in
/// `unclosed` what is not there at all.
fn find(
    chars: &[char],
    from: usize,
    needle: &[char],
    unclosed: &mut HashSet<Vec<char>>,
) -> Option<usize> {
    if unclosed.contains(needle) {
        return None;
    }
    let found = (chars.get(from..).unwrap_or_default())
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|n| from + n);
    if found.is_none() {
        unclosed.insert(needle.to_vec());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Connection;
    use crate::store::Settings;
    use crate::tools::Tool;
    use serde_json::json;

    const SCRIPT: &str = r#"export const meta = { name: "sweep", description: "Looks everywhere.", estimate: { agents: 10 } };
return await agent("Look");"#;

    #[test]
    fn at_crew_in_backticks_quotes_paths_or_urls_does_not_count() {
        let quoted = [
            "look at `@crew` in the parser",
            "``the @crew keyword``",
            "```\nsay @crew here\n```",
            "an unclosed fence\n```\n@crew",
            "he said \"@crew\" once",
            "the word '@crew' is special",
            "the word \u{201c}@crew\u{201d} is special",
            "<note>please use @crew</note>",
            "<system-reminder>\n@crew\n</system-reminder>",
            "<b>bold, then @crew</body></b>",
            "ask <@crew> about it",
            "open src/@crew/mod.rs",
            "install @crew/utils",
            "see https://example.com/@crew",
            "see example.com/@crew",
            "mail team@crew.io",
            "run it with --@crew",
            "run it with -@crew or --mention=@crew",
            "> @crew was in the email\nwhat did it mean?",
            "the @crewmate did it",
            "two @crews at once",
        ];
        for text in quoted {
            assert!(!requested(text, Source::Composer), "{text:?}");
        }
        let meant = [
            "@crew",
            "@CREW find every bug",
            "audit this (@crew)",
            "Audit it all. @Crew.",
            "don't stop, @crew it's big",
            "`code` then @crew",
            "the \"parser\" and @crew",
            "rock 'n' roll @crew",
            "<b>bold</b> @crew",
            "a < b, so @crew",
            "**@crew** please",
            "first line\n  @crew: second",
            "[@crew] audit it",
            "then @crew\u{2026}",
            "@crew's plan, please",
            "@CREW\u{2019}S turn",
            "use \"@crew for this",
        ];
        for text in meant {
            assert!(requested(text, Source::Composer), "{text:?}");
        }
    }

    #[test]
    fn a_slash_command_never_triggers_it() {
        assert!(!requested("/review @crew", Source::Composer));
        assert!(!requested("  /crew on @crew", Source::Bar));
        assert!(requested("check this @crew /review", Source::Bar));
    }

    #[test]
    fn only_typed_text_counts() {
        let text = "find the bugs @crew";
        assert!(requested(text, Source::Composer));
        assert!(requested(text, Source::Bar));
        let others = [
            Source::ChatApp,
            Source::Schedule,
            Source::Print,
            Source::Notice,
            Source::FollowUp,
        ];
        for source in others {
            assert!(!requested(text, source), "{source:?}");
        }
    }

    #[test]
    fn the_full_reminder_comes_first_then_a_short_one_every_tenth_prompt() {
        let session = 4_400_101;
        set(session, true);
        assert!(on(session));
        assert!(offers(session));
        let said: Vec<(usize, String)> = (0..25)
            .flat_map(|n| {
                prompt(session, "go on", Source::Composer)
                    .into_iter()
                    .map(move |notice| (n, notice.text))
            })
            .collect();
        let at: Vec<usize> = said.iter().map(|(n, _)| *n).collect();
        assert_eq!(at, [0, 10, 20]);
        assert!(
            said[0]
                .1
                .starts_with("Crew is on: aim for the most thorough")
        );
        assert!(said[1].1.starts_with("Crew is still on:"));
        assert!(said[2].1.starts_with("Crew is still on:"));
        set(session, false);
        assert!(!offers(session));
        let off = prompt(session, "go on", Source::Composer);
        assert_eq!(off.len(), 1);
        assert_eq!(
            off[0].text,
            "Crew is off: use the workflow tool only when the user asks."
        );
        assert!(prompt(session, "go on", Source::Composer).is_empty());
        // Switched off and on again before the model heard, it hears nothing.
        set(session, true);
        set(session, false);
        assert!(prompt(session, "go on", Source::Composer).is_empty());
        assert!(!sessions().contains_key(&session));
    }

    /// Whether the `workflow` tool is offered to `session`, as the tools
    /// array is built.
    fn offered(session: u64) -> bool {
        let connection = Connection {
            conversation: session,
            ..Default::default()
        };
        tool::Workflow.offered(&connection)
    }

    #[test]
    fn at_crew_offers_the_tool_through_its_notices_until_the_next_prompt() {
        let session = 4_400_102;
        let asked = prompt(session, "look everywhere @crew", Source::Composer);
        assert_eq!(asked.len(), 1);
        assert!(asked[0].text.starts_with(
            "The user wrote @crew: this message opts into multi-agent orchestration; use the \
             workflow tool for it."
        ));
        assert!(offered(session));
        assert!(!on(session));
        // A run's notice and a follow-up carry the turn on, so a failed run is
        // resumed in the tool's presence.
        let ended = "workflow wf-1 failed: workflow {\"resume\": \"wf-1\"} picks it up.";
        assert!(prompt(session, ended, Source::Notice).is_empty());
        assert!(prompt(session, "and the tests too", Source::FollowUp).is_empty());
        assert!(offered(session));
        // The next typed prompt ends it, even with a notice still waiting.
        let notice = Notice {
            from: "workflow wf-1".into(),
            when: When::Next,
            text: ended.into(),
            files: Vec::new(),
        };
        inbox::push(session, notice);
        assert!(prompt(session, "and now?", Source::Composer).is_empty());
        assert!(!offered(session));
        assert_eq!(inbox::take(session, true).len(), 1);
        // So does a schedule's prompt, whose own @crew never counts.
        prompt(session, "again @crew", Source::Bar);
        assert!(offered(session));
        assert!(prompt(session, "nightly @crew", Source::Schedule).is_empty());
        assert!(!offered(session));
        assert!(!sessions().contains_key(&session));
    }

    #[test]
    fn crew_never_takes_away_what_another_opt_in_offered_or_the_reverse() {
        let session = 4_400_105;
        tool::opt_in(session);
        set(session, true);
        set(session, false);
        prompt(session, "go on", Source::Composer);
        assert!(offered(session));
        prompt(session, "look everywhere @crew", Source::Composer);
        tool::opt_out(session);
        assert!(offered(session));
        prompt(session, "go on", Source::Composer);
        assert!(!offered(session));
    }

    #[test]
    fn agreeing_to_resume_a_run_offers_the_tool_and_asks_for_the_resume() {
        let session = 4_400_104;
        let run = LeftOpen {
            name: "wf-3".into(),
            title: "sweep".into(),
            session,
            done: 3,
            seen: 5,
        };
        resume(&run);
        assert!(offered(session));
        let asked = inbox::take(session, true);
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0].text,
            "The user chose to resume workflow wf-3 \"sweep\", which stopped at 3/5 when \
             Letronna closed: call workflow {\"resume\":\"wf-3\"}."
        );
        assert!(prompt(session, &asked[0].text, Source::Notice).is_empty());
        assert!(offered(session));
        assert!(prompt(session, "thanks", Source::Composer).is_empty());
        assert!(!offered(session));
    }

    #[test]
    fn crew_mode_never_changes_effort_or_budgets() {
        let _data = (crate::store::tests::DATA_DIR.lock()).unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("letronna-crew-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = r#"{"endpoint": "", "model": "a-model", "effort": "high", "helper_budget": 200000, "crew": "large"}"#;
        std::fs::write(dir.join("settings.json"), file).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let session = 4_400_103;
        let connection = Connection {
            conversation: session,
            ..Default::default()
        };
        let card = || tool::preview(&json!({ "script": SCRIPT }), &connection).unwrap();
        let off = card();
        set(session, true);
        let reminder = prompt(session, "go on", Source::Composer);
        assert_eq!(card(), off);
        assert_eq!(off.budget, BUDGET);
        assert!(
            reminder[0]
                .text
                .contains("Size guideline: large (about 50 agents).")
        );
        assert!(reminder[0].text.contains("the budget of 3.0M tokens"));
        // Only how many helpers run at once goes up, and never down.
        assert_eq!(helpers_at_once(session, 8), 32);
        assert_eq!(helpers_at_once(session, 64), 64);
        set(session, false);
        assert_eq!(helpers_at_once(session, 8), 8);
        prompt(session, "go on", Source::Composer);
        let settings = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert_eq!(settings, file);
        assert_eq!(crate::store::settings().0.effort, "high");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_settings_file_without_crew_loads_as_medium() {
        let file = r#"{"endpoint": "https://example.com/v1", "model": "a-model", "effort": "high", "helpers_at_once": 4}"#;
        let settings: Settings = serde_json::from_str(file).unwrap();
        assert_eq!(settings.effort, "high");
        assert_eq!(settings.helpers_at_once, 4);
        assert_eq!(size(&settings.crew), ("medium", 10));
        assert!(
            words(Say::On, &settings.crew).contains("Size guideline: medium (about 10 agents).")
        );
        assert_eq!(size("small"), ("small", 5));
        assert_eq!(size("large"), ("large", 50));
        assert_eq!(size("huge"), ("medium", 10));
    }
}
