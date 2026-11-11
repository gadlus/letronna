//! The sentences of a request that state what must hold, read off its words.
//!
//! A run nobody watches that builds what a long request specifies ends once
//! its own idea of the change works, and what it misses is most often one
//! behaviour the request stated in a sentence of its own: an exact error
//! message, values accepted in any order, a second path to the same event.
//! So such a run is sent back once with those sentences quoted, to check its
//! change against each.
//!
//! They are picked by their words, with no model. A sentence is kept when,
//! outside its code, it says must, should, cannot, only, returns, raises,
//! accepts, rejects, fires, supports, defaults to, in any order, at most,
//! each or the like, opens with an imperative such as Add or Ensure, opens
//! with a condition such as When or If and goes on past a comma to what
//! follows, or opens with a code span and says what it does. One about
//! how to work rather than what to build (test files, the working tree, when
//! to stop) is left out, as are a question, a guess, a story told in the
//! first person, the past tense or of how things stand now, and a line that
//! is mostly code. Code blocks, indented code, console sessions, headings
//! and tables are skipped whole; a list item is a sentence of its own, and a
//! line broken inside a paragraph is joined to the next. A sentence ends at
//! a stop outside a code span and not after an abbreviation such as "e.g.",
//! and a name from code opens one as a capital does, backticks or none.
//! The bar is a few such sentences, so a short request or a bug report,
//! which the notice to run a build or test covers, costs nothing.
//!
//! A run whose work is a file the request names rather than source, such as
//! a table a command wrote, is sent back the same way to check that file
//! with a script written for the check alone, since its own check tends to
//! reuse the code that made the file, and a mistake there agrees with
//! itself. Only the words of the request that read as a file's name or path
//! are looked up, never the folder walked, and only a file changed since the
//! run began counts.

use crate::chat::Message;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Requirement sentences a notice quotes: past this many the list is a
/// second copy of the request, so it says how many more there are instead.
pub(crate) const LISTED: usize = 40;

/// Fewest requirement sentences that send a run back: a bug report states
/// its fix in three or fewer, its title among them, which the notice to run
/// a build or test covers, while a specification states many.
pub(crate) const FEWEST: usize = 4;

/// Plain sentences that make a request with at least one ask a
/// specification however few use the words that mark a requirement: a bug
/// report tells what goes wrong in fewer.
const RULES: usize = 8;

/// The cases a review's table has a row for where they apply, as a
/// literal that both the notice asking a run for a review and the brief
/// of a review started for the run are built from.
macro_rules! cases {
    () => {
        "a field, argument or part the requirement names left out, undefined or empty; a plain value as well as the same value wrapped, such as in an object or a parser's state, each also built by hand and passed straight to the method that reads it; each other way the same input can be written, such as its parts in another order or a spelling the project's docs or tests use, including any the parser might reject before the new check runs; an error the requirement names, matched by its exact words, not a generic error; two of something the requirement allows only once; the new form beside the existing form of the same thing in one call, where one applies and the other must not; each place in the code that records the event the requirement is about or changes that state, such as a demotion, a close or a removal, found by searching the code, not only the call the request names; the other side of a condition, such as what \"and is not ended\" or \"unless\" keeps out, with both sides in one input; steps the requirement sets in no order, in each order, such as the later one first; each category the requirement maps to an output, with an input from outside the project's own code, such as a caller's function or a returned error, that uses the requirement's own words for it; and each case the project's own tests check of the existing feature nearest to it, done again for the new one in the same order of calls, with the same fixtures and through the same entry point, such as a query made for the first time only after the change it should see;"
    };
}

/// What a run that can start a helper is told first after the sentences: a
/// helper that has not seen the work reads the request afresh, where the
/// run itself tends to check only the paths it built, with its own code. Its
/// table, a row per numbered requirement and case, is what `unchecked`
/// holds the run to afterwards: a helper asked only for what fails samples
/// cases, and files the ones it is unsure of as asides.
const REVIEW: &str = concat!(
    "First ask a helper that hasn't seen your work: with the agent tool, start a helper of type `review`, not general, and wait for its report. Give it the request word for word, the numbered requirements above, the files you changed or made and each case you chose not to handle, with why. Ask it to change no file, to work out each value it checks with code of its own, not your scripts or helpers, and to end its report with a table of one row per requirement and case, the verdict alone in its cell and what shows it in the cell after, written `R1 | case | PASS` or `R1 | case | FAIL | command that shows it`, with a row for each of these cases that applies: ",
    cases!(),
    " and each case you chose not to handle, which fails when the request's words say what should happen in it, quoted in the row, whatever the reason it was set aside. Ask it to write any doubt, caveat or problem it notices beyond those rows as a row of its own, `R1 | case | UNCLEAR | what it doubts`, never only prose, and the same for a row that can only pass by bending another requirement's words. Judge each row by what the request says should happen, not by what the code now does. Fix every row that fails by changing the work, not the case, and any case it calls unclear or sets aside, unless the request rules that case out."
);

/// The least time a run with a deadline needs left to be asked for the
/// review: the helper takes a few minutes, and the run about as long again
/// to fix what it reports, so with less the review would spend the time the
/// fix needed.
pub(crate) const REVIEW_TIME: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The least a run's token budget needs left to be asked for the review:
/// what a review helper may spend, so the review cannot use up what the fix
/// needs.
pub(crate) const REVIEW_TOKENS: u64 =
    crate::agents::budget_for(crate::agents::BUDGET, crate::agent_defs::REVIEW_ROUNDS);

/// What the run is told after the sentences.
const CHECK: &str = "These are picked by their wording, so read the request once more for any behaviour it states that the list leaves out. For each, run or write a quick test that shows it holds through the entry point a caller uses, end to end, not only by reading internal state, minding exact messages, strings and orderings. A behaviour tied to an event, such as something being demoted, closed or removed, holds on every call that can cause that event, not only the one the sentence names: find each and test it. A sentence that limits a behaviour, with words such as only, unless, otherwise, if they differ, and not, as long as, or while keeping or preserving something, also says where it must not happen: test that side too, with the input closest to the line, such as a value that is there but empty or undefined, values that differ in one place, or new data for something that is to stay as it was. Code of yours that lets a stated behaviour be skipped on some path, such as a flag that turns it off, a default that leaves it out or an early return, needs the request to exempt that path; where it does not, make the behaviour happen there too. A case the request's words decide is not yours to set aside or file as an assumption, even where the code makes it hard, such as a read that cannot tell a missing value from one that is there but empty: find another way to tell them apart, such as checking the length or whether the key is there first, and keep to the request's rule. Where steps are set in no order, try them in each order, such as the later one first. Where the request maps categories to outputs, test each with an input from outside the project's own code, such as a caller's function or a returned error, that uses the request's own words for it. Where a sentence names several cases or inputs, try each one in every form it could be written, including any your parser or code might reject before it reaches your new check. Where the request lists the kinds of thing your change handles, such as several types, formats or sources, each other rule it states holds for every one of them: test each rule on each kind, not only on the first. Where it lets parts nest, test the rules inside a nested part too, including one at a position that is missing or empty. Where a sentence's words could mean more than one shape of input, such as a plain value or the same value wrapped in an object, accept and test every shape they could mean, not only the one your code already handles. Spell each the way the project already writes its parts, as its grammar, tests or docs show them, with the new syntax added where those parts allow it, and try that spelling first: it is the one a user writes, not one you invented. A class, function or file the request names that the project already has is the one to change, unless the request asks for a new one: find it and extend it rather than making a new one of that name beside it. Where your change does something the project already does elsewhere, such as reporting a failure, tracking a change or hiding an option, do it the same way: read how the project's own tests call and check the existing ones, follow that over a looser reading of the request's words, and test yours the same way, with the same calls and set-up, such as one object reused across tests or a reset between them. Where they make a change and then call or query for the first time, do that too rather than calling once beforehand to clear or warm it up, since a first call can take its own path. Fix any that fail; skip one that only gives background or the old behaviour. Once your tests pass, change no behaviour the request does not state on a guess at what is tidier or at what a test you cannot see might check: a late change of behaviour needs the request's words or a failing row behind it. Remove any script or test you added only for this check, unless the task asks for tests. Only your last message is passed on as your answer, so end with your whole answer again, not only what changed.";

/// What a run whose work is files the request names is told after the
/// sentences, in place of `CHECK`.
const CHECK_OUTPUT: &str = "These are picked by their wording, so read the request once more for any rule it states that the list leaves out. Check each against what the files named above hold now, with a short new script that reads only those files and the task's inputs and works out every value it compares itself, reusing none of the scripts or helper functions that made them, since a mistake there would agree with itself. Print only what fails, never a large file whole. Fix any that fail, then delete the check script. Only your last message is passed on as your answer, so end with your whole answer again, not only what changed.";

/// Files the notice for outputs names: the first few are enough to find the
/// rest by.
const NAMED: usize = 5;

/// Words of a request that read as a file's name or path looked up at most,
/// so a request listing a whole tree costs a few dozen looks.
const LOOKED: usize = 64;

/// Words and phrases that make a sentence a requirement, in lower case.
const MARKERS: &[&str] = &[
    "must",
    "should",
    "shall",
    "cannot",
    "can't",
    "can not",
    "may not",
    "need to",
    "needs to",
    "has to",
    "have to",
    "required",
    "requires",
    "only",
    "never",
    "does not",
    "do not",
    "doesn't",
    "don't",
    "return",
    "returns",
    "returned",
    "raise",
    "raises",
    "raised",
    "throw",
    "throws",
    "thrown",
    "reject",
    "rejects",
    "rejected",
    "accept",
    "accepts",
    "accepted",
    "fire",
    "fires",
    "fired",
    "emit",
    "emits",
    "emitted",
    "yield",
    "yields",
    "support",
    "supports",
    "supported",
    "defaults to",
    "default to",
    "in any order",
    "at most",
    "at least",
    "no more than",
    "exactly",
    "instead of",
    "preserve",
    "preserves",
    "preserved",
    "is valid",
    "are valid",
    "is invalid",
    "are invalid",
    "treated as",
    "counts as",
    "each",
    "every",
];

/// First words that make a sentence an instruction to build something.
const IMPERATIVES: &[&str] = &[
    "add",
    "allow",
    "always",
    "apply",
    "avoid",
    "call",
    "convert",
    "create",
    "drop",
    "ensure",
    "exclude",
    "export",
    "expose",
    "fix",
    "handle",
    "implement",
    "include",
    "keep",
    "make",
    "map",
    "remove",
    "rename",
    "respect",
    "sort",
    "store",
    "treat",
    "use",
    "validate",
    "write",
];

/// First words of a condition, which with a comma after it states what
/// follows when it holds.
const CONDITIONS: &[&str] = &["when", "whenever", "if", "unless", "once"];

/// Phrases of the instructions a benchmark or task runner wraps a request
/// in, about how to work rather than what to build.
const PROCESS: &[&str] = &[
    "add test files",
    "modify test files",
    "edit test files",
    "change test files",
    "modify the tests",
    "test files unless",
    "or add test",
    "working tree",
    "this checkout",
    "git checkout of",
    "when you are done",
    "when you're done",
    "when you have finished",
    "smallest correct change",
    "smallest possible change",
    "source code in place",
];

/// Words of a report's story rather than its ask: a guess, the first person,
/// the past tense or how things stand now.
const STORY: &[&str] = &[
    "currently",
    "seem",
    "seems",
    "for some reason",
    "maybe",
    "perhaps",
    "probably",
    "afaik",
    "guess",
    "was",
    "were",
    "previously",
    "i",
    "i'm",
    "i've",
    "i'll",
    "i'd",
    "my",
    "me",
];

/// Words of `STORY` in the past tense, which in a specification say what a
/// rule depends on, as "whether or not a cursor was given" does, rather than
/// telling a story.
const PAST: &[&str] = &["was", "were", "previously"];

/// Words, in lower case and without their stop, after which a stop ends no
/// sentence.
const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "eg", "ie", "etc", "vs", "cf", "approx", "incl", "esp", "resp", "no", "fig",
    "al", "a.k.a", "aka", "w.r.t", "wrt", "viz",
];

/// Words, in lower case and without their stop, that are as often a whole
/// word ending a sentence as a short form, as "max" is: a stop after one
/// ends no sentence before a word in lower case, name or not, as in "The
/// min. chunk_size must be 4 bytes."
const SHORTENED: &[&str] = &[
    "min", "max", "ver", "ex", "excl", "eq", "sec", "ref", "est", "avg", "num", "pos", "neg",
    "temp", "misc",
];

/// What a code span stands as while a sentence's words are weighed: one word
/// that is neither a marker nor code.
const SPAN: &str = "\u{2026}";

/// A run of lines that may hold sentences, and whether it is a list's item,
/// whose first sentence may start in lower case.
struct Block {
    text: String,
    item: bool,
}

/// The requirement sentences of `request`, in its order, each once and as
/// written but for its spacing. Where `FEWEST` or more state one by their
/// wording, or one does and `RULES` or more do with the plain ones counted,
/// the request is a specification, whose plain sentences state one too,
/// such as "Keys are matched without case": then each sentence is kept but
/// a question, a story, how to work or code.
pub(crate) fn sentences(request: &str) -> Vec<String> {
    let (mut stated, mut specified): (Vec<String>, Vec<String>) = Default::default();
    let keep = |found: &mut Vec<String>, sentence: &str| {
        if !found.iter().any(|kept| kept == sentence) {
            found.push(sentence.to_owned());
        }
    };
    for block in blocks(request) {
        for (at, sentence) in split(&block.text).into_iter().enumerate() {
            // One starting in lower case goes on from words cut off by code,
            // as "returns" after a snippet does, unless it opens an item or
            // with a name, as "state_data is injected" does.
            let lower = sentence.chars().next().is_some_and(char::is_lowercase);
            if lower && !(block.item && at == 0) && !namelike(first_word(sentence)) {
                continue;
            }
            let states = states(sentence);
            if states {
                keep(&mut stated, sentence);
            }
            if states || specifies(sentence) {
                keep(&mut specified, sentence);
            }
        }
    }
    // Many plain rules beside one ask make a specification too, though few
    // of them use the words that mark one, as a list of how a new type
    // behaves does.
    let specification = stated.len() >= FEWEST || (!stated.is_empty() && specified.len() >= RULES);
    match specification {
        true => specified,
        false => stated,
    }
}

