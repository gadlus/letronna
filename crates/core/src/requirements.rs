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
//! a stop outside a code span and not after an abbreviation such as "e.g.".
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
    "First ask a helper that hasn't seen your work: with the agent tool, start a helper of type `review`, not general, and wait for its report. Give it the request word for word, the numbered requirements above, the files you changed or made and each case you chose not to handle, with why. Ask it to change no file, to work out each value it checks with code of its own, not your scripts or helpers, and to end its report with a table of one row per requirement and case, written `R1 | case | PASS` or `R1 | case | FAIL | command that shows it`, with a row for each of these cases that applies: ",
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
const CHECK: &str = "These are picked by their wording, so read the request once more for any behaviour it states that the list leaves out. For each, run or write a quick test that shows it holds through the entry point a caller uses, end to end, not only by reading internal state, minding exact messages, strings and orderings. A behaviour tied to an event, such as something being demoted, closed or removed, holds on every call that can cause that event, not only the one the sentence names: find each and test it. A sentence that limits a behaviour, with words such as only, unless, otherwise, if they differ, and not, as long as, or while keeping or preserving something, also says where it must not happen: test that side too, with the input closest to the line, such as a value that is there but empty or undefined, values that differ in one place, or new data for something that is to stay as it was. Code of yours that lets a stated behaviour be skipped on some path, such as a flag that turns it off, a default that leaves it out or an early return, needs the request to exempt that path; where it does not, make the behaviour happen there too. A case the request's words decide is not yours to set aside or file as an assumption, even where the code makes it hard, such as a read that cannot tell a missing value from one that is there but empty: find another way to tell them apart, such as checking the length or whether the key is there first, and keep to the request's rule. Where steps are set in no order, try them in each order, such as the later one first. Where the request maps categories to outputs, test each with an input from outside the project's own code, such as a caller's function or a returned error, that uses the request's own words for it. Where a sentence names several cases or inputs, try each one in every form it could be written, including any your parser or code might reject before it reaches your new check. Where the request lists the kinds of thing your change handles, such as several types, formats or sources, each other rule it states holds for every one of them: test each rule on each kind, not only on the first. Where it lets parts nest, test the rules inside a nested part too, including one at a position that is missing or empty. Where a sentence's words could mean more than one shape of input, such as a plain value or the same value wrapped in an object, accept and test every shape they could mean, not only the one your code already handles. Spell each the way the project already writes its parts, as its grammar, tests or docs show them, with the new syntax added where those parts allow it, and try that spelling first: it is the one a user writes, not one you invented. A class, function or file the request names that the project already has is the one to change, unless the request asks for a new one: find it and extend it rather than making a new one of that name beside it. Where your change does something the project already does elsewhere, such as reporting a failure, tracking a change or hiding an option, do it the same way: read how the project's own tests call and check the existing ones, follow that over a looser reading of the request's words, and test yours the same way, with the same calls and set-up, such as one object reused across tests or a reset between them. Where they make a change and then call or query for the first time, do that too rather than calling once beforehand to clear or warm it up, since a first call can take its own path. Fix any that fail; skip one that only gives background or the old behaviour. Remove any script or test you added only for this check, unless the task asks for tests. Only your last message is passed on as your answer, so end with your whole answer again, not only what changed.";

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
    "e.g", "i.e", "eg", "ie", "etc", "vs", "cf", "approx", "incl", "esp", "resp", "no", "fig", "al",
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
            // as "returns" after a snippet does, unless it opens an item.
            let lower = sentence.chars().next().is_some_and(char::is_lowercase);
            if lower && !(block.item && at == 0) {
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
/// left out, which a reviewer tends to agree with.
pub(crate) fn brief(request: &str, sentences: &[String], share: (usize, usize)) -> String {
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
         case, written `R{first} | case | PASS` or `R{first} | case | FAIL | command that \
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
    text
}

/// What the run is told when helpers that have not seen its work reviewed or
/// tested it for the run: the numbered requirements, as `notice` lists them,
/// the reviewers' `reports` with their tables, or with none the ask for a
/// review as `notice` makes it, what it is told of the tester's report when
/// there is one, `tested`, and the check to make after.
pub(crate) fn reviewed(sentences: &[String], reports: &[String], tested: Option<&str>) -> String {
    // With no review's table the run still asks for a review itself.
    let mut text = notice(sentences, reports.is_empty(), &[]);
    let at = text.find(CHECK).unwrap_or(text.len());
    text.truncate(at);
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
    if let Some(tested) = tested {
        text.push_str(tested.trim());
        text.push_str("\n\n");
    }
    text.push_str(CHECK);
    text
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

/// The rows of a review's table in `report`: each requirement's number,
/// whether the case passed, and the row as written. A line is a row when a
/// cell of it opens with `R` and a number and a later cell says PASS or
/// FAIL, however the helper laid the table out.
fn rows(report: &str) -> Vec<(usize, bool, String)> {
    let mut found = Vec::new();
    for line in report.lines() {
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
        if let Some(passed) = cells[1..].iter().find_map(|cell| verdict(cell)) {
            found.push((number, passed, line.trim().to_owned()));
        }
    }
    found
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

/// Whether a table's row `line` was left `UNCLEAR` rather than failed.
fn left_unclear(line: &str) -> bool {
    cells(line)
        .iter()
        .any(|cell| cell.eq_ignore_ascii_case(UNCLEAR))
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
            let at = (cells.iter().skip(1))
                .position(|cell| verdict(cell).is_some())
                .map_or(cells.len(), |at| at + 1);
            let case = cut(&cells[1..at].join(" | "), CASE_CHARS);
            let shows = cut(&cells[(at + 1).min(cells.len())..].join(" | "), ROW_CHARS);
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
/// that failed; None when it covers each and every row passed.
pub(crate) fn unchecked(report: &str, count: usize) -> Option<String> {
    let rows = rows(report);
    let missing: Vec<String> = (1..=count)
        .filter(|number| !rows.iter().any(|(row, _, _)| row == number))
        .map(|number| format!("R{number}"))
        .collect();
    let failed: Vec<&String> = (rows.iter())
        .filter(|(number, passed, _)| !passed && *number <= count)
        .map(|(_, _, row)| row)
        .collect();
    if missing.is_empty() && failed.is_empty() {
        return None;
    }
    let brand = crate::BRAND;
    let mut text = format!("[{brand} check] The review's table ");
    if !missing.is_empty() {
        text.push_str(&format!("has no row for {}", and_list(&missing)));
    }
    if !failed.is_empty() {
        if !missing.is_empty() {
            text.push_str(", and ");
        }
        let unclear = failed.iter().any(|row| left_unclear(row));
        text.push_str(match unclear {
            true => "has these rows that failed or were left unclear:",
            false => "has these rows that failed:",
        });
        for row in failed.iter().take(FAILED_SHOWN) {
            let row: String = row.chars().take(ROW_CHARS).collect();
            text.push_str(&format!("\n- {row}"));
        }
        if failed.len() > FAILED_SHOWN {
            text.push_str(&format!("\n- and {} more", failed.len() - FAILED_SHOWN));
        }
        text.push('\n');
    } else {
        text.push_str(". ");
    }
    if !missing.is_empty() {
        text.push_str(
            "Test each requirement without a row yourself, through the entry point a caller \
             uses, in every case the table asks for that applies to it. ",
        );
    }
    if !failed.is_empty() {
        text.push_str(
            "A failed row stands for its whole case, not only its example: fix the rule behind \
             it, then test one more input of that case written another way, such as one from \
             outside the project's own code that uses the request's words. Run each failed \
             row's command again and see it pass now, or fix what it shows. A fix for one row \
             can break another, so then run every row's command again, those that passed too, \
             and any tests a tester's report says are kept for you, and fix any that now fail. ",
        );
    }
    if failed.iter().any(|row| left_unclear(row)) {
        text.push_str(
            "A row left unclear is a doubt its reviewer could not settle: decide it by the \
             request's words, quote them in your answer, and test the case; where they settle \
             nothing, keep to the reading closest to them and say which you took. ",
        );
    }
    text.push_str(
        "Fix what fails unless the request rules that case out. Only your last message is \
         passed on as your answer, so end with your whole answer again, not only what changed.",
    );
    Some(text)
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
    let mut looked: Vec<&str> = Vec::new();
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for word in crate::inputs::words(request).filter(|word| filelike(word)) {
        if looked.contains(&word) {
            continue;
        }
        if looked.len() == LOOKED {
            break;
        }
        looked.push(word);
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
/// lower case.
fn split(text: &str) -> Vec<&str> {
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
            let next = text[at + 1..].trim_start().chars().next();
            // After a bang, words in lower case or in brackets go on, as "and
            // ! (complement)" does.
            let goes_on = match c {
                b'.' => {
                    ABBREVIATIONS.contains(&last.as_str()) || next.is_some_and(char::is_lowercase)
                }
                b'!' => next.is_some_and(|next| next.is_lowercase() || next == '('),
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
        let whole = brief(request, &rules, (1, 2));
        assert!(
            whole.contains(&format!("word for word:\n{request}\n")),
            "{whole}"
        );
        assert!(whole.contains("\n- R1: Ranges must be written as `a..b`.\n- R2: Open ranges"));
        assert!(whole.contains("Check each requirement. "), "{whole}");
        assert!(whole.contains(cases!()));
        assert!(whole.contains("git diff"));
        assert!(!whole.contains("with why"));
        let part = brief(request, &rules, (2, 2));
        assert!(
            part.contains("Check R2 to R2; other helpers check the rest."),
            "{part}"
        );
        assert!(part.contains("written `R2 | case | PASS`"), "{part}");
    }

    #[test]
    fn the_reviews_started_for_a_run_come_in_its_notice() {
        let rules: Vec<String> = ["Dates must parse.", "Times must parse."]
            .map(String::from)
            .into();
        let reports = ["review-r1 finished: 3 rounds.\nR1 | iso | PASS\nR2 | utc | FAIL | t.sh"];
        let text = reviewed(&rules, &reports.map(String::from), None);
        assert!(text.starts_with(&notice(&rules, false, &[])[..60]));
        assert_eq!(listed(&text), Some(2));
        assert!(text.contains("Helpers that haven't seen your work reviewed it"));
        assert!(text.contains("\n\nreview-r1 finished: 3 rounds.\nR1 | iso | PASS\n"));
        assert!(text.ends_with(CHECK) && !text.contains(REVIEW));
        assert_eq!(rows(&text).len(), 2);
    }

    #[test]
    fn with_only_the_testers_table_the_run_still_asks_for_a_review() {
        let rules: Vec<String> = ["Dates must parse.", "Times must parse."]
            .map(String::from)
            .into();
        let tested = "A tester wrote tests.\nR1 | iso | FAIL | t.sh | got None";
        let text = reviewed(&rules, &[], Some(tested));
        assert!(text.contains(REVIEW), "{text}");
        assert!(!text.contains("Helpers that haven't seen your work reviewed it"));
        assert!(text.contains(&format!("{tested}\n\n{CHECK}")), "{text}");
    }

    #[test]
    fn a_row_left_unclear_holds_the_run_as_one_that_failed_does() {
        let report = "A caveat.\nR1 | iso | PASS\n\
                      R2 | scope first | UNCLEAR | parent singletons start too; the words may rule it out";
        assert!(tabled(report));
        let text = unchecked(report, 2).unwrap();
        assert!(text.contains("has these rows that failed or were left unclear:\n- R2 | scope"));
        assert!(text.contains("decide it by the request's words"), "{text}");
        // A table with only failed rows is told nothing of doubts.
        let failed = unchecked("R1 | iso | PASS\nR2 | utc | FAIL | t.sh", 2).unwrap();
        assert!(failed.contains("has these rows that failed:\n- R2 | utc"));
        assert!(!failed.contains("left unclear"), "{failed}");
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
        let asked = brief("Dates must parse.", &rules, (1, 1));
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
