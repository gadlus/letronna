//! Slash commands the user adds: a prompt kept in a Markdown file, or a
//! skill, run as `/name` with what follows it put into the text; or a saved
//! workflow, run as `/name` with what follows it as its args.
//!
//! The files are Claude Code's: `name.md` with optional `description` and
//! `argument-hint` frontmatter, a subfolder making `folder:name`. The user's
//! live in `<data>/commands`; a project brings more in `.letronna/commands`
//! or `.claude/commands`, read from its own folder only, as helper types
//! are. Every skill switched on is a command by its name too, and so is
//! every saved workflow (see `workflow::saved`).
//!
//! The list is built afresh each time it is asked for, so an edit counts
//! without a restart, and each front end hands in its own commands, which
//! always win. After them a name goes by trust: the user's file, the user's
//! skill, the user's or a bundled saved workflow, the project's file, the
//! project's skill, the project's saved workflow, so a repository cannot
//! swap out a command the user relies on; the ones that lost stay listed,
//! saying what won, so `/help` can show it. A file counts when its name ends
//! in `.md` in lower case; `.MD` is passed over.
//!
//! What a command gives is put in the composer rather than sent, as a
//! skill's instructions are: a file from a cloned repository is anyone's,
//! and this way its text is read before it goes. So a text that would itself
//! run as a command when sent is refused. A project's is cleaned and marked
//! like its instruction file, capped in size, and never read through a
//! link. Claude Code's `!` lines, which run a shell command into the
//! prompt, are left as the text they are: nothing here runs anything. A
//! saved workflow is sent at once instead, as all it sends is a request to
//! call the `workflow` tool, whose launch card shows the script and waits
//! for the user before anything runs.

use crate::chat as backend;
use crate::skills::{self, Skill, Source};
use crate::untrusted;
use std::path::{Path, PathBuf};

/// The folder commands are kept in, in the data folder and a project's.
const COMMANDS_DIR: &str = "commands";

/// The largest command file read, in bytes: 32 KiB is some eight thousand
/// tokens, pages more than a prompt kept for reuse needs, so a larger one is
/// a mistake or a trick rather than a prompt.
const COMMAND_BYTES: u64 = 32_768;

/// Command files read from one folder of them, as many as skills are.
const ROOT_COMMANDS: usize = 200;

/// Entries of any kind looked at in one folder of commands, subfolders
/// included: four for every command read leaves room for notes and folders
/// beside them, and stops a folder of thousands being walked on every list.
const ROOT_ENTRIES: usize = ROOT_COMMANDS * 4;

/// Subfolders looked into below a commands folder: two makes `group:name`
/// and `group:kind:name`, deeper than Claude Code's own examples go.
const COMMAND_DEPTH: usize = 2;

/// The longest name a command can have, in characters: 40 holds
/// `group:kind:name` in plain words, and a longer one would crowd its
/// description off the menu's row.
const NAME_CHARS: usize = 40;

/// Where a command comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// One of the front end's own.
    BuiltIn,
    /// The user's `<data>/commands`.
    User,
    /// The project's `.letronna/commands` or `.claude/commands`.
    Project,
    /// A skill switched on, the user's or the project's.
    Skill,
    /// A saved workflow, bundled or from a file: it runs rather than filling
    /// the box, and has no body.
    Workflow,
}

/// A command as the list shows it.
#[derive(Clone, Debug)]
pub struct Command {
    /// What follows the slash: the file's name without `.md`, after its
    /// subfolders joined by `:`, or the skill's name.
    pub name: String,
    /// What it does: the frontmatter's, or else the start of the body.
    pub description: String,
    /// What to type after it, from `argument-hint`; empty when not given.
    pub hint: String,
    /// The text it gives, before its arguments go in.
    pub body: String,
    /// Where it comes from.
    pub origin: Origin,
    /// What has the name first, when something does: a command hidden so is
    /// listed for `/help` to say so, and never runs.
    pub hidden_by: Option<Origin>,
}