/// What sends a run back to check its change against `sentences`, the
/// first `LISTED` of them quoted, asked first for a helper's review when it
/// can `review`; or, where its work is `outputs`, files the request names,
/// to check those with a script of its own.
pub(crate) fn notice(sentences: &[String], review: bool, outputs: &[String]) -> String {
    let brand = crate::BRAND;
    let work = match outputs {
        [] => "your change".to_owned(),
        _ => files(outputs),
    };
    let mut text = format!(
        "[{brand} check] Before ending, check {work} against each requirement the request states:"
    );
    for (at, sentence) in sentences.iter().take(LISTED).enumerate() {
        text.push_str(&format!("\n- R{}: {sentence}", at + 1));
    }
    if sentences.len() > LISTED {
        let more = sentences.len() - LISTED;
        text.push_str(&format!(
            "\n- and {more} more in the request; check those too."
        ));
    }
    text.push('\n');
    if review {
        text.push_str(REVIEW);
        text.push(' ');
    }
    text.push_str(if outputs.is_empty() {
        CHECK
    } else {
        CHECK_OUTPUT
    });
    text
}

/// Requirements one review helper is given rows for: past about this many a
/// helper's rounds end before its table does, so a longer list is shared
/// between helpers that run side by side.
const PER_REVIEW: usize = 10;

/// The most review helpers started for one run.
const REVIEWS: usize = 4;

/// The rows of `count` numbered requirements each review helper is given,
/// first and last, shared as evenly as `PER_REVIEW` and `REVIEWS` allow.
pub(crate) fn shares(count: usize) -> Vec<(usize, usize)> {
    let count = count.min(LISTED);
    let helpers = count.div_ceil(PER_REVIEW).clamp(1, REVIEWS);
    let (each, over) = (count / helpers, count % helpers);
    let mut first = 1;
    (0..helpers)
        .map(|at| {
            let last = first + each + usize::from(at < over) - 1;
            let share = (first, last);
            first = last + 1;
            share
        })
        .collect()
}

/// The task of a review helper the run is given, checking rows `share` of
/// `sentences`, the requirements `request` states. It is told what the run
/// would leave out: the request word for word, how to find the change, and
/// each case its table has a row for; and not the run's reasons for what it
/// left out, which a reviewer tends to agree with, though each case it
/// weighed and set aside, `doubts`, is listed for a row of its own, and the
/// words it read more than one way, `readings`, for a row per reading.
pub(crate) fn brief(
    request: &str,
    sentences: &[String],
    share: (usize, usize),
    doubts: &[String],
    readings: &[crate::doubts::Reading],
) -> String {
    let mut text = String::from(
        "Review a change you have not seen, made to the project in your working folder, \
         against the request below. Its author's reasons are not given, so judge it by the \
         request's words alone. See the change with `git status --short` and `git diff`, \
         new files included. Change no file, and work out each value you check with code of \
         your own, not the change's scripts or helpers.\n\nThe request, word for word:\n",
    );
    text.push_str(request.trim());
    text.push_str("\n\nThe requirements it states, numbered:");
    for (at, sentence) in sentences.iter().take(LISTED).enumerate() {
        text.push_str(&format!("\n- R{}: {sentence}", at + 1));
    }
    let (first, last) = share;
    let whole = first == 1 && last >= sentences.len().min(LISTED);
    let rows = match whole {
        true => "each requirement".to_owned(),
        false => format!("R{first} to R{last}; other helpers check the rest"),
    };
    text.push_str(&format!(
        "\n\nCheck {rows}. End your report with a table of one row per requirement and \
         case, the verdict alone in its cell and what shows it in the cell after, written \
         `R{first} | case | PASS` or `R{first} | case | FAIL | command that \
         shows it`, with a row for each of these cases that applies: {} and each case the \
         change leaves out or handles more narrowly than the request's words, which fails \
         when those words say what should happen in it, quoted in the row. Any doubt about a \
         case, any caveat to a row, and anything wrong you notice beyond the rows asked for \
         gets a row of its own, written `R{first} | case | UNCLEAR | what you doubt`, never \
         only prose, since prose may be cut before the change's author reads it. A row that \
         can only pass by bending another requirement's words is UNCLEAR too: quote both, and \
         prescribe no fix.",
        cases!()
    ));
    text.push_str(&crate::doubts::section(doubts));
    // Only the readings of its own rows, as the requirements are shared.
    let own: Vec<_> = (readings.iter())
        .filter(|reading| whole || (first..=last).contains(&reading.number))
        .cloned()
        .collect();
    text.push_str(&crate::doubts::reading_rows(&own));
    text
}

/// What the run is told when helpers that have not seen its work reviewed or
/// tested it for the run: the numbered requirements, as `notice` lists them,
/// the reviewers' `reports` with their tables, or with none the ask for a
/// review as `notice` makes it where the run can still `review`, after what
/// became of the reviews `lost`,
/// the reports that came back with no table that could be read, `unread`,
/// what it is told of the tester's report when there is one, `tested`, and
/// the check to make after.
pub(crate) fn reviewed(
    sentences: &[String],
    reports: &[String],
    unread: &[String],
    tested: Option<&str>,
    lost: Option<(usize, usize)>,
    review: bool,
) -> String {
    // With no review's table the run still asks for a review itself, where
    // it can `review`.
    let mut text = notice(sentences, reports.is_empty() && review, &[]);
    let at = text.find(CHECK).unwrap_or(text.len());
    text.truncate(at);
    if let Some((started, spent)) = lost {
        let at = text.find(REVIEW).unwrap_or(text.len());
        text.insert_str(at, &unreported(started, spent));
    }
    if !reports.is_empty() {
        text.push_str(
            "Helpers that haven't seen your work reviewed it against the request; their \
             reports follow, each ending with a table of its rows. Fix every row that fails by \
             changing the work, not the case, and any case a report calls unclear or sets \
             aside, unless the request rules that case out.",
        );
        for report in reports {
            text.push_str(&format!("\n\n{}", report.trim()));
        }
        text.push_str("\n\n");
    }
    if !unread.is_empty() {
        text.push_str(&untabled(unread.len()));
        for report in unread {
            text.push_str(&format!("\n\n{}", report.trim()));
        }
        text.push_str("\n\n");
    }
    if let Some(tested) = tested {
        text.push_str(tested.trim());
        text.push_str("\n\n");
    }
    text.push_str(CHECK);
    text
}

/// What a run is told before it is asked to start a review itself, when
/// none of the `started` reviews begun for it came back with a table and
/// their budget stopped `spent` of them: a review that ran out checked
/// nothing to the end, which is not a review that found nothing wrong.
fn unreported(started: usize, spent: usize) -> String {
    let which = match (started, spent) {
        (1, _) => "The review started for you ran out of budget before reporting".to_owned(),
        (n, spent) if spent == n => {
            format!("The {n} reviews started for you ran out of budget before reporting")
        }
        (n, spent) => format!(
            "{spent} of the {n} reviews started for you ran out of budget before reporting, \
             and the rest gave no table"
        ),
    };
    format!("{which}, so nothing is reviewed yet: that is not a pass. ")
}

/// What a run is told ahead of the reports of the `count` reviews started
/// for it that came back with no table that could be read: none of their
/// rows holds the run, so what they found is its own to read, where a
/// report dropped without a word passed for a review that found nothing.
fn untabled(count: usize) -> String {
    let which = match count {
        1 => "One review started for you came back with no table that could be read, so none \
              of its rows is counted: its report follows. Read it for what it found, and fix \
              what it says fails"
            .to_owned(),
        n => format!(
            "{n} reviews started for you came back with no table that could be read, so none \
             of their rows is counted: their reports follow. Read them for what they found, \
             and fix what they say fails"
        ),
    };
    format!("{which} by changing the work, not the case, unless the request rules that case out.")
}

/// How many requirements `text` numbers, when it is the notice `notice`
/// writes; None for any other text.
pub(crate) fn listed(text: &str) -> Option<usize> {
    let opening = format!("[{} check] Before ending, check ", crate::BRAND);
    if !text.starts_with(&opening) {
        return None;
    }
    let numbered = |line: &str| {
        let rest = line.strip_prefix("- R")?;
        let (number, _) = rest.split_once(": ")?;
        number.parse::<usize>().ok()
    };
    text.lines().filter_map(numbered).max()
}

/// The line a notice opens a tester's report with when the tester wrote it
/// ahead of the work, in the project as it was, and the line that closes
/// it: the rows between judge nothing of the work, so `rows` skips them.
pub(crate) const AHEAD_OPENS: &str =
    "[The tester's report, written before your change; its rows judge nothing of your work]";
pub(crate) const AHEAD_CLOSES: &str = "[End of the tester's report]";

/// The rows of a review's table in `report`: each requirement's number,
/// whether the case passed, and the row as written. A line is a row when a
/// cell of it opens with `R` and a number and a later cell says PASS or
/// FAIL, alone or ahead of what shows it (`judged`), however the helper
/// laid the table out; one whose last cell says the case waits on the work
/// (`placeholder`) is no row, since it says nothing of the work once the
/// work is there, nor is any line of a report written ahead of the work,
/// between `AHEAD_OPENS` and `AHEAD_CLOSES`.
fn rows(report: &str) -> Vec<(usize, bool, String)> {
    let mut found = Vec::new();
    let mut ahead = false;
    for line in report.lines() {
        match line.trim() {
            AHEAD_OPENS => {
                ahead = true;
                continue;
            }
            AHEAD_CLOSES => {
                ahead = false;
                continue;
            }
            _ if ahead => continue,
            _ => {}
        }
        let cells = cells(line);
        let Some(first) = cells.first() else {
            continue;
        };
        let digits: String = (first.strip_prefix('R').unwrap_or_default().chars())
            .take_while(char::is_ascii_digit)
            .collect();
        let Ok(number) = digits.parse::<usize>() else {
            continue;
        };
        if cells.last().is_some_and(|cell| placeholder(cell)) {
            continue;
        }
        if let Some(passed) = judged(&cells).and_then(|(_, word)| verdict(word)) {
            found.push((number, passed, line.trim().to_owned()));
        }
    }
    found
}

/// Whether a table's `cell` only says that the case waits on the work, as
/// a tester writing its tests ahead of the work marks each of its rows.
fn placeholder(cell: &str) -> bool {
    cell.to_ascii_lowercase().starts_with("not yet implemented")
}

/// The cells of a table's `line`, cut at each `|` and stripped of the marks
/// a table is often written with.
fn cells(line: &str) -> Vec<&str> {
    line.split('|')
        .map(|cell| cell.trim().trim_matches(|c| c == '*' || c == '`').trim())
        .filter(|cell| !cell.is_empty())
        .collect()
}

/// Whether a table's `cell` says a case passed or failed, if it says either;
/// one left `UNCLEAR` holds the run as one that failed does.
fn verdict(cell: &str) -> Option<bool> {
    match cell.to_ascii_uppercase().as_str() {
        "PASS" | "PASSED" | "PASSES" => Some(true),
        "FAIL" | "FAILED" | "FAILS" | UNCLEAR => Some(false),
        _ => None,
    }
}

/// The verdict's word a table's `cell` opens with and what the cell goes on
/// to say, when it holds more than the word, as `PASS — what shows it`,
/// `FAIL: why` and `PASS (3 cases)` do. Only a word in capitals with such a
/// mark after it is one: a case that opens "fails when", "Fail fast" or
/// "FAIL-fast" is prose, and so is one that goes on past a bracket, as
/// "`PASS (3)` is the summary line" does.
fn opens(cell: &str) -> Option<(&str, &str)> {
    let letters = cell.bytes().take_while(u8::is_ascii_uppercase).count();
    let (word, rest) = cell.split_at(letters);
    verdict(word)?;
    // What is left of the marks a word written in bold closes with.
    let rest = rest.trim_start_matches(['*', '`']);
    let said = rest.trim_start();
    let mut after = said.chars();
    let goes_on = match after.next()? {
        '—' | '–' => true,
        // A bracket holds what shows it, so it stays, and ends the cell.
        '(' => return said.ends_with(')').then_some((word, said)),
        // Bold may close after the mark, as in `**FAIL:** why`.
        ':' | '.' => {
            let next = after.as_str().trim_start_matches('*').chars().next();
            next.is_none_or(char::is_whitespace)
        }
        // Set off by a space, as no hyphen inside a word is.
        '-' => said.len() < rest.len(),
        // Before a bar escaped to keep the row in one column of a table,
        // so what shows it is in the next cell.
        '\\' => after.next().is_none(),
        _ => false,
    };
    let said = said.trim_start_matches(['—', '–', ':', '.', '-', '\\']);
    goes_on.then_some((word, said.trim_start_matches('*').trim_start()))
}

/// The verdict's word a table's `cell` ends with, the case before it and
/// any note in brackets after it, when the helper wrote the case and the
/// verdict in one cell, as `case — PASS` and `case — UNCLEAR (what it
/// doubts)` do. Only a word in capitals after a dash, with no more than
/// such a note after it, is one: a case that mentions a verdict is prose,
/// and a cell that opens with one verdict's word and ends with another
/// says both.
fn closes(cell: &str) -> Option<(&str, &str, &str)> {
    let marks = ['*', '`'];
    cell.rmatch_indices(['—', '–', '-']).find_map(|(at, dash)| {
        let (case, said) = (&cell[..at], &cell[at + dash.len()..]);
        // Set off by spaces, as no hyphen inside a word is.
        let apart = dash != "-" || (case.ends_with(' ') && said.starts_with(' '));
        let said = said.trim_start().trim_start_matches(marks);
        let letters = said.bytes().take_while(u8::is_ascii_uppercase).count();
        let (word, note) = said.split_at(letters);
        let note = note.trim_start_matches(marks).trim_start();
        let noted = note.is_empty() || (note.starts_with('(') && note.ends_with(')'));
        let case = case.trim_end();
        let leads = case.bytes().take_while(u8::is_ascii_uppercase).count();
        let cased = !case.is_empty() && verdict(&case[..leads]).is_none();
        (apart && noted && cased && verdict(word).is_some()).then_some((case, word, note))
    })
}

/// What a table's `cell` holds when it gives a verdict and more: the case
/// before the word, the word, and what shows it after (`opens`, `closes`).
/// One that says PASS and goes on to say FAIL or UNCLEAR in capitals says
/// both, so it gives no verdict rather than a wrong one.
fn carried(cell: &str) -> Option<(&str, &str, &str)> {
    let opened = opens(cell).map(|(word, shows)| ("", word, shows));
    let (case, word, shows) = opened.or_else(|| closes(cell))?;
    let denies = |said: &str| verdict(said) == Some(false) && !said.contains(char::is_lowercase);
    let both = verdict(word) == Some(true)
        && (shows.split(|c: char| !c.is_ascii_alphabetic())).any(denies);
    (!both).then_some((case, word, shows))
}

/// Whether a table's `cell` says a test was not run, as a tester marks one
/// it could not run, with or without the reason after it.
fn unrun(cell: &str) -> bool {
    let rest = cell.strip_prefix("NOT RUN");
    rest.is_some_and(|rest| !rest.starts_with(char::is_alphabetic))
}