/// Every command a session in `workspace` can run beside a front end's own
/// `built_ins` (named without the slash), sorted by name, the ones that lost
/// a name marked.
pub fn list(workspace: Option<&Path>, built_ins: &[&str]) -> Vec<Command> {
    // An error here is `/skills`'s to report; the command files still count.
    let skills = skills::list(workspace).unwrap_or_default();
    let data = backend::data_dir();
    let workflows = workflows(&data, workspace);
    list_in(&data, workspace, &skills, &workflows, built_ins)
}

/// `list` with the data folder, the skills and the saved workflows (each with
/// whether it is the project's) given.
fn list_in(
    data: &Path,
    workspace: Option<&Path>,
    skills: &[Skill],
    workflows: &[(Command, bool)],
    built_ins: &[&str],
) -> Vec<Command> {
    let roots = roots(data, workspace);
    let files = |origin: Origin| {
        roots
            .iter()
            .filter(move |(_, from)| *from == origin)
            .flat_map(|(root, from)| read_root(root, *from))
    };
    let skills_of = |source: Source| {
        skills
            .iter()
            .filter(move |s| s.enabled && s.source == source)
            .filter_map(from_skill)
    };
    let workflows_of = |project: bool| {
        workflows
            .iter()
            .filter(move |(_, theirs)| *theirs == project)
            .map(|(command, _)| command.clone())
    };
    // By trust, so the first to take a name keeps it.
    let ordered = files(Origin::User)
        .chain(skills_of(Source::User))
        .chain(workflows_of(false))
        .chain(files(Origin::Project))
        .chain(skills_of(Source::Project))
        .chain(workflows_of(true));
    let mut out: Vec<Command> = Vec::new();
    for mut command in ordered {
        command.hidden_by = if built_ins.iter().any(|b| same(b, &command.name)) {
            Some(Origin::BuiltIn)
        } else {
            find(&out, &command.name).map(|winner| winner.origin)
        };
        out.push(command);
    }
    out.sort_by_key(|c| (c.name.to_lowercase(), c.hidden_by.is_some()));
    out
}

/// The command that runs as `name`, whatever its case.
pub fn find<'a>(commands: &'a [Command], name: &str) -> Option<&'a Command> {
    commands
        .iter()
        .find(|c| c.hidden_by.is_none() && same(&c.name, name))
}

/// The name and the arguments of `line` when it is a command, or `None` when
/// its first word goes on to another `/` or `\`, a path as in "/usr/bin/foo is
/// broken".
pub fn split(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start().strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (name, args) = rest.split_at(end);
    if name.contains(['/', '\\']) {
        return None;
    }
    Some((name, args.trim()))
}

/// What `command` gives for the user to read and send, refused when that text
/// would itself run as a command once sent.
pub fn fill(command: &Command, args: &str) -> Result<String, String> {
    let given = expand(command, args);
    if split(&given).is_some() {
        return Err(format!(
            "/{}'s text starts with a command, so it was not put in the box.",
            command.name
        ));
    }
    Ok(given)
}