/// Whether a row's first `cell` names requirements and nothing else, as
/// `R4`, `R4/R5` and `R4 and R5` do, where a sentence that opens with a
/// requirement's number goes on in other words.
fn numbered(cell: &str) -> bool {
    let mut words = (cell.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .peekable();
    let names = |word: &str| {
        let digits = word.strip_prefix('R').unwrap_or_default();
        word == "and" || (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    };
    words.peek().is_some() && words.all(names)
}

/// The cell of a row's `cells` that gives its verdict, and the word it
/// gives. A cell that is the word alone wins, the first of them, as a
/// review is asked to write it; with none, a cell that holds the word and
/// more (`carried`), and of several the first that does not say the case
/// passed, since a failed row read as passed is the worse mistake. The
/// cell after the number is the case wherever more cells follow it, so
/// whatever it quotes it gives a verdict only as the row's last, and a row
/// a cell of which says the test was not run (`unrun`) has none to give,
/// nor has a line whose first cell holds more than requirements' numbers
/// (`numbered`), which is a sentence a bar in its code happened to cut.
fn judged<'a>(cells: &[&'a str]) -> Option<(usize, &'a str)> {
    let later = || cells.iter().copied().enumerate().skip(1);
    if let Some(alone) = later().find(|(_, cell)| verdict(cell).is_some()) {
        return Some(alone);
    }
    let row = cells.first().is_some_and(|cell| numbered(cell));
    if !row || later().any(|(_, cell)| unrun(cell)) {
        return None;
    }
    let said = |(at, cell)| {
        let case = at == 1 && cells.len() > 2;
        (carried(cell).filter(|_| !case)).map(|(_, word, _)| (at, word))
    };
    (later().filter_map(said))
        .find(|(_, word)| verdict(word) == Some(false))
        .or_else(|| later().find_map(said))
}

/// Whether a table's row `line` was left `UNCLEAR` rather than failed, by
/// the cell that gives its verdict (`judged`): a failed row may say unclear
/// of why.
fn left_unclear(line: &str) -> bool {
    judged(&cells(line)).is_some_and(|(_, word)| word.eq_ignore_ascii_case(UNCLEAR))
}

/// Whether `report` holds a review's table, with a row of at least one
/// requirement.
pub(crate) fn tabled(report: &str) -> bool {
    !rows(report).is_empty()
}

/// What a review's row says of a case it doubts, so the doubt reaches the run
/// as a row rather than as prose a cut can lose.
const UNCLEAR: &str = "UNCLEAR";

/// Rows of a failed case a follow-up quotes: enough to act on, and past
/// them the run reads the report it already has.
const FAILED_SHOWN: usize = 10;

/// Rows left unclear a follow-up quotes, counted apart from the failed
/// ones: counted with them, a long table's doubts all fell under "more".
const UNCLEAR_SHOWN: usize = 10;

/// Fewest words in a row a quote in a line that settles a row shares with
/// the request: one or two, such as "must not", are found in most requests.
const SETTLING: usize = 3;

/// The first word of a reading cell that takes no reading, in lower case.
const SHRUGS: &[&str] = &[
    "unclear",
    "ambiguous",
    "unknown",
    "unsure",
    "undecided",
    "leave",
    "skip",
    "none",
    "n/a",
    "?",
    "-",
];

/// The reading cell of a row the request states nothing on, which the run
/// leaves as it is: silence is no ground for a late change of behaviour.
const NOT_STATED: &str = "not stated";

/// The longest row a follow-up quotes, in characters.
const ROW_CHARS: usize = 300;

/// The longest case a row the quote cut is written again with, in characters.
const CASE_CHARS: usize = 120;

/// Rows the quote cut that are written again after it; a bigger table is
/// read in the helper's whole report.
const CUT_SHOWN: usize = 60;

/// The rows of a review's table in `report` that `quote`, the report as cut
/// to fit, lost, to go after it so the run is held to the whole table: each
/// failed row with what shows it, and each passed one by its requirement and
/// case. None when the quote kept every row.
pub(crate) fn cut_rows(report: &str, quote: &str) -> Option<String> {
    let cut = |text: &str, most: usize| -> String { text.chars().take(most).collect() };
    let lost: Vec<String> = (rows(report).into_iter())
        .filter(|(_, _, line)| !quote.contains(line.as_str()))
        .map(|(number, passed, line)| {
            let cells = cells(&line);
            let at = judged(&cells).map_or(cells.len(), |(at, _)| at);
            // What the verdict's own cell says besides the word is the
            // end of the case before it and shows it after.
            let (before, after) = (cells.get(at).copied().and_then(carried))
                .map_or(("", ""), |(case, _, shows)| (case, shows));
            let joined = |cells: &[&str], more: &[&str]| -> String {
                let said: Vec<&str> = (cells.iter().chain(more).copied())
                    .filter(|said| !said.is_empty())
                    .collect();
                said.join(" | ")
            };
            let case = cut(&joined(&cells[1..at], &[before]), CASE_CHARS);
            let rest = &cells[(at + 1).min(cells.len())..];
            let shows = cut(&joined(&[after], rest), ROW_CHARS);
            let word = if left_unclear(&line) { UNCLEAR } else { "FAIL" };
            match (passed, shows.is_empty()) {
                (true, _) => format!("R{number} | {case} | PASS"),
                (false, true) => format!("R{number} | {case} | {word}"),
                (false, false) => format!("R{number} | {case} | {word} | {shows}"),
            }
        })
        .collect();
    if lost.is_empty() {
        return None;
    }
    let mut text = String::from("\nRows of its table the quote above cut, written again:");
    for row in lost.iter().take(CUT_SHOWN) {
        text.push_str(&format!("\n{row}"));
    }
    if lost.len() > CUT_SHOWN {
        text.push_str(&format!("\nand {} more rows", lost.len() - CUT_SHOWN));
    }
    Some(text)
}

/// What sends a run back once more when the review's table in `report`
/// leaves some of the first `count` requirements without a row, or has rows
/// that failed; None when it covers each and every row passed. The failed
/// rows come first, then the ones left unclear, each kind with a count of
/// its own, and the run is asked for a line of a set form for each row it
/// decides rather than fixes, which `unresolved` then looks for.
pub(crate) fn unchecked(report: &str, count: usize) -> Option<String> {
    let rows = rows(report);
    let missing: Vec<String> = (1..=count)
        .filter(|number| !rows.iter().any(|(row, _, _)| row == number))
        .map(|number| format!("R{number}"))
        .collect();
    let (unclear, failed): (Vec<_>, Vec<_>) = (rows.iter())
        .filter(|(number, passed, _)| !passed && *number <= count)
        .partition(|(_, _, row)| left_unclear(row));
    if missing.is_empty() && failed.is_empty() && unclear.is_empty() {
        return None;
    }
    let brand = crate::BRAND;
    let mut text = format!("[{brand} check] The review's table ");
    if !missing.is_empty() {
        text.push_str(&format!("has no row for {}", and_list(&missing)));
    }
    let mut said = !missing.is_empty();
    let kinds = [
        (&failed, FAILED_SHOWN, "that failed"),
        (&unclear, UNCLEAR_SHOWN, "left unclear"),
    ];
    for (held, shown, kind) in kinds {
        if held.is_empty() {
            continue;
        }
        // The first list goes on from "has no row for"; a second from the
        // first, which ends its line.
        text.push_str(match (said, text.ends_with('\n')) {
            (false, _) => "has these rows ",
            (true, false) => ", and has these rows ",
            (true, true) => "and these ",
        });
        text.push_str(&format!("{kind}:"));
        for (_, _, row) in held.iter().take(shown) {
            let row: String = row.chars().take(ROW_CHARS).collect();
            text.push_str(&format!("\n- {row}"));
        }
        if held.len() > shown {
            text.push_str(&format!("\n- and {} more {kind}", held.len() - shown));
        }
        text.push('\n');
        said = true;
    }
    if failed.is_empty() && unclear.is_empty() {
        text.push_str(". ");
    }
    if !missing.is_empty() {
        text.push_str(
            "Test each requirement without a row yourself, through the entry point a caller \
             uses, in every case the table asks for that applies to it. ",
        );
    }
    if !failed.is_empty() || !unclear.is_empty() {
        text.push_str(
            "A failed row stands for its whole case, not only its example: fix the rule behind \
             it, then test one more input of that case written another way, such as one from \
             outside the project's own code that uses the request's words. Run each failed \
             row's command again and see it pass now, or fix what it shows. A fix for one row \
             can break another, so then run every row's command again, those that passed too, \
             and any tests a tester's report says are kept for you, and fix any that now fail. ",
        );
    }
    if !failed.is_empty() || unclear.is_empty() {
        text.push_str("Fix what fails unless the request rules that case out. ");
    }
    if !unclear.is_empty() {
        text.push_str(
            "A row left unclear is a doubt its reviewer could not settle, not a failure: \
             decide it by the request's words and test the case, and change the work for it \
             only where the words you quote ask for that. ",
        );
    }
    if let Some((number, _, _)) = unclear.first().or(failed.first()) {
        let which = match (unclear.is_empty(), failed.is_empty()) {
            (false, true) => "each row left unclear above",
            (false, false) => "each row left unclear above, and each failed row you do not fix",
            (true, _) => "each failed row you do not fix",
        };
        text.push_str(&format!(
            "For {which}, write one line in your answer, `R{number} | reading taken | \"the \
             request's words for it\" | command run`, with the words quoted as the request \
             writes them and a command you ran that tests that reading. A bare \"the request \
             does not say\" settles nothing: where it states nothing on the case, write \
             `R{number} | {NOT_STATED} | \"the nearest words it does have\" | left as is`, \
             and change nothing for that row on a guess. "
        ));
    }
    text.push_str(
        "Only your last message is passed on as your answer, so end with your whole answer \
         again, not only what changed.",
    );
    Some(text)
}

/// The requirements whose rows `text` shows as left unclear, each once,
/// when it is the notice `unchecked` writes; none for any other text. They
/// are told by the list they are in, since a long row is cut before the
/// cell that says so.
pub(crate) fn left_open(text: &str) -> Vec<usize> {
    let opening = format!("[{} check] The review's table ", crate::BRAND);
    if !text.starts_with(&opening) {
        return Vec::new();
    }
    let mut open = Vec::new();
    let mut listing = false;
    for line in text.lines() {
        let Some(row) = line.strip_prefix("- ").filter(|_| listing) else {
            listing = line.ends_with("left unclear:");
            continue;
        };
        let number = (cells(row).first())
            .and_then(|cell| numbers(cell).first().copied())
            .filter(|number| !open.contains(number));
        open.extend(number);
    }
    open
}

/// The requirements `head` opens with, as `R4`, `R4/R5` or `R4 and R5`
/// name them; none where it opens with anything else.
fn numbers(head: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut rest = head;
    while let Some(after) = rest.strip_prefix('R') {
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let Ok(number) = digits.parse() else {
            break;
        };
        found.push(number);
        let after = after[digits.len()..].trim_start();
        let after = after.strip_prefix("and ").unwrap_or(after);
        rest = after.trim_start_matches(['/', ',', '&', '+', ' ']);
    }
    found
}

/// What sends a run back once more when its replies in `messages`, those
/// after the notice that showed it `rows` left unclear, hold no line for
/// one of them in the form that notice asked for (`settles`). None when
/// each has one. A check of words, with no model: it tells an answer that
/// weighed the request from "unclear; leave", and asks for no change.
pub(crate) fn unresolved(messages: &[Message], request: &str, rows: &[usize]) -> Option<String> {
    let request: Vec<String> = (crate::doubts::words(request).into_iter())
        .map(|(_, word)| word)
        .collect();
    let lines: Vec<&str> = (messages.iter())
        .filter(|message| message.role == "assistant")
        .flat_map(|message| message.content.lines())
        .collect();
    let mut open: Vec<String> = Vec::new();
    for &number in rows {
        let name = format!("R{number}");
        let settled = lines.iter().any(|line| settles(line, number, &request));
        if !settled && !open.contains(&name) {
            open.push(name);
        }
    }
    let first = open.first()?.clone();
    let (rows, each) = match open.len() {
        1 => ("a row", "it"),
        _ => ("rows", "each"),
    };
    Some(format!(
        "[{} check] The review left {rows} of {} unclear, and your answer has no line for \
         {each} that names the reading you took and quotes the request. Write one line for \
         {each}, `{first} | reading taken | \"the request's words for it\" | command run`, \
         with {SETTLING} or more words in a row quoted as the request writes them and a \
         command you ran that tests that reading. Where the request states nothing on the \
         case, write `{first} | {NOT_STATED} | \"the nearest words it does have\" | left as \
         is`, and change nothing for that row on a guess. Only your last message is passed \
         on as your answer, so end with your whole answer again, not only what changed.",
        crate::BRAND,
        and_list(&open)
    ))
}

/// Whether `line` of an answer settles requirement `number`'s row. It opens
/// with that number, after any marks of a list, a heading or a table; it
/// has a cell after that for the reading taken, which is no shrug such as
/// "unclear" though it may be `NOT_STATED`; it quotes, in quote marks or a
/// code span, `SETTLING` or more words found in a row in `request`, its
/// words as `doubts::words` gives them, the quote's middle left out or not;
/// and it is no row of the review's table copied back, with its verdict.
fn settles(line: &str, number: usize, request: &[String]) -> bool {
    let marks = |c: char| matches!(c, '-' | '*' | '|' | '`' | '>' | '#' | ' ' | '\t');
    let line = line.trim_start_matches(marks);
    // A numbered list's item, as "1. R4".
    let item = line.trim_start_matches(|c: char| c.is_ascii_digit());
    let line = match item.strip_prefix(['.', ')']) {
        Some(rest) if item.len() < line.len() => rest.trim_start_matches(marks),
        _ => line,
    };
    let cells = cells(line);
    let [head, reading, ..] = cells.as_slice() else {
        return false;
    };
    if cells.len() < 3 || !numbers(head).contains(&number) {
        return false;
    }
    let reading = reading.to_lowercase();
    let first = reading.split_whitespace().next().unwrap_or("?");
    let first = first.trim_end_matches([',', ';', ':', '.']);
    let shrugged = SHRUGS.contains(&first);
    if shrugged || judged(&cells).is_some() {
        return false;
    }
    crate::doubts::literals(line).into_iter().any(|range| {
        let quote = crate::doubts::words(&line[range]);
        let said: Vec<&str> = quote.iter().map(|(_, word)| word.as_str()).collect();
        crate::doubts::shared(&said, request).len() >= SETTLING
    })
}