/// What `command` gives run with `args`, in one pass so an argument is never
/// read for places itself: `$ARGUMENTS` is the arguments as typed, `$1` to
/// `$9` are always their words (so a price of `$5` is taken for one, while
/// `$10` and above stay), and a body with no place gets them after it.
fn expand(command: &Command, args: &str) -> String {
    let args = args.trim();
    let words: Vec<&str> = args.split_whitespace().collect();
    let mut out = String::with_capacity(command.body.len() + args.len());
    let mut placed = false;
    let mut rest = command.body.as_str();
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix("ARGUMENTS") {
            out.push_str(args);
            (placed, rest) = (true, tail);
            continue;
        }
        let mut next = after.chars();
        match (next.next(), next.next()) {
            (Some(digit @ '1'..='9'), after_digit)
                if !after_digit.is_some_and(|c| c.is_ascii_digit()) =>
            {
                let n = digit as usize - '1' as usize;
                out.push_str(words.get(n).copied().unwrap_or_default());
                (placed, rest) = (true, &after[1..]);
            }
            _ => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    if !placed && !args.is_empty() {
        out = format!("{}\n\n{args}", out.trim_end());
    }
    out
}

/// The folders command files are read from, first one first when names
/// clash: the user's, then the project's own `.letronna` before its
/// `.claude`.
fn roots(data: &Path, workspace: Option<&Path>) -> Vec<(PathBuf, Origin)> {
    let mut roots = vec![(data.join(COMMANDS_DIR), Origin::User)];
    if let Some(workspace) = workspace {
        for hidden in [".letronna", ".claude"] {
            let root = workspace.join(hidden).join(COMMANDS_DIR);
            // A linked folder on the way would read files from outside it.
            if !is_link(&workspace.join(hidden)) && !is_link(&root) {
                roots.push((root, Origin::Project));
            }
        }
    }
    roots
}

/// Whether `path` is itself a link (a junction too, on Windows).
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// The commands in one folder of them, from the first `ROOT_COMMANDS` files
/// in the order of their paths.
fn read_root(root: &Path, origin: Origin) -> Vec<Command> {
    let (mut files, mut seen) = (Vec::new(), 0);
    walk(root, "", 0, origin, &mut files, &mut seen);
    files
        .into_iter()
        .filter_map(|(path, name)| read_command(&path, name, origin))
        .collect()
}

/// Adds the `.md` files in `dir` and its subfolders to `files` with their
/// command names, counting every entry looked at in `seen`.
fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    origin: Origin,
    files: &mut Vec<(PathBuf, String)>,
    seen: &mut usize,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let room = ROOT_ENTRIES.saturating_sub(*seen);
    let mut entries: Vec<_> = entries.flatten().take(room).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if files.len() >= ROOT_COMMANDS || *seen >= ROOT_ENTRIES {
            return;
        }
        *seen += 1;
        // A project's links are passed over, so a planted one cannot put
        // some other file in the composer; the user's may be links they made.
        let linked = entry.file_type().is_ok_and(|kind| kind.is_symlink());
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if (linked && origin == Origin::Project) || file_name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            if depth < COMMAND_DEPTH {
                walk(
                    &path,
                    &format!("{prefix}{file_name}:"),
                    depth + 1,
                    origin,
                    files,
                    seen,
                );
            }
        } else if let Some(stem) = file_name.strip_suffix(".md") {
            let name = format!("{prefix}{stem}");
            if valid(&name) {
                files.push((path, name));
            }
        }
    }
}

/// The command in the regular UTF-8 file at `path` of at most
/// `COMMAND_BYTES`, a project's cleaned as its instruction file is.
fn read_command(path: &Path, name: String, origin: Origin) -> Option<Command> {
    let text = skills::read_text(path, COMMAND_BYTES)?;
    let (entries, body) = skills::split(&text);
    let description =
        skills::field(&entries, "description").unwrap_or_else(|| skills::summary_of(&body));
    let hint = skills::field(&entries, "argument-hint").unwrap_or_default();
    let (description, hint, body) = match origin {
        Origin::Project => (
            untrusted::visible(&description),
            untrusted::visible(&hint),
            untrusted::vetted("this project's command", &body),
        ),
        _ => (description, hint, body),
    };
    let body = typeable(&body);
    Some(Command {
        name,
        description,
        hint,
        body,
        origin,
        hidden_by: None,
    })
}

/// A skill as a command, when its name can be typed as one.
fn from_skill(skill: &Skill) -> Option<Command> {
    if !valid(&skill.name) {
        return None;
    }
    let (description, body) = match skill.source {
        Source::User if !skill.learned && !skills::installed(skill) => {
            (skill.description.clone(), skill.body.clone())
        }
        // Anyone's, or a model's: fenced as the prompt fences them.
        source => (
            untrusted::visible(&skill.description),
            untrusted::vetted(
                match source {
                    Source::Project => "this project's skill",
                    Source::Plugin => "a plugin's skill",
                    Source::User if skill.learned => "a learned skill",
                    Source::User => "an installed skill",
                    Source::BuiltIn => "a built-in skill",
                },
                &skill.body,
            ),
        ),
    };
    let body = typeable(&body);
    Some(Command {
        name: skill.name.clone(),
        description,
        hint: String::new(),
        body,
        origin: Origin::Skill,
        hidden_by: None,
    })
}

/// The saved workflows as commands, each with whether it is the project's.
#[cfg(feature = "workflows")]
fn workflows(data: &Path, workspace: Option<&Path>) -> Vec<(Command, bool)> {
    let mut commands = Vec::new();
    for saved in crate::workflow::saved::list(data, workspace) {
        if !valid(&saved.name) {
            continue;
        }
        // The file goes first, so a row cut short keeps it, and a project's
        // file cannot pass for the bundled workflow it replaces.
        let description = match &saved.from {
            Some(from) => {
                let shown = workspace.and_then(|w| from.strip_prefix(w).ok());
                format!("{}: {}", shown.unwrap_or(from).display(), saved.description)
            }
            None => saved.description,
        };
        let description = if saved.project {
            untrusted::visible(&description)
        } else {
            description
        };
        let command = Command {
            name: saved.name,
            description,
            hint: String::new(),
            body: String::new(),
            origin: Origin::Workflow,
            hidden_by: None,
        };
        commands.push((command, saved.project));
    }
    commands
}

/// No saved workflows in a build without them.
#[cfg(not(feature = "workflows"))]
fn workflows(_: &Path, _: Option<&Path>) -> Vec<(Command, bool)> {
    Vec::new()
}

/// `body` without the control characters a keyboard cannot type, lines and
/// tabs kept: it goes into the input box, and a terminal drawing an escape
/// there would act on it.
fn typeable(body: &str) -> String {
    body.chars()
        .filter(|&c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Whether `name` can be typed after a slash: up to `NAME_CHARS` of
/// something, with no spaces, path separators, control characters or
/// characters that show as nothing in it.
fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= NAME_CHARS
        && !name.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || untrusted::invisible(c)
                || matches!(c, '/' | '\\')
        })
}