/// `items` joined as a sentence lists them: "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `outputs` as a notice names them: "`a`", "`a` and `b`", or the first
/// `NAMED` and how many more.
fn files(outputs: &[String]) -> String {
    let shown: Vec<String> = (outputs.iter().take(NAMED))
        .map(|file| format!("`{file}`"))
        .collect();
    match shown.split_last() {
        Some((last, [])) => last.clone(),
        Some(_) if outputs.len() > NAMED => {
            let more = outputs.len() - NAMED;
            let noun = if more == 1 { "file" } else { "files" };
            format!("{} and {more} more {noun}", shown.join(", "))
        }
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// The files `request` names by a path or a file's name that were changed
/// after `since`, each once and as the request first writes it: a relative
/// path or a name is looked for in `workspace`, an absolute path where it
/// points.
pub(crate) fn outputs(request: &str, workspace: &Path, since: SystemTime) -> Vec<String> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for word in named(request) {
        // Before looking, which on Windows signs in to a share it names.
        if crate::paths::unsafe_raw(word, Some(workspace)).is_some() {
            continue;
        }
        let path = workspace.join(word);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || !meta.modified().is_ok_and(|at| at > since) {
            continue;
        }
        // `out.csv`, `./out.csv` and its full path are one file.
        let path = path.canonicalize().unwrap_or(path);
        if !found.iter().any(|(_, seen)| *seen == path) {
            found.push((word.to_owned(), path));
        }
    }
    found.into_iter().map(|(word, _)| word).collect()
}

/// The words of `request` that read as a file's path or name, each once and
/// as the request first writes it, `LOOKED` at most: the files a run on it
/// is to write or change, as far as its words say.
pub(crate) fn named(request: &str) -> Vec<&str> {
    let mut named: Vec<&str> = Vec::new();
    for word in crate::inputs::words(request).filter(|word| filelike(word)) {
        if named.contains(&word) {
            continue;
        }
        if named.len() == LOOKED {
            break;
        }
        named.push(word);
    }
    named
}

/// Whether `word` reads as a file's path or name, such as `out/a.csv` or
/// `report.json`, rather than a plain word, a number or a web address.
fn filelike(word: &str) -> bool {
    if word.contains("://") {
        return false;
    }
    let extension = word.rsplit_once('.').map(|(_, extension)| extension);
    word.contains(['/', '\\'])
        || extension.is_some_and(|extension| {
            extension.chars().all(char::is_alphanumeric)
                && extension.chars().any(char::is_alphabetic)
        })
}

/// The blocks of prose in `request`, its lines joined, with code, console
/// sessions, headings, tables and lone tags left out.
fn blocks(request: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut item = false;
    let mut fence: Option<&str> = None;
    // A console's output runs from its prompt to the next blank line.
    let mut output = false;
    let mut flush = |lines: &mut Vec<&str>, item: &mut bool| {
        if !lines.is_empty() {
            blocks.push(Block {
                text: lines.join(" "),
                item: *item,
            });
        }
        lines.clear();
        *item = false;
    };
    for raw in request.lines() {
        let line = raw.trim();
        if let Some(mark) = fence {
            if line.starts_with(mark) {
                fence = None;
            }
            continue;
        }
        if line.is_empty() {
            output = false;
            flush(&mut lines, &mut item);
            continue;
        }
        if output {
            continue;
        }
        if let Some(mark) = ["```", "~~~"].into_iter().find(|m| line.starts_with(m)) {
            flush(&mut lines, &mut item);
            fence = Some(mark);
            continue;
        }
        if prompt(line) {
            flush(&mut lines, &mut item);
            output = true;
            continue;
        }
        // Four spaces open a code block only where no paragraph is under
        // way, since a list item's next lines are indented too.
        let indented = raw.starts_with('\t') || (raw.starts_with("    ") && lines.is_empty());
        if indented || heading(line) || line.starts_with('|') || tag(line) {
            flush(&mut lines, &mut item);
            continue;
        }
        if let Some(rest) = bullet(line) {
            flush(&mut lines, &mut item);
            item = true;
            lines.push(rest);
            continue;
        }
        // An issue's title has no stop and runs straight into its body.
        if let [first] = lines.as_slice()
            && !item
            && !first.ends_with(['.', '!', '?', ':', ';', ','])
            && line.starts_with(char::is_uppercase)
        {
            flush(&mut lines, &mut item);
        }
        lines.push(line);
    }
    flush(&mut lines, &mut item);
    blocks
}

/// The sentences of `text`, split at a stop followed by a space or the end,
/// outside a code span and not after an abbreviation or before a word in
/// lower case that is no name; a bang ends one only before a word that
/// opens with a capital and is not all capitals, outside a quoted string.
pub(crate) fn split(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut sentences = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < bytes.len() {
        let c = bytes[at];
        if c == b'`' {
            let run = bytes[at..].iter().take_while(|&&b| b == b'`').count();
            let ticks = &text[at..at + run];
            // A span never closed is plain text from its ticks on.
            at += run + text[at + run..].find(ticks).map_or(0, |end| end + run);
            continue;
        }
        let stop = matches!(c, b'.' | b'!' | b'?');
        if stop && bytes.get(at + 1).is_none_or(|next| *next == b' ') {
            let last = text[start..at]
                .split_whitespace()
                .last()
                .unwrap_or_default();
            let last = last.trim_start_matches(['(', '"', '\'']).to_lowercase();
            let word = first_word(&text[at + 1..]);
            let next = word.chars().next();
            let goes_on = match c {
                // A name in lower case opens a sentence as a capital does,
                // as "isolate scopes. state_data is injected" shows.
                b'.' => {
                    ABBREVIATIONS.contains(&last.as_str())
                        || (next.is_some_and(char::is_lowercase)
                            && (!namelike(word) || SHORTENED.contains(&last.as_str())))
                }
                // A bang is as often part of what is written out, as in "and
                // ! (complement)" or "emits Bail out! ERROR message".
                b'!' => {
                    next.is_some_and(|next| !next.is_uppercase())
                        || shouted(word)
                        || quoted(&text[start..at])
                }
                _ => false,
            };
            if !goes_on {
                sentences.push(text[start..=at].trim());
                start = at + 1;
            }
        }
        at += 1;
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        sentences.push(rest);
    }
    sentences
}

/// Whether `sentence` states something the change must do, rather than how
/// to work, a question, a story or code.
fn states(sentence: &str) -> bool {
    if sentence.ends_with('?') {
        return false;
    }
    let prose = outside_code(sentence)
        .to_lowercase()
        .replace('\u{2019}', "'");
    let words: Vec<&str> = prose.split_whitespace().collect();
    let first =
        (words.first().copied().unwrap_or_default()).trim_matches(|c: char| !c.is_alphanumeric());
    // Two words do when the first asks for something, as "Export `Result`." does.
    if words.len() < 2 || (words.len() == 2 && !IMPERATIVES.contains(&first)) {
        return false;
    }
    // More than one word in five that reads as code is a snippet, not a sentence.
    if words.iter().filter(|word| codelike(word)).count() * 5 > words.len() {
        return false;
    }
    if PROCESS.iter().any(|phrase| prose.contains(phrase)) {
        return false;
    }
    if STORY.iter().any(|word| holds(&prose, word)) {
        return false;
    }
    let condition = CONDITIONS.contains(&first) && prose.contains(',');
    // A sentence that opens with a name in code and goes on with a word says
    // what that name does.
    let named = words[0] == SPAN && words[1].chars().all(char::is_alphabetic);
    IMPERATIVES.contains(&first)
        || condition
        || named
        || MARKERS.iter().any(|marker| holds(&prose, marker))
}

/// Whether `sentence`, in a request that is a specification, states
/// something it asks for: any sentence but a question, a story, how to work
/// or a snippet of code, where an interface spelled out in prose, such as
/// routes and what each returns, is no snippet.
fn specifies(sentence: &str) -> bool {
    if sentence.ends_with('?') {
        return false;
    }
    let prose = outside_code(sentence)
        .to_lowercase()
        .replace('\u{2019}', "'");
    let words: Vec<&str> = prose.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    // Mostly code is a snippet, unless it opens in words, as "Web API at
    // /api/x: save(POST, returns {id})" does.
    let opens_in_words = words.iter().take(3).all(|word| !codelike(word));
    let code = words.iter().filter(|word| codelike(word)).count();
    if code * 2 > words.len() && !opens_in_words {
        return false;
    }
    if PROCESS.iter().any(|phrase| prose.contains(phrase)) {
        return false;
    }
    !(STORY.iter())
        .filter(|word| !PAST.contains(word))
        .any(|word| holds(&prose, word))
}

/// `sentence` with each closed code span standing as `SPAN`.
fn outside_code(sentence: &str) -> String {
    let mut prose = String::new();
    let mut rest = sentence;
    while let Some(open) = rest.find('`') {
        let run = rest[open..].bytes().take_while(|&b| b == b'`').count();
        let ticks = &rest[open..open + run];
        let Some(close) = rest[open + run..].find(ticks) else {
            break;
        };
        prose.push_str(&rest[..open]);
        prose.push_str(SPAN);
        rest = &rest[open + run + close + run..];
    }
    prose.push_str(rest);
    prose
}

/// Whether `text` holds `phrase` as whole words, not as part of a name such
/// as `read-only`, `obj.only` or the "i" of "i.e.".
fn holds(text: &str, phrase: &str) -> bool {
    let inside = |c: char| c.is_alphanumeric() || matches!(c, '\'' | '-' | '_');
    let joined = |mut beside: std::str::Chars| match beside.next() {
        Some('.') => beside.next().is_some_and(char::is_alphanumeric),
        next => next.is_some_and(inside),
    };
    text.match_indices(phrase).any(|(at, _)| {
        let before: String = text[..at].chars().rev().take(2).collect();
        !joined(before.chars()) && !joined(text[at + phrase.len()..].chars())
    })
}

/// Whether `word`, outside a code span, reads as code: a path, a call, an
/// assignment, markup or a name with an underscore.
fn codelike(word: &str) -> bool {
    let word = word.trim_matches(['*', ',', '.', ';', ':', '!', '?']);
    let called = word
        .char_indices()
        .any(|(at, c)| c == '(' && word[..at].ends_with(char::is_alphanumeric));
    called
        || word.contains("::")
        || word.contains([
            '=', '/', '\\', '"', '{', '}', '[', ']', '<', '>', '#', '@', '$', '|', '_',
        ])
}

/// The first word of `text`, as far as the next space.
fn first_word(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or_default()
}

/// Whether `word`, outside a code span, reads as a name from code rather
/// than a word of prose: one that is `codelike`, has a dot between letters,
/// or a capital after a small letter, as `resetBailState` has, and is no
/// abbreviation.
fn namelike(word: &str) -> bool {
    let name = word.trim_end_matches(['*', ',', '.', ';', ':', '!', '?']);
    if ABBREVIATIONS.contains(&name.to_lowercase().as_str()) {
        return false;
    }
    let letters: Vec<char> = name.chars().collect();
    let dotted =
        (letters.windows(3)).any(|w| w[1] == '.' && w[0].is_alphabetic() && w[2].is_alphabetic());
    let humped = (letters.windows(2)).any(|w| w[0].is_lowercase() && w[1].is_uppercase());
    codelike(name) || dotted || humped
}

/// Whether `word` is written all in capitals, two letters or more, as the
/// `ERROR` of a message spelled out is.
fn shouted(word: &str) -> bool {
    let word = word.trim_matches(|c: char| !c.is_alphanumeric());
    word.chars().filter(|c| c.is_uppercase()).count() >= 2 && !word.contains(char::is_lowercase)
}

/// Whether `text` ends inside a quoted string: after a double quote not
/// yet closed.
fn quoted(text: &str) -> bool {
    let plain = text.matches('"').count() % 2 == 1;
    plain || text.matches('\u{201c}').count() > text.matches('\u{201d}').count()
}

/// Whether `line` opens a console session: a shell's or Python's prompt, or
/// a traceback.
fn prompt(line: &str) -> bool {
    let numbered = line
        .strip_prefix("In [")
        .and_then(|rest| rest.split_once("]:"))
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    numbered
        || [">>>", "$ ", "Traceback ("]
            .iter()
            .any(|p| line.starts_with(p))
}

/// Whether `line` is a markdown heading, not a sentence that starts with a
/// `#`, as a comment directive written out in prose does.
fn heading(line: &str) -> bool {
    let marks = line.bytes().take_while(|&b| b == b'#').count();
    let title = line[marks..].trim();
    marks > 0
        && (line.len() == marks || line[marks..].starts_with(' '))
        && !title.ends_with('.')
        && !title.contains(". ")
}

/// Whether `line` is a lone tag a task is wrapped in, as `<task>` is.
fn tag(line: &str) -> bool {
    let Some(name) = line.strip_prefix('<').and_then(|l| l.strip_suffix('>')) else {
        return false;
    };
    let name = name.strip_prefix('/').unwrap_or(name);
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The words of a list's item after its bullet or number.
fn bullet(line: &str) -> Option<&str> {
    if let Some(rest) = ["- ", "* ", "+ "].iter().find_map(|b| line.strip_prefix(b)) {
        return Some(rest.trim_start());
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = line[digits..]
        .strip_prefix(". ")
        .or_else(|| line[digits..].strip_prefix(") "));
    (1..=3)
        .contains(&digits)
        .then_some(rest)
        .flatten()
        .map(str::trim_start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sentences_that_state_what_must_hold_are_kept_in_order() {
        let request = "Add a `retry` option to the uploader. It is a nice idea. \
                       Failed uploads should be tried again up to three times. \
                       The client returns the last error when every try fails.";
        assert_eq!(
            sentences(request),
            [
                "Add a `retry` option to the uploader.",
                "Failed uploads should be tried again up to three times.",
                "The client returns the last error when every try fails.",
            ]
        );
    }

    #[test]
    fn instructions_about_how_to_work_are_not_requirements() {
        let request = "You are in a git checkout of the project at /src. Edit the source code in place.\n\n\
                       Do not modify or add test files unless the task requires it. \
                       Make the smallest correct change. When you are done, stop; \
                       your changes are collected from the working tree.\n\n\
                       <task>\nThe parser must reject a trailing comma.\n</task>";
        assert_eq!(
            sentences(request),
            ["The parser must reject a trailing comma."]
        );
    }

    #[test]
    fn code_spans_abbreviations_and_versions_do_not_end_a_sentence() {
        let request = "Wrappers (e.g. `withDefault`) must keep the flag. \
                       Calling `cfg.load()` on v1.2.3 files returns `None`. Values such as \
                       `\"a. B\"` must parse.";
        assert_eq!(
            sentences(request),
            [
                "Wrappers (e.g. `withDefault`) must keep the flag.",
                "Calling `cfg.load()` on v1.2.3 files returns `None`.",
                "Values such as `\"a. B\"` must parse.",
            ]
        );
    }

    #[test]
    fn code_blocks_indented_code_and_console_sessions_are_skipped() {
        let request = "The loader must accept tabs.\n\n\
                       ```\nfn must_return() -> bool { true }\n```\n\n\
                       \tif value: return None\n\n\
                       >>> load('x')\nValueError: it must not fail\n\n\
                       $ run --check\nerror: must not\n\n\
                       It should also accept spaces.";
        assert_eq!(
            sentences(request),
            [
                "The loader must accept tabs.",
                "It should also accept spaces."
            ]
        );
    }

    #[test]
    fn each_list_item_is_a_sentence_of_its_own() {
        let request = "Rules\n\n\
                       - the name must be unique\n\
                       - Empty names are rejected\n\
                       1. Only one default is allowed\n\
                       * **Order:** results are returned in the order given";
        assert_eq!(
            sentences(request),
            [
                "the name must be unique",
                "Empty names are rejected",
                "Only one default is allowed",
                "**Order:** results are returned in the order given",
            ]
        );
    }

    #[test]
    fn lines_broken_inside_a_paragraph_join_into_one_sentence() {
        let request = "When the cache is full the oldest entry\nmust be dropped first.";
        assert_eq!(
            sentences(request),
            ["When the cache is full the oldest entry must be dropped first."]
        );
    }

    #[test]
    fn an_issue_title_is_a_sentence_apart_from_its_body() {
        let request =
            "Export should keep empty rows\nOur report has gaps. Export should keep them.";
        assert_eq!(
            sentences(request),
            ["Export should keep empty rows", "Export should keep them."]
        );
    }

    #[test]
    fn questions_guesses_stories_and_snippets_are_dropped() {
        let request = "Should the cache be shared? It seems the lock should be held longer. \
                       I think it should return early. The old code was only reading half. \
                       Currently the pool cannot grow. \
                       \"DIRS\": os.getenv(\"X\").split(\",\") # should be filter(None, x) here. \
                       Each worker must hold its own lock.";
        assert_eq!(sentences(request), ["Each worker must hold its own lock."]);
    }

    #[test]
    fn conditions_and_what_a_named_api_does_are_requirements() {
        let request = "When the active consumer is cancelled, the next one is promoted. \
                       If the pool is empty the call blocks. \
                       `Channel.close()` cancels all consumers. \
                       `retry` option. \
                       Links in a chain are checked each on its own.";
        assert_eq!(
            sentences(request),
            [
                "When the active consumer is cancelled, the next one is promoted.",
                "`Channel.close()` cancels all consumers.",
                "Links in a chain are checked each on its own.",
            ]
        );
    }

    #[test]
    fn in_a_specification_each_plain_sentence_is_a_requirement_too() {
        let request = "The parser must accept tabs. Keys must be unique. Values should be trimmed. \
                       Each key returns its value. Keys are matched without case. \
                       Later keys break ties from earlier keys. \
                       Every reply holds `next` whether or not a cursor was given. \
                       Web API at /api/items/: save(POST, returns {id}), list(GET, returns {items}). \
                       I think the old parser was slow. Should keys allow dots? \
                       x = load(path_a, path_b) # [skip]";
        assert_eq!(
            sentences(request),
            [
                "The parser must accept tabs.",
                "Keys must be unique.",
                "Values should be trimmed.",
                "Each key returns its value.",
                "Keys are matched without case.",
                "Later keys break ties from earlier keys.",
                "Every reply holds `next` whether or not a cursor was given.",
                "Web API at /api/items/: save(POST, returns {id}), list(GET, returns {items}).",
            ]
        );
        // With fewer sentences that state a requirement by their wording, a
        // plain one is description.
        assert_eq!(
            sentences("The parser must accept tabs. Keys are matched without case."),
            ["The parser must accept tabs."]
        );
    }

    #[test]
    fn many_plain_rules_beside_one_ask_are_a_specification() {
        let request = "Add a CharClass variant to OptimizedExpr. Choice chains of qualifying \
                       alternatives collapse into CharClass holding merged ranges. Coalescing \
                       runs as the final optimizer pass. A choice alternative qualifies if it is \
                       a single-character Str. A wrapped alternative qualifies when its inner \
                       expression qualifies. A coalesced result is emitted only when merging \
                       produces fewer ranges. A single merged range simplifies to Range. \
                       Overlapping and adjacent ranges merge. Merged ranges are sorted ascending \
                       by start code point.";
        let found = sentences(request);
        assert_eq!(found.len(), 9, "{found:?}");
        assert!(found.contains(&"Overlapping and adjacent ranges merge.".to_owned()));
        // A few plain sentences beside an ask describe; only the ask is kept.
        let short = "Add a CharClass variant to OptimizedExpr. Choice chains collapse into it. \
                     Coalescing runs as the final pass. Ranges are sorted.";
        assert_eq!(
            sentences(short),
            ["Add a CharClass variant to OptimizedExpr."]
        );
    }

    #[test]
    fn a_line_that_starts_with_a_hash_and_reads_as_a_sentence_is_no_heading() {
        let request = "# Directives\n\n\
                       # begin-skip [RULES]: Start a region that must cover the lines after it.\n\
                       # end-skip: End the latest region, which must not cover this line.";
        assert_eq!(
            sentences(request),
            [
                "# begin-skip [RULES]: Start a region that must cover the lines after it.",
                "# end-skip: End the latest region, which must not cover this line.",
            ]
        );
    }

    #[test]
    fn a_bang_before_words_in_lower_case_or_brackets_ends_no_sentence() {
        let request = "The operators | (union), & (intersection) and ! (complement) must combine \
                       rules. Stop! Each rule must apply once.";
        assert_eq!(
            sentences(request),
            [
                "The operators | (union), & (intersection) and ! (complement) must combine rules.",
                "Each rule must apply once.",
            ]
        );
    }

    #[test]
    fn a_sentence_that_opens_with_a_name_is_a_sentence_of_its_own() {
        let request = "get_state_data(state) returns active data dict or None. \
                       state_data_values property snapshots all active data by state identifier. \
                       set_state_data(state, key, value) validates active state, declared key, \
                       and DataVar type constraints, raising InvalidDefinition on violation. \
                       get_data_changes() returns DataChangeInfo records accumulated during the \
                       current macrostep, cleared at each macrostep boundary, with state_id, key, \
                       old_value, new_value attributes.";
        let split = split(request);
        assert_eq!(split.len(), 4, "{split:?}");
        assert_eq!(
            split[1],
            "state_data_values property snapshots all active data by state identifier."
        );
        assert!(split[3].starts_with("get_data_changes() returns DataChangeInfo records"));
        let kept = sentences(request);
        assert!(kept.contains(&split[0].to_owned()), "{kept:?}");
        assert!(kept.contains(&split[3].to_owned()), "{kept:?}");
        assert_eq!(
            sentences("Parallel regions must isolate scopes. state_data is injected into each."),
            [
                "Parallel regions must isolate scopes.",
                "state_data is injected into each.",
            ]
        );
        assert_eq!(
            super::split("Both expose bail state. resetBailState clears all bail state."),
            [
                "Both expose bail state.",
                "resetBailState clears all bail state.",
            ]
        );
    }

    #[test]
    fn a_word_of_prose_in_lower_case_still_goes_on_from_the_stop_before_it() {
        assert_eq!(
            split(
                "Use a marker, e.g. something short. Keep it, i.e. foo, as is. Call the \
                 handler. then it returns."
            ),
            [
                "Use a marker, e.g. something short.",
                "Keep it, i.e. foo, as is.",
                "Call the handler. then it returns.",
            ]
        );
        let request = "The loader must accept tabs.\n\n```\nload()\n```\n\n\
                       returns `None` when the file must not be read.\n\n\
                       e.g. the cache must never be read twice.";
        assert_eq!(sentences(request), ["The loader must accept tabs."]);
    }

    #[test]
    fn a_short_form_before_a_name_ends_no_sentence() {
        for request in [
            "The min. chunk_size must be 4 bytes.",
            "Up to the max. retry_count is allowed, a.k.a. maxRetries in the config.",
            "Compare it w.r.t. base_url only, as in ver. v2_schema and ex. get_user().",
        ] {
            assert_eq!(split(request), [request]);
        }
        // As a whole word one still ends a sentence before a capital.
        assert_eq!(
            split("Each value is capped at the max. Values above it must be rejected."),
            [
                "Each value is capped at the max.",
                "Values above it must be rejected.",
            ]
        );
    }

    #[test]
    fn a_bang_inside_a_message_written_out_ends_no_sentence() {
        let request = "Teamcity emits Bail out! ERROR message, and a count of the tests.";
        assert_eq!(split(request), [request]);
        let request = "The reporter prints \"Bail out! Too many failures\" only once.";
        assert_eq!(split(request), [request]);
        assert_eq!(
            split("Stop! Each rule must apply once."),
            ["Stop!", "Each rule must apply once."]
        );
    }

    #[test]
    fn two_words_that_ask_for_something_are_a_requirement() {
        assert_eq!(
            sentences("Export `Result`. `retry` option. Respect `strict`."),
            ["Export `Result`.", "Respect `strict`."]
        );
    }

    #[test]
    fn a_sentence_said_twice_is_listed_once() {
        let request = "The flag must default to off.\n\nThe flag must default to off.";
        assert_eq!(sentences(request), ["The flag must default to off."]);
    }

    #[test]
    fn plain_description_is_not_a_requirement() {
        let request = "This module parses dates. Dates come in many shapes. Thanks.";
        assert!(sentences(request).is_empty());
    }

    #[test]
    fn the_notice_quotes_at_most_listed_sentences_and_counts_the_rest() {
        let all: Vec<String> = (1..=LISTED + 3)
            .map(|n| format!("Rule {n} must hold."))
            .collect();
        let text = notice(&all, false, &[]);
        assert!(text.starts_with("[Letronna check] Before ending, check your change"));
        assert!(text.contains(&format!("\n- R{LISTED}: Rule {LISTED} must hold.\n")));
        assert!(!text.contains(&format!("Rule {} must", LISTED + 1)));
        assert!(text.contains("\n- and 3 more in the request; check those too.\n"));
        assert!(text.ends_with(CHECK) && !text.contains(REVIEW));
        assert!(notice(&all, true, &[]).ends_with(&format!("\n{REVIEW} {CHECK}")));
    }

    #[test]
    fn the_notice_numbers_its_requirements_and_says_how_many() {
        let rules: Vec<String> = [
            "Dates must parse.",
            "Times must parse.",
            "Zones must parse.",
        ]
        .map(String::from)
        .into();
        let text = notice(&rules, true, &[]);
        assert!(text.contains("\n- R1: Dates must parse.\n- R2: Times must parse.\n- R3: "));
        assert_eq!(listed(&text), Some(3));
        // Past the quoted ones, only those numbered count.
        let all: Vec<String> = (1..=LISTED + 3)
            .map(|n| format!("Rule {n} must hold."))
            .collect();
        assert_eq!(listed(&notice(&all, false, &[])), Some(LISTED));
        assert_eq!(listed("- R1: Dates must parse."), None);
        assert_eq!(listed("Helper finished: R1 | missing | PASS"), None);
    }

    #[test]
    fn a_review_table_is_read_however_it_is_laid_out() {
        let report = "Checked the work.\n\n\
                      | Req | Case | Result | Command |\n\
                      |-----|------|--------|---------|\n\
                      | R1 | missing field | PASS | |\n\
                      | **R2** | plain value | **FAIL** | `node check.js` |\n\
                      R3 | two rests | fail | go run probe.go\n\
                      `R4` | close | Passed\n\
                      R5 is mentioned in prose, as is PASS.\n\
                      Verdict: wrong.";
        let found: Vec<(usize, bool)> = (rows(report).into_iter())
            .map(|(number, passed, _)| (number, passed))
            .collect();
        assert_eq!(found, [(1, true), (2, false), (3, false), (4, true)]);
        assert!(tabled(report));
        assert!(!tabled("Every behaviour holds. Verdict: right."));
    }

    #[test]
    fn a_verdict_with_its_evidence_in_the_same_cell_is_read() {
        // As a review wrote its whole table, what shows each row after a
        // dash, and the other ways a cell goes on past the word.
        let report = "| Requirement | Case | Status / evidence |\n\
                      |---|---|---|\n\
                      | R1 | first character is no separator (`b'{}'`) | PASS — `DecodingError` |\n\
                      | R1 | closed but never read | UNCLEAR — raises `Closed`; no rule names it |\n\
                      | R2 | values arrive before the body ends | FAIL — `python lazy.py`: read whole |\n\
                      | R3 | two forms in one call | PASS (3 cases) | `node both.js` |\n\
                      | R4 | empty input | FAIL: got `[]` |\n\
                      | R5 | later step first | PASS - both orders |\n\
                      | R6 | exact message | **FAIL** — says `bad`, not `invalid` |\n\
                      | R7 | wrapped value | FAIL. `node wrap.js` throws |\n\
                      | R8 | plain value | PASSED: as written |";
        let found: Vec<(usize, bool)> = (rows(report).into_iter())
            .map(|(number, passed, _)| (number, passed))
            .collect();
        let read = [
            (1, true),
            (1, false),
            (2, false),
            (3, true),
            (4, false),
            (5, true),
            (6, false),
            (7, false),
            (8, true),
        ];
        assert_eq!(found, read);
        // The run is held to them as to any row, a doubt as a doubt: it
        // is listed with the rows left unclear, and answered as one.
        let text = unchecked(report, 8).unwrap();
        assert!(
            text.starts_with(
                "[Letronna check] The review's table has these rows that failed:\n\
                 - | R2 | values arrive before the body ends | FAIL — `python lazy.py`: read \
                 whole |\n- | R4 | empty input"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "throws |\nand these left unclear:\n- | R1 | closed but never read | UNCLEAR — \
                 raises `Closed`; no rule names it |\n"
            ),
            "{text}"
        );
        assert_eq!(left_open(&text), [1]);
        assert!(left_unclear(
            "R1 | closed but never read | UNCLEAR — raises `Closed`"
        ));
        assert!(!left_unclear(
            "R2 | read whole | FAIL — unclear why it waits"
        ));
    }

    #[test]
    fn a_verdict_at_the_end_of_its_cases_cell_is_read() {
        // As a review wrote its whole table, the case and its verdict in
        // one cell with a dash between.
        let report = "| R1 | skipped results before the threshold do not count — PASS |\n\
                      | R1 | failures are listed by name (threshold 2) — PASS (no limit is set) |\n\
                      | R2 | an empty name gives an empty reason — UNCLEAR (\"the test's name\") |\n\
                      | R3 | the summary line is printed once - FAIL |\n\
                      | R4 | the count includes the last test – **FAIL** (`node count.js`: 2) |";
        let found: Vec<(usize, bool, bool)> = (rows(report).into_iter())
            .map(|(number, passed, row)| (number, passed, left_unclear(&row)))
            .collect();
        let read = [
            (1, true, false),
            (1, true, false),
            (2, false, true),
            (3, false, false),
            (4, false, false),
        ];
        assert_eq!(found, read);
        // Written again after a cut, the case and what shows it part at
        // the verdict, and read as the rows they were.
        let again = cut_rows(report, "cut").unwrap();
        assert_eq!(
            again,
            "\nRows of its table the quote above cut, written again:\n\
             R1 | skipped results before the threshold do not count | PASS\n\
             R1 | failures are listed by name (threshold 2) | PASS\n\
             R2 | an empty name gives an empty reason | UNCLEAR | (\"the test's name\")\n\
             R3 | the summary line is printed once | FAIL\n\
             R4 | the count includes the last test | FAIL | (`node count.js`: 2)"
        );
        let found: Vec<(usize, bool, bool)> = (rows(&again).into_iter())
            .map(|(number, passed, row)| (number, passed, left_unclear(&row)))
            .collect();
        assert_eq!(found, read);
        // A case that only mentions a verdict is no row.
        for case in [
            "the reporter prints PASS",
            "a failing test is marked — fail",
            "the exit code is non-zero-FAIL",
            "prints `ok` — PASS (in TAP) and FAIL otherwise",
            "the status reads PASS or FAIL",
            "the legend — PASS, FAIL or UNCLEAR",
        ] {
            assert!(!tabled(&format!("R1 | {case} | NOT RUN")), "{case}");
        }
        // A cell that is the word alone wins over one that ends with it.
        let alone = "R1 | a failing test is marked — FAIL | PASS | node tap.js";
        assert!(rows(alone)[0].1);
        // Of two that hold more after the case, no row is passed while one
        // says otherwise.
        let failed = "R1 | a passing test | its line is marked — PASS | FAIL — not `ok 1`";
        assert!(!rows(failed)[0].1);
    }

    #[test]
    fn a_cell_that_passes_a_case_and_then_doubts_or_fails_it_gives_no_verdict() {
        // As a review wrote one: the pass, then a doubt it had beside it.
        for both in [
            "R1 | set once | PASS (stays on the second hop); other keys untested — UNCLEAR",
            "R1 | the exit code | PASS — though the summary line FAILS to print",
            "R1 | the count is kept — PASS (UNCLEAR for a count of 0)",
        ] {
            assert!(!tabled(both), "{both}");
        }
        // A word of prose is no second verdict, and a row that fails or is
        // left unclear stays so whatever it says of passing.
        let read = |row: &str| (rows(row)[0].1, left_unclear(row));
        let passed = "R1 | the exit code | PASS — fails before the change, as it should";
        assert_eq!(read(passed), (true, false));
        let failed = "R1 | the exit code | FAIL — the line reads PASS where it should not";
        assert_eq!(read(failed), (false, false));
        let doubted = "R1 | which end | UNCLEAR — PASS on one reading, FAIL on the other";
        assert_eq!(read(doubted), (false, true));
        // Alone in its cell the word is the verdict, whatever shows it says.
        let alone = "R1 | a failing test | PASS | prints `FAIL: 1` for it";
        assert_eq!(read(alone), (true, false));
    }

    #[test]
    fn a_verdict_in_bold_that_closes_after_its_mark_is_read() {
        let report = "| R1 | exact count | **FAIL:** got 2 |\n\
                      | R2 | empty input | **PASS —** `[]` |\n\
                      | R3 | later step first | *UNCLEAR.* The request sets no order |\n\
                      | R4 | exact message | FAIL — **not** the one asked for |";
        let found: Vec<(usize, bool, bool)> = (rows(report).into_iter())
            .map(|(number, passed, row)| (number, passed, left_unclear(&row)))
            .collect();
        let read = [
            (1, false, false),
            (2, true, false),
            (3, false, true),
            (4, false, false),
        ];
        assert_eq!(found, read);
        // Written again, what shows it comes without the verdict's marks
        // and with its own.
        let again = cut_rows(report, "cut").unwrap();
        assert!(
            again.ends_with(
                "\nR1 | exact count | FAIL | got 2\nR2 | empty input | PASS\n\
                 R3 | later step first | UNCLEAR | The request sets no order\n\
                 R4 | exact message | FAIL | **not** the one asked for"
            ),
            "{again}"
        );
        // A colon run into bold words is no mark after a verdict.
        assert!(!tabled(
            "R1 | exact count | FAIL:**2** rows where 3 were asked for"
        ));
    }

    #[test]
    fn a_sentence_that_opens_with_a_requirements_number_is_no_row() {
        // A bar inside a code span cuts a cell as any bar does, so a
        // sentence of the report can end in what looks like a verdict's cell.
        for prose in [
            "R1: handles `a | b` — FAIL",
            "R3 is right but for `a | b`, where the summary line reads — PASS (3)",
            "R2 holds for a plain value, and for `a | b` too — PASS",
        ] {
            assert!(!tabled(prose), "{prose}");
        }
        // A row's first cell names requirements alone, however many.
        let read = |row: &str| {
            rows(row)
                .first()
                .map(|(number, passed, _)| (*number, *passed))
        };
        assert_eq!(read("R1 | handles `a | b` — FAIL"), Some((1, false)));
        assert_eq!(
            read("| **R4** / R5 | both ends | PASS — `t.sh` |"),
            Some((4, true))
        );
        assert_eq!(read("R4 and R5 | both ends — PASS"), Some((4, true)));
        // The bar inside a span still cuts: a row written whole in one, as
        // the brief shows a row, is read, and so is one whose first cell
        // says more beside a verdict alone in its cell.
        assert_eq!(read("`R1 | case | FAIL | node check.js`"), Some((1, false)));
        assert_eq!(
            read("R2 `test_iter` | it is an iterator | FAIL"),
            Some((2, false))
        );
    }

    #[test]
    fn a_row_kept_in_one_column_by_escaping_its_bars_is_read() {
        let report = "| Req | Row |\n|---|---|\n\
                      | R1 | pair branch only | FAIL \\| node m.ts gives 0 and 0 |\n\
                      | R2 | same cache | UNCLEAR \\| identity cannot show it |\n\
                      | R3 | trait branch | PASS |";
        let found: Vec<(usize, bool)> = (rows(report).into_iter())
            .map(|(number, passed, _)| (number, passed))
            .collect();
        assert_eq!(found, [(1, false), (2, false), (3, true)]);
        let again = cut_rows(report, "| R3 | trait branch | PASS |").unwrap();
        assert!(
            again.ends_with(
                "\nR1 | pair branch only | FAIL | node m.ts gives 0 and 0\nR2 | same cache | \
                 UNCLEAR | identity cannot show it"
            ),
            "{again}"
        );
        // A bar escaped inside a case is no end of a verdict's cell: the
        // word alone in its cell still wins.
        let listed = "| R1 | prints one of `PASS \\| FAIL \\| SKIP` | PASS | node tap.js |";
        assert!(rows(listed)[0].1);
    }

    #[test]
    fn a_case_that_opens_with_a_verdicts_word_in_prose_is_no_verdict() {
        for case in [
            "fails when the input is empty",
            "passes values through unchanged",
            "Fail fast on the first error",
            "FAIL-fast mode stops the run",
            "PASS and FAIL are both counted",
            "PASS/FAIL column is printed",
            "FAIL.md is written beside the log",
            "PASSING tests are listed first",
            "Passed: 3 of 3 is printed last",
            "unclear: which of the two wins",
        ] {
            assert!(!tabled(&format!("R1 | {case} | NOT RUN")), "{case}");
            // Nor does it change what the row's own verdict says.
            let passed = format!("R1 | {case} | PASS | node check.js");
            assert!(rows(&passed)[0].1, "{case}");
            assert!(!left_unclear(&passed), "{case}");
        }
        // A cell that is the word alone wins over one that goes on.
        let alone = "R1 | FAIL: the line printed for a failing test | PASS | node tap.js";
        assert!(rows(alone)[0].1);
        // Of two that go on after the case, no row is passed while one
        // says otherwise.
        let failed = "R1 | a passing test | PASS: printed | FAIL — `ok 1` is missing";
        assert!(!rows(failed)[0].1);
        let either = "R1 | a failing test | FAIL: `not ok 1` is missing | PASS — printed";
        assert!(!rows(either)[0].1);
    }

    #[test]
    fn a_case_that_quotes_a_verdict_gives_none_ahead_of_other_cells() {
        // The cell after the number is the case: what it quotes of a
        // summary line or a mark is no verdict where more cells follow.
        for row in [
            "R6 | `PASS (3)` is the summary line | \u{274c} | node tap.js",
            "R1 | a good row is marked — PASS | Fail — marked ok",
            "R1 | what it checks — PASS | NOT RUN",
            "R1 | PASS: the line printed for a passing test | see the log",
        ] {
            assert!(!tabled(row), "{row}");
        }
        // The verdict in a later cell is then the row's, whatever the case
        // quotes.
        let passed = "R6 | `FAIL: <name>` printed for each failing test | PASS — `node tap.js` \
                      shows it";
        assert!(rows(passed)[0].1);
        let failed = "R6 | `PASS (3)` is the summary line | FAIL — `node tap.js` prints 2";
        assert!(!rows(failed)[0].1);
        // As the row's last cell it still gives one, the case before it or
        // none at all, but not where it goes on past a bracket.
        assert!(rows("R1 | a good row is marked — PASS")[0].1);
        assert!(!rows("R1 | FAIL: `not ok 1` is missing")[0].1);
        assert!(!tabled("R6 | `PASS (3)` is the summary line"));
        // A row that says its test was not run is no row, whatever a later
        // cell says of why, unless a cell of it is a verdict alone.
        for row in [
            "R1 | adds | NOT RUN | import — FAILED (ImportError)",
            "R1 | adds | NOT RUN — no seam for it | collection: FAILED (1 error)",
        ] {
            assert!(!tabled(row), "{row}");
        }
        assert!(!rows("R1 | adds | NOT RUN before the fix | FAIL | pytest -q")[0].1);
        assert!(!rows("R1 | NOT RUNNING the hook leaves the file | FAIL — it is gone")[0].1);
    }

    #[test]
    fn a_long_list_is_shared_between_review_helpers() {
        assert_eq!(shares(5), [(1, 5)]);
        assert_eq!(shares(10), [(1, 10)]);
        assert_eq!(shares(11), [(1, 6), (7, 11)]);
        // Kombu's 37 go to four helpers, and no more than four start.
        assert_eq!(shares(37), [(1, 10), (11, 19), (20, 28), (29, 37)]);
        assert_eq!(shares(LISTED + 10).len(), REVIEWS);
        assert_eq!(shares(LISTED + 10).last(), Some(&(31, LISTED)));
    }

    #[test]
    fn a_review_brief_holds_the_request_and_the_cases_but_no_reasons() {
        let request = "Add `parse_range`. Ranges must be written as `a..b`.";
        let rules: Vec<String> = ["Ranges must be written as `a..b`.", "Open ranges default."]
            .map(String::from)
            .into();
        let whole = brief(request, &rules, (1, 2), &[], &[]);
        assert!(
            whole.contains(&format!("word for word:\n{request}\n")),
            "{whole}"
        );
        assert!(whole.contains("\n- R1: Ranges must be written as `a..b`.\n- R2: Open ranges"));
        assert!(whole.contains("Check each requirement. "), "{whole}");
        assert!(whole.contains(cases!()));
        assert!(whole.contains("git diff"));
        assert!(!whole.contains("with why"));
        let part = brief(request, &rules, (2, 2), &[], &[]);
        assert!(
            part.contains("Check R2 to R2; other helpers check the rest."),
            "{part}"
        );
        assert!(part.contains("written `R2 | case | PASS`"), "{part}");
    }

    #[test]
    fn a_review_is_asked_for_the_verdict_alone_in_its_cell() {
        // What shows a row goes in the cell after, where a helper left to
        // itself writes it on after the verdict, as `PASS — what shows it`.
        let alone = "a table of one row per requirement and case, the verdict alone in its \
                     cell and what shows it in the cell after, written `R1 | case | PASS` or \
                     `R1 | case | FAIL | command that shows it`, with a row for each";
        let rules: Vec<String> = ["Dates must parse.".to_owned()].into();
        let asked = brief("Dates must parse.", &rules, (1, 1), &[], &[]);
        assert!(asked.contains(alone), "{asked}");
        // The run that starts a review itself asks its helper for the same.
        assert!(REVIEW.contains(alone), "{REVIEW}");
    }

    #[test]
    fn the_reviews_started_for_a_run_come_in_its_notice() {
        let rules: Vec<String> = ["Dates must parse.", "Times must parse."]
            .map(String::from)
            .into();
        let reports = ["review-r1 finished: 3 rounds.\nR1 | iso | PASS\nR2 | utc | FAIL | t.sh"];
        let text = reviewed(&rules, &reports.map(String::from), &[], None, None, true);
        assert!(text.starts_with(&notice(&rules, false, &[])[..60]));
        assert_eq!(listed(&text), Some(2));
        assert!(text.contains("Helpers that haven't seen your work reviewed it"));
        assert!(text.contains("\n\nreview-r1 finished: 3 rounds.\nR1 | iso | PASS\n"));
        assert!(text.ends_with(CHECK) && !text.contains(REVIEW));
        assert_eq!(rows(&text).len(), 2);
    }

    #[test]
    fn a_review_with_no_table_that_could_be_read_is_told_with_its_report() {
        let rules: Vec<String> = ["Dates must parse.", "Times must parse."]
            .map(String::from)
            .into();
        let read = "review-r1 finished: 3 rounds.\nR1 | iso | PASS".to_owned();
        let prose = "review-r2 finished: 4 rounds.\n    R2 does not hold: `t.sh` prints 1.";
        // Beside a review whose table was read, after it.
        let text = reviewed(&rules, &[read], &[prose.into()], None, None, true);
        let said = "One review started for you came back with no table that could be read, so \
                    none of its rows is counted: its report follows. Read it for what it found, \
                    and fix what it says fails by changing the work, not the case, unless the \
                    request rules that case out.";
        assert!(
            text.contains(&format!("R1 | iso | PASS\n\n{said}\n\n{prose}\n\n{CHECK}")),
            "{text}"
        );
        assert!(!text.contains(REVIEW), "{text}");
        // It adds no row, so the run is still told R2 has none.
        assert_eq!(rows(&text).len(), 1);
        assert!(unchecked(&text, 2).unwrap().contains("has no row for R2."));
        // With no table read at all the run still asks for a review itself,
        // and reads what came back all the same.
        let both = [prose.to_owned(), prose.replace("r2", "r1")];
        let none = reviewed(&rules, &[], &both, None, None, true);
        let said = "2 reviews started for you came back with no table that could be read, so \
                    none of their rows is counted: their reports follow. Read them for what \
                    they found, and fix what they say fails by changing the work, not the case, \
                    unless the request rules that case out.";
        assert!(
            none.contains(&format!(
                "{REVIEW} {said}\n\n{}\n\n{}\n\n{CHECK}",
                both[0], both[1]
            )),
            "{none}"
        );
        assert_eq!(listed(&none), Some(2));
        assert!(!tabled(&none));
    }

    #[test]
    fn a_review_is_asked_for_a_row_for_each_reading_only_where_words_were_read_two_ways() {
        let rules: Vec<String> = ["Dates must parse in any order.".to_owned()].into();
        let plain = brief("Dates must parse.", &rules, (1, 1), &[], &[]);
        assert!(!plain.contains("more than one way"), "{plain}");
        let read = [crate::doubts::Reading {
            number: 1,
            words: "must parse in any order".into(),
        }];
        let asked = brief("Dates must parse.", &rules, (1, 1), &[], &read);
        // After all else, so the brief is otherwise as it was.
        let rows = asked.strip_prefix(plain.as_str()).unwrap();
        assert!(
            rows.starts_with(
                "\n\nThe author read these words of the request more than one way; its choice \
                 is not shown:\n- R1: \"must parse in any order\"\nFor each, name the plausible \
                 readings and write a row per reading, `R1 | reading: "
            ),
            "{rows}"
        );
        // A helper with a share of the rows is given the readings of its own.
        let rules: Vec<String> = (1..=24).map(|n| format!("Rule {n} must hold.")).collect();
        let read = [3, 20].map(|number| crate::doubts::Reading {
            number,
            words: format!("Rule {number} must hold"),
        });
        let part = brief("Rules.", &rules, (1, 8), &[], &read);
        assert!(part.contains("- R3: \"Rule 3 must hold\""), "{part}");
        assert!(!part.contains("- R20: \""), "{part}");
        let part = brief("Rules.", &rules, (17, 24), &[], &read);
        assert!(part.contains("- R20: \"Rule 20 must hold\""), "{part}");
        assert!(part.contains("`R20 | reading: "), "{part}");
        assert!(!part.contains("- R3: \""), "{part}");
        assert!(!brief("Rules.", &rules, (9, 16), &[], &read).contains("more than one way"));
        // A row written as asked is read as any other row is.
        let table = "R1 | reading: parts in any order | FAIL | t.sh";
        assert!(unchecked(table, 1).unwrap().contains(table));
    }

    #[test]
    fn with_only_the_testers_table_the_run_still_asks_for_a_review() {
        let rules: Vec<String> = ["Dates must parse.", "Times must parse."]
            .map(String::from)
            .into();
        let tested = "A tester wrote tests.\nR1 | iso | FAIL | t.sh | got None";
        let text = reviewed(&rules, &[], &[], Some(tested), None, true);
        assert!(text.contains(REVIEW), "{text}");
        assert!(!text.contains("Helpers that haven't seen your work reviewed it"));
        assert!(text.contains(&format!("{tested}\n\n{CHECK}")), "{text}");
    }

    #[test]
    fn reviews_that_ran_out_of_budget_are_not_told_as_reviews_that_found_nothing() {
        let rules: Vec<String> = ["Dates must parse.", "Times must parse."]
            .map(String::from)
            .into();
        let plain = notice(&rules, true, &[]);
        let all = reviewed(&rules, &[], &[], None, Some((2, 2)), true);
        let said = "The 2 reviews started for you ran out of budget before reporting, so \
                    nothing is reviewed yet: that is not a pass. ";
        // Said once, just before the ask for a review, with the rest as it was.
        assert_eq!(all, plain.replacen(REVIEW, &format!("{said}{REVIEW}"), 1));
        assert_eq!(listed(&all), Some(2));
        assert!(!tabled(&all));

        let one = reviewed(&rules, &[], &[], None, Some((1, 1)), true);
        assert!(
            one.contains("\nThe review started for you ran out of budget before reporting, so"),
            "{one}"
        );
        let some = reviewed(&rules, &[], &[], None, Some((3, 1)), true);
        assert!(
            some.contains(
                "\n1 of the 3 reviews started for you ran out of budget before reporting, \
                 and the rest gave no table, so nothing is reviewed yet: that is not a pass. \
                 First ask a helper"
            ),
            "{some}"
        );
        // With nothing known of how they ended, the ask stands alone.
        assert_eq!(reviewed(&rules, &[], &[], None, None, true), plain);
        // A run with no tokens left for a review of its own is told what
        // became of them, and not to start one that would be refused.
        let spent = reviewed(&rules, &[], &[], None, Some((2, 2)), false);
        assert!(!spent.contains(REVIEW), "{spent}");
        assert!(spent.ends_with(&format!("{said}{CHECK}")), "{spent}");
    }

    #[test]
    fn a_row_left_unclear_holds_the_run_as_one_that_failed_does() {
        let report = "A caveat.\nR1 | iso | PASS\n\
                      R2 | scope first | UNCLEAR | parent singletons start too; the words may rule it out";
        assert!(tabled(report));
        let text = unchecked(report, 2).unwrap();
        assert!(text.contains("The review's table has these rows left unclear:\n- R2 | scope"));
        assert!(text.contains("decide it by the request's words"), "{text}");
        // It is asked for a line that quotes the request, not for a shrug.
        assert!(
            text.contains(
                "For each row left unclear above, write one line in your answer, `R2 | reading \
                 taken | \"the request's words for it\" | command run`"
            ),
            "{text}"
        );
        // Silence is answered by quoting the nearest words and changing
        // nothing, not by a bare shrug, and a doubt is no failure to fix.
        assert!(
            text.contains(
                "A bare \"the request does not say\" settles nothing: where it states nothing \
                 on the case, write `R2 | not stated | \"the nearest words it does have\" | \
                 left as is`, and change nothing for that row on a guess."
            ),
            "{text}"
        );
        assert!(text.contains("not a failure"), "{text}");
        assert!(!text.contains("Fix what fails"), "{text}");
        assert!(text.ends_with("not only what changed."), "{text}");
        // A failed row that says unclear of why is a failed row, and a row
        // too long to show its verdict is still told by its list.
        let why = unchecked("R1 | iso | FAIL | t.sh | unclear | why", 1).unwrap();
        assert!(
            why.contains("has these rows that failed:\n- R1 | iso"),
            "{why}"
        );
        assert!(left_open(&why).is_empty());
        let long = format!("R1 | {} | UNCLEAR | which zone", "a long case ".repeat(40));
        let cut = unchecked(&long, 1).unwrap();
        assert!(!cut.contains("UNCLEAR"), "{cut}");
        assert_eq!(left_open(&cut), [1]);
        // A table with only failed rows is told nothing of doubts, and is
        // asked for the line only for a row it does not fix.
        let failed = unchecked("R1 | iso | PASS\nR2 | utc | FAIL | t.sh", 2).unwrap();
        assert!(failed.contains("has these rows that failed:\n- R2 | utc"));
        assert!(!failed.contains("left unclear"), "{failed}");
        assert!(
            failed.contains("For each failed row you do not fix, write one line in your answer"),
            "{failed}"
        );
        assert!(
            failed.contains("Fix what fails unless the request rules"),
            "{failed}"
        );
        assert!(left_open(&failed).is_empty());
        // And one with only rows missing for no line at all.
        let short = unchecked("R1 | a | PASS", 3).unwrap();
        assert!(!short.contains("write one line"), "{short}");
    }

    #[test]
    fn failed_rows_come_before_unclear_ones_and_each_kind_is_counted_apart() {
        // Twelve doubts written before eleven failures, as a review that
        // doubts early and fails late writes them.
        let mut report = String::new();
        for n in 1..=12 {
            report.push_str(&format!(
                "R{n} | doubt {n} | UNCLEAR | the words may mean either\n"
            ));
        }
        for n in 13..=23 {
            report.push_str(&format!("R{n} | case {n} | FAIL | t.sh {n}\n"));
        }
        report.push_str("R24 | fine | PASS\n");
        let text = unchecked(&report, 24).unwrap();
        let at = |words: &str| {
            text.find(words)
                .unwrap_or_else(|| panic!("{words}: {text}"))
        };
        assert!(
            text.starts_with(
                "[Letronna check] The review's table has these rows that failed:\n\
                 - R13 | case 13 | FAIL | t.sh 13\n"
            ),
            "{text}"
        );
        // Ten of each are shown, and each kind says how many more it has.
        assert!(at("- R22 | case 22") < at("- and 1 more that failed\nand these left unclear:\n"));
        assert!(!text.contains("- R23 | case 23"), "{text}");
        assert!(at("left unclear:\n- R1 | doubt 1 |") < at("- R10 | doubt 10"));
        assert!(at("- R10 | doubt 10") < at("- and 2 more left unclear\n"));
        assert!(!text.contains("- R11 | doubt 11"), "{text}");
        assert!(
            text.contains("For each row left unclear above, and each failed row you do not fix,"),
            "{text}"
        );
        // The rows shown as unclear are the ones an answer is held to.
        assert_eq!(left_open(&text), (1..=10).collect::<Vec<_>>());
        // With rows missing too, the lists go on from them.
        let both = unchecked("R1 | a | FAIL | t.sh\nR1 | b | UNCLEAR | which end", 2).unwrap();
        assert!(
            both.starts_with(
                "[Letronna check] The review's table has no row for R2, and has these rows \
                 that failed:\n- R1 | a | FAIL | t.sh\nand these left unclear:\n\
                 - R1 | b | UNCLEAR | which end\nTest each requirement without a row"
            ),
            "{both}"
        );
        assert_eq!(left_open(&both), [1]);
        // No other text holds a run to a line.
        assert!(left_open("- R1 | b | UNCLEAR | which end").is_empty());
        assert!(left_open(&notice(&["It must hold.".to_owned()], true, &[])).is_empty());
    }

    /// An assistant's reply saying `content`.
    fn answered(content: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    /// A request whose fourth sentence leaves open which end is meant.
    const OPEN: &str = "Add a `parse_range` function. Ranges must be written as `a..b`. \
                        Open ranges such as `a..` default to the end of the input.";

    #[test]
    fn a_line_that_quotes_the_request_for_the_reading_taken_settles_a_row() {
        let line = "R3 | the end of the whole input, not of the line | \"default to the end \
                    of the input\" | `cargo test open_range`";
        let answer = format!("Fixed R2.\n\n{line}\n\nAll rows pass now.");
        assert_eq!(unresolved(&[answered(&answer)], OPEN, &[3]), None);
        // However the line is laid out, in any reply after the notice: as
        // a list's item, a table's row or a heading, for two rows at once,
        // the quote in a code span, without the request's marks, or with
        // its middle left out.
        let laid = [
            "- R3 | whole input | \"end of the input\" | cargo test",
            "| R3 | whole input | \u{201c}Open ranges such as a.. default\u{201d} | cargo test |",
            "**R3** | whole input | 'the end of the input' | cargo test",
            "### R3 | whole input | \"to the end of the input\" | cargo test",
            "1. R3 | whole input | \"to the end of the input\" | cargo test",
            "R2/R3 | whole input | \"to the end of the input\" | cargo test",
            "R2 and R3 | whole input | \"to the end of the input\" | cargo test",
            "R3 | whole input | `default to the end of the input` | cargo test",
            "R3 | whole input | \"Open ranges \u{2026} default to the end\" | cargo test",
            "R3 | whole input | \"Open ranges such as [...] the end of the input.\" | ran it",
            "R3 | left end is inclusive | \"to the end of the input\" | cargo test",
        ];
        for line in laid {
            let replies = [answered(line), answered("Done.")];
            assert_eq!(unresolved(&replies, OPEN, &[3]), None, "{line}");
        }
        // Where the request states nothing on the case, saying so with the
        // nearest words is an answer, and no change is asked for.
        let silent = "R3 | not stated | \"default to the end of the input\" | left as is";
        assert_eq!(unresolved(&[answered(silent)], OPEN, &[3]), None);
        // Nothing is asked with no row left unclear.
        assert_eq!(unresolved(&[answered("Done.")], OPEN, &[]), None);
    }

    #[test]
    fn a_row_shrugged_off_or_settled_by_words_the_request_lacks_is_listed() {
        let listed = |answer: &str, rows: &[usize]| unresolved(&[answered(answer)], OPEN, rows);
        let text = listed("R3: unclear; the request does not say. Leave.", &[3]).unwrap();
        assert!(
            text.starts_with(
                "[Letronna check] The review left a row of R3 unclear, and your answer has no \
                 line for it that names the reading you took and quotes the request. Write \
                 one line for it, `R3 | reading taken | \"the request's words for it\" | \
                 command run`, with 3 or more words in a row quoted as the request writes them"
            ),
            "{text}"
        );
        // It is sent back for the line, not for a change made on a guess.
        assert!(
            text.contains(
                "write `R3 | not stated | \"the nearest words it does have\" | left as is`, and \
                 change nothing for that row on a guess."
            ),
            "{text}"
        );
        assert!(!text.contains("change the work"), "{text}");
        assert!(text.ends_with("not only what changed."), "{text}");
        // A quote the request does not hold or too little of it, a shrug
        // beside a quote, the review's own row copied back, a line with no
        // reading, another row's line, and prose settle nothing.
        let unsettled = [
            "R3 | whole input | \"defaults to the last byte\" | cargo test",
            "R3 | whole input | \"input\" | cargo test",
            "R3 | whole input | \"the input\" | cargo test",
            "R3 | unclear | \"end of the input\" is ambiguous; left",
            "R3 | Unclear, leave | \"default to the end of the input\" | none",
            "R3 | ? | \"default to the end of the input\" | -",
            "R3 | which end \"default to the end of the input\" means | UNCLEAR | input or line",
            "R3 | \"default to the end of the input\"",
            "R2 | whole input | \"default to the end of the input\" | cargo test",
            "R30 | whole input | \"default to the end of the input\" | cargo test",
            "As for R3, \"default to the end of the input\" is clear enough.",
            "R3: whole input, since it says \"to the end of the input\"; ran cargo test",
            "Leave.",
        ];
        for answer in unsettled {
            assert!(listed(answer, &[3]).is_some(), "{answer}");
        }
        // Each row without a line is named, once, and the settled ones are not.
        let answer = "R2 | both ends given | \"must be written as `a..b`\" | cargo test\n\
                      R1: unclear. Leave.";
        let text = listed(answer, &[1, 2, 3, 3]).unwrap();
        assert!(
            text.contains("left rows of R1 and R3 unclear, and your answer has no line for each"),
            "{text}"
        );
        assert!(text.contains("`R1 | reading taken | "), "{text}");
        // Only the run's own replies are read.
        let user = Message {
            role: "user".into(),
            content: "R3 | whole input | \"default to the end of the input\" | cargo test".into(),
            ..Default::default()
        };
        assert!(unresolved(&[user], OPEN, &[3]).is_some());
    }

    #[test]
    fn a_reviews_row_copied_back_settles_nothing_however_its_verdict_is_written() {
        let listed = |answer: &str| unresolved(&[answered(answer)], OPEN, &[3]);
        // With what shows the verdict in its cell, or the verdict at the
        // end of another, the row is the review's all the same.
        for row in [
            "R3 | which end \"default to the end of the input\" means | UNCLEAR — input or line",
            "R3 | whole input | PASS — \"default to the end of the input\"",
            "R3 | whole input | FAIL: \"default to the end of the input\" is not kept | t.sh",
            "R3 | which end | \"default to the end of the input\" — UNCLEAR (input or line)",
        ] {
            assert!(listed(row).is_some(), "{row}");
        }
        // A line of the form asked for settles its row, a dash in it or not.
        let line = "R3 | the end of the whole input — not of the line | \"default to the end \
                    of the input\" | cargo test";
        assert_eq!(listed(line), None);
    }

    #[test]
    fn a_doubt_a_cut_lost_is_written_again_as_a_doubt() {
        let report = "R1 | iso | PASS\nR2 | scope first | UNCLEAR | starts the parent's too\n";
        let again = cut_rows(report, "cut").unwrap();
        assert!(
            again.contains("R2 | scope first | UNCLEAR | starts the parent's too"),
            "{again}"
        );
    }

    #[test]
    fn a_review_is_asked_to_write_each_doubt_as_a_row() {
        let rules: Vec<String> = ["Dates must parse.".to_owned()].into();
        let asked = brief("Dates must parse.", &rules, (1, 1), &[], &[]);
        assert!(
            asked.contains("`R1 | case | UNCLEAR | what you doubt`"),
            "{asked}"
        );
        assert!(asked.contains("never only prose"), "{asked}");
        assert!(
            asked.contains("bending another requirement's words"),
            "{asked}"
        );
        assert!(REVIEW.contains("`R1 | case | UNCLEAR | what it doubts`"));
    }

    #[test]
    fn a_run_sent_back_fixes_the_case_and_runs_every_row_again() {
        let text = unchecked("R1 | iso | PASS\nR2 | utc | FAIL | t.sh", 2).unwrap();
        assert!(
            text.contains("stands for its whole case, not only its example"),
            "{text}"
        );
        assert!(text.contains("uses the request's words"), "{text}");
        assert!(
            text.contains("run every row's command again, those that passed too"),
            "{text}"
        );
        assert!(
            text.contains("tests a tester's report says are kept for you"),
            "{text}"
        );
        // With only a row missing there is no fix to check again.
        let missing = unchecked("R1 | iso | PASS", 2).unwrap();
        assert!(
            !missing.contains("run every row's command again"),
            "{missing}"
        );
    }

    #[test]
    fn the_check_tests_the_other_side_of_a_rule_each_order_and_each_category() {
        assert!(CHECK.contains("if they differ, and not, as long as,"));
        assert!(CHECK.contains("try them in each order, such as the later one first"));
        assert!(CHECK.contains("that uses the request's own words for it"));
    }

    #[test]
    fn the_check_allows_no_late_change_of_behaviour_on_a_guess() {
        assert!(CHECK.contains("change no behaviour the request does not state on a guess"));
        assert!(CHECK.contains("needs the request's words or a failing row behind it"));
        // Whichever way the run is sent back, it reads it.
        let stated = ["It must hold.".to_owned()];
        assert!(notice(&stated, false, &[]).contains("on a guess"));
        assert!(
            reviewed(
                &stated,
                &["R1 | plain | PASS".to_owned()],
                &[],
                None,
                None,
                true
            )
            .contains("on a guess")
        );
        assert!(!CHECK_OUTPUT.contains("on a guess"));
    }

    #[test]
    fn the_review_names_the_helper_type_that_judges_by_the_request() {
        // A general helper has neither the review type's prompt nor its rounds.
        assert!(REVIEW.contains("start a helper of type `review`, not general,"));
    }

    #[test]
    fn the_review_asks_a_row_for_each_way_a_case_has_been_missed() {
        for case in [
            "built by hand and passed straight to the method that reads it",
            "each other way the same input can be written",
            "matched by its exact words, not a generic error",
            "the new form beside the existing form of the same thing in one call",
            "found by searching the code, not only the call the request names",
            "the other side of a condition",
            "steps the requirement sets in no order, in each order",
            "uses the requirement's own words for it",
            "each case the project's own tests check of the existing feature nearest to it",
            "a query made for the first time only after the change it should see",
            "and each case you chose not to handle,",
        ] {
            assert!(REVIEW.contains(case), "{case}");
        }
        // A case set aside that the request decides fails, whatever kept it out,
        // and each row is judged by the request rather than by the code.
        assert!(REVIEW.contains("fails when the request's words say what should happen in it"));
        assert!(REVIEW.contains("not by what the code now does"));
        // What the run set aside goes to the helper, and a failed row is fixed
        // in the work.
        assert!(REVIEW.contains("each case you chose not to handle, with why"));
        assert!(REVIEW.contains("by changing the work, not the case"));
    }

    #[test]
    fn rows_a_cut_report_lost_are_written_again_after_it() {
        let report = "Checked.\n| R1 | missing field | PASS |\n| **R2** | plain value | \
                      **FAIL** | `node check.js` |\nR3 | two rests | fail\nDone.";
        // The quote kept the first row and lost the others.
        let quote = "    Checked.\n    | R1 | missing field | PASS |\n    [… cut …]\n    Done.";
        assert_eq!(
            cut_rows(report, quote).as_deref(),
            Some(
                "\nRows of its table the quote above cut, written again:\nR2 | plain value | \
                 FAIL | node check.js\nR3 | two rests | FAIL"
            )
        );
        // Written again, they read as the rows they were.
        let whole = format!("{quote}{}", cut_rows(report, quote).unwrap());
        let found: Vec<(usize, bool)> = (rows(&whole).into_iter())
            .map(|(number, passed, _)| (number, passed))
            .collect();
        assert_eq!(found, [(1, true), (2, false), (3, false)]);
        assert_eq!(cut_rows(report, report), None);
        assert_eq!(cut_rows("No table here.", ""), None);
        // A long case is cut, and past the limit the rest are counted.
        let long = format!("R1 | {} | PASS", "x".repeat(500));
        let again = cut_rows(&long, "").unwrap();
        assert!(again.ends_with(&format!("R1 | {} | PASS", "x".repeat(CASE_CHARS))));
        let many: String = (1..=CUT_SHOWN + 2)
            .map(|n| format!("R{n} | c | PASS\n"))
            .collect();
        assert!(cut_rows(&many, "").unwrap().ends_with("\nand 2 more rows"));
    }

    #[test]
    fn a_row_written_again_keeps_what_its_verdicts_cell_went_on_to_say() {
        let report = "| R1 | missing field | PASS — `KeyError` |\n\
                      | R2 | plain value | FAIL — `node check.js` prints 2 | expected 3 |\n\
                      | R3 | later step first | UNCLEAR (the request sets no order) |";
        let again = cut_rows(report, "cut").unwrap();
        assert_eq!(
            again,
            "\nRows of its table the quote above cut, written again:\nR1 | missing field | \
             PASS\nR2 | plain value | FAIL | `node check.js` prints 2 | expected 3\nR3 | later \
             step first | UNCLEAR | (the request sets no order)"
        );
        // Written again, they read as the rows they were.
        let found: Vec<(usize, bool, bool)> = (rows(&again).into_iter())
            .map(|(number, passed, row)| (number, passed, left_unclear(&row)))
            .collect();
        assert_eq!(
            found,
            [(1, true, false), (2, false, false), (3, false, true)]
        );
    }

    #[test]
    fn rows_of_a_report_written_ahead_of_the_work_hold_the_run_to_nothing() {
        let ahead = format!(
            "{AHEAD_OPENS}\nR1 | a | FAIL | pytest | ImportError: no module a\nR2 | b | NOT RUN\n\
             {AHEAD_CLOSES}"
        );
        assert!(!tabled(&ahead));
        // Beside a review's rows, only the review's count.
        let both = format!("{ahead}\nR1 | a | PASS\nR2 | b | FAIL | cargo test");
        let text = unchecked(&both, 2).unwrap();
        assert!(text.contains("- R2 | b | FAIL | cargo test"), "{text}");
        assert!(!text.contains("ImportError"), "{text}");
        assert!(!text.contains("has no row"), "{text}");
        // Indented, as a notice quotes a report, the marks still hold.
        let quoted = format!(
            "    {AHEAD_OPENS}\n    R1 | a | FAIL | pytest | ImportError\n    {AHEAD_CLOSES}"
        );
        assert!(!tabled(&quoted));
    }

    #[test]
    fn a_table_missing_requirements_or_failing_rows_sends_the_run_back() {
        let report = "| R1 | missing | PASS |\n\
                      | R2 | plain value | FAIL | node check.js |\n\
                      | R4 | close | PASS |\n\
                      | R9 | beyond the list | FAIL | x |";
        let text = unchecked(report, 5).unwrap();
        assert!(
            text.starts_with(
                "[Letronna check] The review's table has no row for R3 and R5, and has these \
                 rows that failed:\n- | R2 | plain value | FAIL | node check.js |\n"
            ),
            "{text}"
        );
        assert!(!text.contains("R9"), "{text}");
        assert!(text.contains("Test each requirement without a row yourself"));
        assert!(text.contains("Run each failed row's command again"));
        assert!(text.ends_with("not only what changed."));
        // Each row there and passing, nothing is left.
        let passed = "R1 | a | PASS\nR2 | b | PASS\nR2 | c | PASS";
        assert_eq!(unchecked(passed, 2), None);
        // Only missing rows: no failed ones are spoken of.
        let short = unchecked("R1 | a | PASS", 3).unwrap();
        assert!(
            short.starts_with("[Letronna check] The review's table has no row for R2 and R3. ")
        );
        assert!(!short.contains("failed"), "{short}");
    }

    #[test]
    fn a_row_that_waits_on_the_work_is_no_row() {
        // A tester writing tests ahead of the work marks each row so, and
        // the row says nothing of the work once the work is there.
        let ahead = "Run: pytest -q tests/test_letronna_spec.py\n\
                     R1 | adds | FAIL | pytest -q tests/test_letronna_spec.py | not yet implemented\n\
                     R2 | rejects | FAIL | pytest -q | Not yet implemented (parse_range is missing).";
        assert!(!tabled(ahead));
        let passed = format!("R1 | a | PASS\nR2 | b | PASS\n{ahead}");
        assert_eq!(unchecked(&passed, 2), None);
        // A row that failed on the work holds the run, whatever it says of why.
        let failed = format!(
            "{ahead}\nR2 | b | FAIL | pytest -q | got NotImplementedError: not yet implemented"
        );
        let text = unchecked(&failed, 2).unwrap();
        assert!(
            text.contains("- R2 | b | FAIL | pytest -q | got NotImplementedError"),
            "{text}"
        );
        assert!(!text.contains("R1 | adds"), "{text}");
        assert!(text.contains("has no row for R1"), "{text}");
    }

    #[test]
    fn the_files_a_request_names_are_listed_once_each_as_written() {
        let files = named(
            "Write the orders to `report.csv`, then check report.csv against `in/orders.json`. \
             See https://example.com/a.csv for the format, version 1.2 of it.",
        );
        assert_eq!(files, ["report.csv", "in/orders.json"]);
        assert!(named("Fix the parser so that ranges work.").is_empty());
    }

    #[test]
    fn a_case_the_request_decides_is_kept_even_where_the_code_makes_it_hard() {
        assert!(CHECK.contains(
            "A case the request's words decide is not yours to set aside or file as an assumption"
        ));
        assert!(CHECK.contains("find another way to tell them apart"));
    }

    #[test]
    fn the_notice_for_outputs_names_them_and_says_to_check_them_with_new_code() {
        let rules = ["Rows must be sorted by date.".to_owned()];
        let outputs = |count: usize| -> Vec<String> {
            (1..=count).map(|n| format!("out/part{n}.csv")).collect()
        };
        let text = notice(&rules, false, &outputs(1));
        assert_eq!(
            text,
            format!(
                "[Letronna check] Before ending, check `out/part1.csv` against each requirement \
                 the request states:\n- R1: Rows must be sorted by date.\n{CHECK_OUTPUT}"
            )
        );
        let two = notice(&rules, true, &outputs(2));
        assert!(
            two.contains("check `out/part1.csv` and `out/part2.csv` against"),
            "{two}"
        );
        assert!(
            two.ends_with(&format!("\n{REVIEW} {CHECK_OUTPUT}")),
            "{two}"
        );
        let many = notice(&rules, false, &outputs(NAMED + 2));
        let shown = "`out/part1.csv`, `out/part2.csv`, `out/part3.csv`, `out/part4.csv`, \
                     `out/part5.csv` and 2 more files against";
        assert!(many.contains(shown), "{many}");
        let one_more = notice(&rules, false, &outputs(NAMED + 1));
        assert!(
            one_more.contains("`out/part5.csv` and 1 more file against"),
            "{one_more}"
        );
    }

    /// Sets when `file` was last changed.
    fn changed_at(file: &Path, at: SystemTime) {
        let file = std::fs::File::options().write(true).open(file).unwrap();
        file.set_modified(at).unwrap();
    }

    #[test]
    fn outputs_are_the_files_a_request_names_that_changed_since_the_run_began() {
        let dir = crate::store::tests::Scratch::new("requirements-outputs");
        let elsewhere = crate::store::tests::Scratch::new("requirements-outputs-elsewhere");
        let since = SystemTime::now();
        let (before, after) = (
            since - Duration::from_secs(3600),
            since + Duration::from_secs(5),
        );
        std::fs::create_dir_all(dir.join("out")).unwrap();
        for (file, at) in [
            ("params.txt", after),
            ("out/table.csv", after),
            ("input.json", before),
            ("scratch.txt", after),
            ("results", after),
        ] {
            std::fs::write(dir.join(file), "made").unwrap();
            changed_at(&dir.join(file), at);
        }
        let summary = elsewhere.join("summary.md");
        std::fs::write(&summary, "made").unwrap();
        changed_at(&summary, after);
        let request = format!(
            "Fit the curve to input.json and save its parameters to **`params.txt`**. Put the \
             table in out/table.csv, a summary at {}, and the results in results. \
             Check ./params.txt and {} once more, and see https://example.com/out/table.csv.",
            summary.display(),
            dir.join("params.txt").display()
        );
        assert_eq!(
            outputs(&request, &dir, since),
            [
                "params.txt".to_owned(),
                "out/table.csv".to_owned(),
                summary.display().to_string(),
            ]
        );
    }
}