/// Whether two names are the same command, whatever their case.
fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_BYTES, Command, NAME_CHARS, Origin, ROOT_ENTRIES, expand, fill, find, list_in,
        split,
    };
    use crate::skills::{Skill, Source};
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        // The process's id too, so two checkouts testing at once keep apart.
        let dir =
            std::env::temp_dir().join(format!("letronna-slash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn command(body: &str) -> Command {
        Command {
            name: "test".into(),
            description: String::new(),
            hint: String::new(),
            body: body.into(),
            origin: Origin::User,
            hidden_by: None,
        }
    }

    fn skill(name: &str, source: Source) -> Skill {
        Skill {
            name: name.into(),
            description: format!("the {name} skill"),
            body: format!("Follow the {name} steps."),
            folder: PathBuf::from(name),
            source,
            enabled: true,
            learned: false,
        }
    }

    #[test]
    fn arguments_go_where_the_body_asks_for_them() {
        let review = command("Review $ARGUMENTS, starting with $1 then $2.");
        assert_eq!(
            expand(&review, "  src/a.rs src/b.rs "),
            "Review src/a.rs src/b.rs, starting with src/a.rs then src/b.rs."
        );
        // A word not given is left out, rather than the placeholder kept.
        assert_eq!(
            expand(&review, "one"),
            "Review one, starting with one then ."
        );
    }

    #[test]
    fn a_body_with_no_placeholder_gets_the_arguments_after_it() {
        assert_eq!(
            expand(&command("Tidy up.\n"), "the tests"),
            "Tidy up.\n\nthe tests"
        );
        assert_eq!(expand(&command("Tidy up."), ""), "Tidy up.");
    }

    #[test]
    fn arguments_are_not_read_for_placeholders_themselves() {
        let echo = command("Say $ARGUMENTS and $1.");
        assert_eq!(expand(&echo, "$1 $ARGUMENTS"), "Say $1 $ARGUMENTS and $1.");
    }

    #[test]
    fn dollars_stay_as_written_but_one_to_nine_are_always_places() {
        let prices = command("Costs $100, or $0 with $X; $ at the end $");
        assert_eq!(
            expand(&prices, ""),
            "Costs $100, or $0 with $X; $ at the end $"
        );
        // Claude Code's own rule, so a kept prompt reads the same in both.
        assert_eq!(
            expand(&command("Budget is $5 per run."), ""),
            "Budget is  per run."
        );
    }

    #[test]
    fn a_text_that_would_run_as_a_command_is_not_put_in_the_box() {
        let root = scratch("fill");
        let (data, project) = (root.join("data"), root.join("project"));
        write(&project.join(".claude/commands/review.md"), "/mode bypass");
        write(&project.join(".claude/commands/look.md"), "/src/a.rs: $1");
        write(&project.join(".claude/commands/aside.md"), "//tmp is full");
        let listed = list_in(&data, Some(&project), &[], &[], &[]);
        let review = find(&listed, "review").unwrap();
        assert_eq!(
            fill(review, ""),
            Err("/review's text starts with a command, so it was not put in the box.".into())
        );
        assert!(fill(&command("  $ARGUMENTS"), "/logout").is_err());
        // A path, or the escape for a leading slash, is text as typed.
        let look = find(&listed, "look").unwrap();
        assert_eq!(fill(look, "why").unwrap(), "/src/a.rs: why");
        assert!(fill(find(&listed, "aside").unwrap(), "").is_ok());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_line_whose_first_word_is_a_path_is_text() {
        assert_eq!(split("/review 42 now"), Some(("review", "42 now")));
        assert_eq!(
            split("/frontend:component"),
            Some(("frontend:component", ""))
        );
        assert_eq!(split("/usr/bin/foo is broken"), None);
        assert_eq!(split("/tmp/ is full"), None);
        assert_eq!(split("//comment"), None);
        assert_eq!(split("/C:\\Users is odd"), None);
        assert_eq!(split("no slash"), None);
        // A lone slash is a command with no name, which none has.
        assert_eq!(split("/"), Some(("", "")));
        assert_eq!(split("/ x"), Some(("", "x")));
    }

    #[test]
    fn built_ins_then_the_user_s_then_the_project_s_keep_a_name() {
        let root = scratch("order");
        let (data, project) = (root.join("data"), root.join("project"));
        write(&data.join("commands/review.md"), "Yours: $ARGUMENTS");
        write(&data.join("commands/Help.md"), "Not the help.");
        write(
            &project.join(".claude/commands/review.md"),
            "The project's.",
        );
        write(&project.join(".claude/commands/deploy.md"), "Ship it.");
        write(
            &project.join(".letronna/commands/deploy.md"),
            "Ship it our way.",
        );
        write(&project.join(".claude/commands/ship.md"), "Ship the file.");
        let skills = [
            skill("deploy", Source::User),
            skill("pdf", Source::User),
            skill("ship", Source::Project),
        ];
        let listed = list_in(&data, Some(&project), &skills, &[], &["help", "model"]);
        let seen: Vec<(&str, Origin, Option<Origin>)> = listed
            .iter()
            .map(|c| (c.name.as_str(), c.origin, c.hidden_by))
            .collect();
        assert_eq!(
            seen,
            [
                ("deploy", Origin::Skill, None),
                ("deploy", Origin::Project, Some(Origin::Skill)),
                ("deploy", Origin::Project, Some(Origin::Skill)),
                ("Help", Origin::User, Some(Origin::BuiltIn)),
                ("pdf", Origin::Skill, None),
                ("review", Origin::User, None),
                ("review", Origin::Project, Some(Origin::User)),
                ("ship", Origin::Project, None),
                ("ship", Origin::Skill, Some(Origin::Project)),
            ]
        );
        let deploy = find(&listed, "DEPLOY").unwrap();
        assert_eq!(deploy.body, "Follow the deploy steps.");
        assert_eq!(find(&listed, "ship").unwrap().body, "Ship the file.");
        assert_eq!(find(&listed, "review").unwrap().body, "Yours: $ARGUMENTS");
        assert!(find(&listed, "help").is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "workflows")]
    #[test]
    fn saved_workflows_are_commands_that_keep_their_place_by_trust() {
        let root = scratch("workflows");
        let (data, project) = (root.join("data"), root.join("project"));
        let meta = |said: &str| {
            format!("export const meta = {{ name: \"x\", description: \"{said}\" }};\n")
        };
        write(&data.join("workflows/help.js"), &meta("Not the help."));
        write(&data.join("workflows/triage.js"), &meta("Sort the issues."));
        write(&data.join("commands/triage.md"), "Yours.");
        let ship = project.join(".letronna/workflows/ship.js");
        write(&ship, &meta("Ship\u{200b} it."));
        write(&project.join(".claude/commands/ship.md"), "The project's.");
        write(
            &project.join(".claude/commands/review.md"),
            "Not the review.",
        );
        let workflows = super::workflows(&data, Some(&project));
        let listed = list_in(&data, Some(&project), &[], &workflows, &["help"]);
        let seen: Vec<(&str, Origin, Option<Origin>)> = listed
            .iter()
            .map(|c| (c.name.as_str(), c.origin, c.hidden_by))
            .collect();
        assert_eq!(
            seen,
            [
                ("help", Origin::Workflow, Some(Origin::BuiltIn)),
                ("review", Origin::Workflow, None),
                ("review", Origin::Project, Some(Origin::Workflow)),
                ("ship", Origin::Project, None),
                ("ship", Origin::Workflow, Some(Origin::Project)),
                ("triage", Origin::User, None),
                ("triage", Origin::Workflow, Some(Origin::User)),
            ]
        );
        // A file's is named by where it is, the project's from inside it and
        // cleaned; the bundled one has only what it does.
        let shipped = listed
            .iter()
            .find(|c| c.origin == Origin::Workflow && c.name == "ship");
        let file = Path::new(".letronna").join("workflows").join("ship.js");
        assert_eq!(
            shipped.unwrap().description,
            format!("{}: Ship it.", file.display())
        );
        let triage = data.join("workflows").join("triage.js");
        let sorted = listed
            .iter()
            .find(|c| c.origin == Origin::Workflow && c.name == "triage");
        assert_eq!(
            sorted.unwrap().description,
            format!("{}: Sort the issues.", triage.display())
        );
        let review = find(&listed, "review").unwrap();
        assert!(
            !review.description.contains(".js"),
            "{}",
            review.description
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_file_reads_its_frontmatter_and_subfolders_name_a_group() {
        let root = scratch("files");
        let data = root.join("data");
        write(
            &data.join("commands/git/ship.md"),
            "---\ndescription: Push the branch\nargument-hint: [remote]\n---\nPush to $1.",
        );
        write(&data.join("commands/a/b/name.md"), "Two deep.");
        write(&data.join("commands/a/b/c/deep.md"), "Too deep.");
        write(&data.join("commands/notes.txt"), "Not a command.");
        write(&data.join("commands/two words.md"), "No name to type.");
        write(&data.join("commands/review\u{200b}.md"), "A lookalike.");
        let long = "x".repeat(NAME_CHARS + 1);
        write(&data.join(format!("commands/{long}.md")), "Too long.");
        write(&data.join("commands/bell.md"), "Ring\u{7} it.\u{1b}[2J");
        write(
            &data.join("commands/plain.md"),
            "# Plain\n\nFirst paragraph here.",
        );
        let listed = list_in(&data, None, &[], &[], &[]);
        let names: Vec<&str> = listed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a:b:name", "bell", "git:ship", "plain"]);
        // The user's own text is not vetted, but the box still cannot draw
        // an escape.
        assert_eq!(find(&listed, "bell").unwrap().body, "Ring it.[2J");
        let ship = find(&listed, "git:ship").unwrap();
        assert_eq!(
            (ship.description.as_str(), ship.hint.as_str()),
            ("Push the branch", "[remote]")
        );
        assert_eq!(expand(ship, "origin"), "Push to origin.");
        let plain = find(&listed, "plain").unwrap();
        assert_eq!(plain.description, "First paragraph here.");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_project_is_read_from_inside_it_capped_and_cleaned() {
        let root = scratch("project");
        let (data, project) = (root.join("data"), root.join("project"));
        let secret = root.join("secret.md");
        write(&secret, "the user's private notes");
        let commands = project.join(".claude/commands");
        write(&commands.join("hidden.md"), "Fix it\u{200b} now.\u{1b}[2J");
        write(
            &commands.join("huge.md"),
            &"x".repeat(COMMAND_BYTES as usize + 1),
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&secret, commands.join("peek.md")).unwrap();
            std::os::unix::fs::symlink(root.join("elsewhere"), commands.join("more")).unwrap();
            write(&root.join("elsewhere/peek.md"), "the user's private notes");
        }
        let mut off = skill("off", Source::User);
        off.enabled = false;
        let theirs = Skill {
            body: "Run it\u{200b}.".into(),
            ..skill("theirs", Source::Project)
        };
        write(
            &commands.join("labels.md"),
            "---\ndescription: Safe\u{202e}txt\nargument-hint: [a\u{200b}]\n---\nBody\u{200b}.",
        );
        let listed = list_in(&data, Some(&project), &[off, theirs], &[], &[]);
        let names: Vec<&str> = listed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["hidden", "labels", "theirs"]);
        let labels = find(&listed, "labels").unwrap();
        assert_eq!(
            (labels.description.as_str(), labels.hint.as_str()),
            ("Safetxt", "[a]")
        );
        for command in &listed {
            assert!(
                !command.body.contains(['\u{200b}', '\u{1b}']),
                "{}",
                command.body
            );
            assert!(
                command.body.starts_with("[Letronna's note:"),
                "{}",
                command.body
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_installed_skill_is_cleaned_and_the_user_s_own_is_not() {
        let data = scratch("installed");
        let folder = data.join("skills").join("tidy");
        write(&folder.join(".installed-from"), "owner/repo\n");
        let tidy = Skill {
            description: "Tidies\u{202e} up".into(),
            body: "Tidy\u{200b} up.".into(),
            folder,
            ..skill("tidy", Source::User)
        };
        let mine = Skill {
            body: "Mine\u{200b}.".into(),
            ..skill("mine", Source::User)
        };
        let listed = list_in(&data, None, &[tidy, mine], &[], &[]);
        let tidy = find(&listed, "tidy").unwrap();
        assert_eq!(tidy.description, "Tidies up");
        assert!(
            tidy.body
                .starts_with("[Letronna's note: an installed skill holds 1 character"),
            "{}",
            tidy.body
        );
        assert!(tidy.body.ends_with("]\nTidy up."), "{}", tidy.body);
        assert_eq!(find(&listed, "mine").unwrap().body, "Mine\u{200b}.");
    }

    #[cfg(unix)]
    #[test]
    fn a_project_s_linked_commands_folder_is_not_followed_but_the_user_s_is() {
        use std::os::unix::fs::symlink;
        let root = scratch("links");
        let (data, project) = (root.join("data"), root.join("project"));
        write(&root.join("home/notes.md"), "the user's private notes");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        symlink(root.join("home"), project.join(".claude/commands")).unwrap();
        symlink(root.join("elsewhere"), project.join(".letronna")).unwrap();
        write(
            &root.join("elsewhere/commands/plan.md"),
            "Not the project's.",
        );
        std::fs::create_dir_all(data.join("commands")).unwrap();
        symlink(root.join("home"), data.join("commands/mine")).unwrap();
        let listed = list_in(&data, Some(&project), &[], &[], &[]);
        let seen: Vec<(&str, Origin)> =
            listed.iter().map(|c| (c.name.as_str(), c.origin)).collect();
        assert_eq!(seen, [("mine:notes", Origin::User)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_folder_of_other_files_stops_the_walk_at_its_cap() {
        let root = scratch("cap");
        let data = root.join("data");
        write(&data.join("commands/z.md"), "Past the cap.");
        let lot = data.join("commands/a");
        std::fs::create_dir_all(&lot).unwrap();
        for n in 0..ROOT_ENTRIES {
            std::fs::write(lot.join(format!("{n}.txt")), "").unwrap();
        }
        assert!(list_in(&data, None, &[], &[], &[]).is_empty());
        // With the folder and `z.md`, two fewer files make exactly the cap.
        std::fs::remove_file(lot.join("0.txt")).unwrap();
        std::fs::remove_file(lot.join("1.txt")).unwrap();
        let names: Vec<String> = list_in(&data, None, &[], &[], &[])
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["z"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
