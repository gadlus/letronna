//! Helper types: what a helper is told before its task, which tools it may
//! use, and whether it works in plan mode.
//!
//! Four are built in. More come from Markdown files -- the user's in
//! `<data>/agents`, then the project's `.claude/agents` and `.letronna/agents`
//! -- and a later file takes the place of an earlier type of the same name, so
//! a project can reshape even a built-in one. The files are read whenever they
//! are needed, as memory is, so an edit counts without a restart.
//!
//! The frontmatter is read by hand rather than with a YAML library: these
//! files hold a few `key: value` lines and lists, which do not justify a
//! dependency. Keys it does not know are passed over, so Claude Code's files
//! work as they are. Their tool names are put into ours, and one with no
//! counterpart here stays as written, where it matches no tool.

use std::path::{Path, PathBuf};

/// The most rounds a definition may give its helpers: four times the usual 50,
/// room for a long job that still ends a helper going round in circles.
const MOST_ROUNDS: usize = 200;
/// The rounds a review helper's turn may take: twice the usual 50, since it
/// checks every requirement and case with a command or two each, and most
/// reviews of a long request reached 50 with cases still unchecked.
pub(crate) const REVIEW_ROUNDS: usize = 100;
/// How much of each description the types line carries. The line goes into
/// every request, and Claude Code's descriptions can run to paragraphs of
/// examples, where a sentence is enough to choose by.
const ADVERT_CHARS: usize = 150;
/// Claude Code's names for the tools Letronna has too.
const CLAUDE_TOOLS: &[(&str, &str)] = &[
    ("Read", "read_file"),
    ("Grep", "search"),
    ("Glob", "list_dir"),
    ("Bash", "run_command"),
    ("Edit", "edit_file"),
    ("Write", "write_file"),
    ("WebFetch", "web_fetch"),
    ("WebSearch", "search_web"),
    ("ListMcpResourcesTool", "list_mcp_resources"),
    ("ReadMcpResourceTool", "read_mcp_resource"),
];
/// What the built-in types that only look may use.
const LOOKING: &[&str] = &[
    "read_file",
    "list_dir",
    "search",
    "web_fetch",
    "search_web",
    "list_sessions",
    "search_sessions",
    "read_job",
    "list_mcp_resources",
    "read_mcp_resource",
    "list_jobs",
];

/// A frontmatter key with its value, or with the `- item` lines under it.
type Field = (String, Vec<String>);

/// A kind of helper the model can start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentDef {
    pub name: String,
    /// What it is for, as the model chooses by.
    pub description: String,
    /// The tools it may use, or None for all of them.
    pub tools: Option<Vec<String>>,
    /// Tools it may not use, whatever `tools` says.
    pub disallowed: Vec<String>,
    /// Whether an MCP server's tool needs naming in `tools`, as it does in a
    /// Claude Code file, whose author reads a list as all the helper may use.
    pub mcp_by_name: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Plan mode: it may look and propose but change nothing.
    pub plan: bool,
    /// The rounds its turn may take, when not the helpers' usual number.
    pub rounds: Option<usize>,
    pub background: Option<bool>,
    pub isolation: Option<String>,
    /// What it is told before its task: the file's body.
    pub prompt: String,
}

impl AgentDef {
    /// Whether a helper of this type may use `tool`: never when `disallowed`
    /// names it, and otherwise when there is no `tools` list, the list names
    /// it, or it is an MCP server's that need not be named.
    pub fn allows(&self, tool: &str) -> bool {
        // A patch writes and edits files, so a type kept from either is kept
        // from it.
        let patch = tool == crate::patch::APPLY_PATCH;
        let kept = |name: &String| {
            name == tool || (patch && matches!(name.as_str(), "write_file" | "edit_file"))
        };
        if self.disallowed.iter().any(kept) {
            return false;
        }
        (tool.starts_with("mcp__") && !self.mcp_by_name)
            || self
                .tools
                .as_ref()
                .is_none_or(|list| list.iter().any(|name| name == tool))
    }
}

/// Every helper type: the built-ins, then those the user's and the project's
/// files define.
pub fn all(workspace: Option<&Path>) -> Vec<AgentDef> {
    loaded(&crate::chat::data_dir(), workspace)
        .into_iter()
        .map(|(def, _)| def)
        .collect()
}

/// The type called `name`, refused when there is none or when its tools list
/// names no tool this session has.
pub fn find(name: &str, workspace: Option<&Path>) -> Result<AgentDef, String> {
    let tools = crate::tools::all();
    let available: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    find_in(&crate::chat::data_dir(), workspace, name, &available)
}

fn find_in(
    data: &Path,
    workspace: Option<&Path>,
    name: &str,
    available: &[&str],
) -> Result<AgentDef, String> {
    let defs = loaded(data, workspace);
    let Some((def, path)) = defs.iter().find(|(def, _)| def.name == name) else {
        let names: Vec<&str> = defs.iter().map(|(def, _)| def.name.as_str()).collect();
        return Err(format!(
            "No helper type \"{name}\". Available: {}.",
            names.join(", ")
        ));
    };
    let Some(list) = &def.tools else {
        return Ok(def.clone());
    };
    if list
        .iter()
        .any(|tool| available.contains(&tool.as_str()) && def.allows(tool))
    {
        return Ok(def.clone());
    }
    let unknown: Vec<&str> = list
        .iter()
        .map(String::as_str)
        .filter(|tool| !available.contains(tool))
        .collect();
    let unknown = if unknown.is_empty() {
        String::new()
    } else {
        format!(" (unknown: {})", unknown.join(", "))
    };
    let file = path.as_ref().map_or_else(
        || "its definition".to_owned(),
        |path| path.display().to_string(),
    );
    Err(format!(
        "Helper type \"{name}\" would run with no tools: its tools list matched none{unknown}. \
         Fix {file} or pick another type."
    ))
}

/// The line that tells the model about the types files define, which the
/// agent tool's own description cannot name; None when there are none.
pub fn advert(workspace: Option<&Path>) -> Option<String> {
    advert_in(&crate::chat::data_dir(), workspace)
}

pub(crate) fn advert_in(data: &Path, workspace: Option<&Path>) -> Option<String> {
    let types: Vec<String> = loaded(data, workspace)
        .into_iter()
        .filter(|(_, path)| path.is_some())
        .map(|(def, _)| {
            let mut about = def
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if about.chars().count() > ADVERT_CHARS {
                about = about.chars().take(ADVERT_CHARS).collect::<String>() + "…";
            }
            match about.trim_end_matches('.') {
                "" => def.name,
                about => format!("{} ({about})", def.name),
            }
        })
        .collect();
    if types.is_empty() {
        return None;
    }
    Some(format!(
        "Helper types from the user's and the project's own files, for the agent tool's type: {}.",
        types.join("; ")
    ))
}

/// Every type with the file it came from (None for a built-in), a later file
/// taking the place of an earlier type of the same name.
fn loaded(data: &Path, workspace: Option<&Path>) -> Vec<(AgentDef, Option<PathBuf>)> {
    let mut defs: Vec<_> = built_ins().into_iter().map(|def| (def, None)).collect();
    let mut sources = vec![(data.join("agents"), false)];
    if let Some(workspace) = workspace {
        sources.push((workspace.join(".claude").join("agents"), true));
        sources.push((workspace.join(".letronna").join("agents"), false));
    }
    for (dir, claude) in sources {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        // A folder lists in whatever order its file system keeps; sorted, the
        // types line reads the same on every request and the prompt stays
        // cached.
        paths.sort();
        for path in paths {
            let text = std::fs::read_to_string(&path);
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            let (Ok(text), Some(stem)) = (text, stem) else {
                continue;
            };
            let Some(def) = parse(&text, stem, claude) else {
                continue;
            };
            // Replaced where it stands, so the types line keeps its order.
            match defs.iter_mut().find(|(known, _)| known.name == def.name) {
                Some(slot) => *slot = (def, Some(path)),
                None => defs.push((def, Some(path))),
            }
        }
    }
    defs
}

/// The types every session has.
fn built_ins() -> Vec<AgentDef> {
    let names = |list: &[&str]| -> Option<Vec<String>> {
        Some(list.iter().map(|name| name.to_string()).collect())
    };
    vec![
        AgentDef {
            name: "general".into(),
            description: "Any task, with every tool.".into(),
            prompt: "Carry the whole task through, then report.".into(),
            ..Default::default()
        },
        AgentDef {
            name: "explore".into(),
            description: "Read-only: reads and searches files, past sessions and the web. \
                          Fetching a page or searching the web still needs the user's approval."
                .into(),
            tools: names(LOOKING),
            plan: true,
            prompt: "Find out what the task asks, and change nothing. Say where you found each \
                     thing: file paths and line numbers, quoting what matters."
                .into(),
            ..Default::default()
        },
        AgentDef {
            name: "plan".into(),
            description: "Read-only: works out how to do a task, step by step, and names the \
                          files it turns on."
                .into(),
            tools: names(LOOKING),
            plan: true,
            prompt: "Work out how the task should be done, and change nothing: the steps in \
                     order, and what each one touches. End with a \"Critical files\" list of \
                     the 3 to 5 files the work turns on."
                .into(),
            ..Default::default()
        },
        AgentDef {
            name: "review".into(),
            description: "Checks work by reading it and running its tests, and ends with a \
                          verdict."
                .into(),
            tools: names(&[LOOKING, &["run_command"]].concat()),
            // Not plan mode: running the tests is the point, and plan mode
            // refuses commands.
            plan: false,
            rounds: Some(REVIEW_ROUNDS),
            // A doubt is told apart from a failure here, where every review
            // reads it: a run may answer a row left unclear by leaving its
            // work as it is, and reviews filed there a difference their own
            // run had shown, from what the requirement's sentence covers or
            // from what the project's feature of the same kind does. A rule
            // the task has for a kind of case still decides it, and the
            // kinds are named: left as "unless your task says otherwise", a
            // task's line that files anything else it notices as a doubt
            // read as saying otherwise of every case.
            prompt: "Look for what is wrong with the work you are pointed at. Verify by running \
                     tests. Where the work does less than its request's words allow, or two of \
                     its parts disagree about the same thing, that is wrong, not an open question \
                     or an aside. Judge by those words, not by what the code does now or what \
                     the work chose: a behaviour they tie to an event, such as a demotion, a \
                     close or a removal, holds on every path in the code that causes that \
                     event, so search for each; and a case the work set aside or filed as an \
                     assumption is wrong when they say what should happen in it, even through a \
                     rule stated for another path to the same event. Say each doubt, caveat or \
                     problem you notice in your verdict as something to settle, and in a table \
                     if you were asked for one as a row of its own, not as an aside. A doubt is \
                     only what no run of yours could settle: where your own run shows the work \
                     giving another result than a sentence of the request covers, or, in a case \
                     none covers, than the project's own feature of the same kind gives for the \
                     same call, the row is FAIL with that command, not UNCLEAR, unless your task \
                     has a rule of its own for that kind of case, as for a guard, a case set \
                     aside or readings. Put any \
                     script or file you check with in the temp folder, never in the project, and keep each command \
                     you run short: a long script goes into its file in parts. End with a \
                     verdict: whether it is right, and what has to change if not."
                .into(),
            ..Default::default()
        },
        AgentDef {
            name: "tester".into(),
            description: "Writes and runs tests of work it has not seen, from its request's \
                          words alone, and reports which fail."
                .into(),
            tools: names(&[LOOKING, &["run_command", "write_file", "edit_file"]].concat()),
            // Not plan mode: it writes its tests and runs them.
            plan: false,
            prompt: "Test the work you are pointed at by what its request says should happen, \
                     not by what the code does. Read the code only to learn how to call it, write \
                     each test the way the project's own tests are written, and run it. A test \
                     that fails on what the request says is a finding to report, never one to \
                     change until it passes."
                .into(),
            ..Default::default()
        },
    ]
}

/// A definition from a file's text, named by its `name` key or else by the
/// file, and None for a file with no frontmatter, such as a README beside the
/// definitions; `claude` marks a file written for Claude Code.
fn parse(text: &str, file_name: &str, claude: bool) -> Option<AgentDef> {
    let (fields, prompt) = frontmatter(text)?;
    let values = |keys: &[&str]| {
        fields
            .iter()
            .rev()
            .find(|(key, _)| keys.contains(&key.as_str()))
            .map(|(_, values)| values.as_slice())
    };
    let text = |keys: &[&str]| {
        values(keys)
            .and_then(<[String]>::first)
            .map(|value| unquote(value).to_owned())
            .filter(|value| !value.is_empty())
    };
    let list = |keys: &[&str]| values(keys).map(|values| items(values, claude));
    let flag = |key: &str| text(&[key]).and_then(|value| value.parse::<bool>().ok());
    Some(AgentDef {
        name: text(&["name"]).unwrap_or_else(|| file_name.to_owned()),
        description: text(&["description"]).unwrap_or_default(),
        tools: list(&["tools"]),
        disallowed: list(&["disallowed", "disallowedTools"]).unwrap_or_default(),
        mcp_by_name: claude,
        // Claude Code's models are its own aliases, such as sonnet or inherit,
        // which name nothing on the user's gateway.
        model: text(&["model"]).filter(|_| !claude),
        effort: text(&["effort"]),
        plan: flag("plan") == Some(true)
            || text(&["permissionMode"]).is_some_and(|mode| mode == "plan"),
        rounds: text(&["rounds", "maxTurns"])
            .and_then(|rounds| rounds.parse::<usize>().ok())
            .filter(|&rounds| rounds > 0)
            .map(|rounds| rounds.min(MOST_ROUNDS)),
        background: flag("background"),
        isolation: text(&["isolation"]),
        prompt,
    })
}

/// The `key: value` lines between a file's opening and closing `---`, each
/// with the `- item` lines under it, and the text after them; None when the
/// file does not open with `---`.
fn frontmatter(text: &str) -> Option<(Vec<Field>, String)> {
    // Some Windows editors save a byte-order mark, which would hide the `---`
    // and with it every restriction the file sets.
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return None;
    }
    let mut fields: Vec<Field> = Vec::new();
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        // An indented key belongs to a nested map, which nothing here reads.
        if let Some(item) = trimmed.strip_prefix('-') {
            if let Some((_, values)) = fields.last_mut() {
                values.push(item.trim().to_owned());
            }
        } else if !line.starts_with([' ', '\t'])
            && let Some((key, value)) = line.split_once(':')
        {
            let value = value.trim();
            let values = if value.is_empty() {
                Vec::new()
            } else {
                vec![value.to_owned()]
            };
            fields.push((key.trim().to_owned(), values));
        }
    }
    let body = lines.collect::<Vec<_>>().join("\n");
    Some((fields, body.trim().to_owned()))
}

/// A list's names, split at commas and unquoted, with a Claude Code file's tool
/// names put into ours.
fn items(values: &[String], claude: bool) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| unquote(value).trim_matches(['[', ']']).split(','))
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .map(|item| {
            let ours = CLAUDE_TOOLS
                .iter()
                .find(|(theirs, _)| claude && *theirs == item);
            ours.map_or(item, |(_, ours)| ours).to_owned()
        })
        .collect()
}

/// The text inside a pair of quotes, or all of it when there are none.
fn unquote(text: &str) -> &str {
    ['"', '\'']
        .into_iter()
        .find_map(|quote| text.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;

    /// The tools a session has, standing in for the registry.
    const AVAILABLE: &[&str] = &[
        "read_file",
        "list_dir",
        "search",
        "write_file",
        "edit_file",
        "run_command",
        "read_job",
        "web_fetch",
        "search_web",
        "list_sessions",
        "mcp__docs__look_up",
    ];

    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("agent-defs-{name}"))
    }

    fn write(dir: &Path, file: &str, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
    }

    #[test]
    fn a_review_helper_keeps_its_scratch_out_of_the_project() {
        let review = (built_ins().into_iter())
            .find(|def| def.name == "review")
            .unwrap();
        // It has no write_file, so its scripts go through commands, which the
        // gate lets make new files in the temp folder and the judge reads whole.
        let tools = review.tools.unwrap();
        assert!(tools.contains(&"run_command".to_owned()));
        assert!(!tools.contains(&"write_file".to_owned()));
        assert!(
            review
                .prompt
                .contains("in the temp folder, never in the project")
        );
        assert!(review.prompt.contains("keep each command you run short"));
    }

    #[test]
    fn a_review_helper_has_the_rounds_a_whole_table_takes() {
        let review = (built_ins().into_iter())
            .find(|def| def.name == "review")
            .unwrap();
        assert_eq!(review.rounds, Some(REVIEW_ROUNDS));
        const { assert!(REVIEW_ROUNDS > 50 && REVIEW_ROUNDS <= MOST_ROUNDS) };
    }

    #[test]
    fn a_review_helper_judges_by_the_request_whatever_it_is_told() {
        let review = (built_ins().into_iter())
            .find(|def| def.name == "review")
            .unwrap();
        // The run writes the helper's task itself and tends to leave these out.
        assert!(
            review
                .prompt
                .contains("not by what the code does now or what the work chose")
        );
        assert!(
            review
                .prompt
                .contains("holds on every path in the code that causes that event")
        );
        assert!(
            review
                .prompt
                .contains("set aside or filed as an assumption is wrong when")
        );
        // A difference its own run shows is a failure, not a doubt: from the
        // request's sentence, or where none covers the case from what the
        // project's feature of the same kind does.
        assert!(
            review.prompt.contains(
                "A doubt is only what no run of yours could settle: where your own run shows \
                 the work giving another result than a sentence of the request covers, or, in \
                 a case none covers, than the project's own feature of the same kind gives for \
                 the same call, the row is FAIL with that command, not UNCLEAR"
            ),
            "{}",
            review.prompt
        );
        // A kind of case the task has a rule for is still decided by that
        // rule, and the kinds are named, so no other line of a task reads
        // as leave to file a failure as a doubt.
        assert!(review.prompt.contains(
            "not UNCLEAR, unless your task has a rule of its own for that kind of case, as for \
             a guard, a case set aside or readings."
        ));
        assert!(!review.prompt.contains("says otherwise"));
    }

    #[test]
    fn frontmatter_reads_lists_both_ways() {
        // Saved on Windows, with CRLF line ends.
        let text = "---\r\nname: docs\r\ntools: read_file, \"search\"\r\ndisallowed:\r\n  - run_command\r\n  - 'write_file'\r\n---\r\nWrite the docs.\r\n\r\nKeep them short.\r\n";
        let def = parse(text, "file", false).unwrap();
        assert_eq!(def.name, "docs");
        assert_eq!(def.tools.clone().unwrap(), ["read_file", "search"]);
        assert_eq!(def.disallowed, ["run_command", "write_file"]);
        assert_eq!(def.prompt, "Write the docs.\n\nKeep them short.");
        // A byte-order mark, as some Windows editors write, changes nothing.
        assert_eq!(
            parse(&format!("\u{feff}{text}"), "file", false),
            Some(def.clone())
        );
        // A list in brackets reads the same, and with no name the file names it.
        let brackets = parse("---\ntools: [read_file, search]\n---\n", "file", false).unwrap();
        assert_eq!(brackets.tools, def.tools);
        assert_eq!(brackets.name, "file");
        // With no frontmatter it is no definition.
        assert_eq!(parse("Just do it.", "bare", false), None);
    }

    #[test]
    fn a_project_definition_overrides_a_user_one() {
        let data = scratch("override");
        let project = data.join("project");
        let user = data.join("agents");
        write(
            &user,
            "docs.md",
            "---\ndescription: The user's own.\n---\nUser.",
        );
        write(&user, "notes.txt", "Not a definition.");
        let claude = project.join(".claude").join("agents");
        write(
            &claude,
            "docs.md",
            "---\ndescription: From Claude Code.\n---\nClaude.",
        );
        write(&claude, "README.md", "# Our agents\n\nOne per file.");
        let letronna = project.join(".letronna").join("agents");
        write(
            &letronna,
            "docs.md",
            "---\ndescription: The project's.\n---\nProject.",
        );

        let defs = loaded(&data, Some(&project));
        let names: Vec<&str> = defs.iter().map(|(def, _)| def.name.as_str()).collect();
        assert_eq!(
            names,
            ["general", "explore", "plan", "review", "tester", "docs"]
        );
        assert_eq!(defs[5].0.prompt, "Project.");
        assert_eq!(defs[5].1, Some(letronna.join("docs.md")));
        assert_eq!(loaded(&data, None)[5].0.prompt, "User.");

        // Only types from files are advertised: the tool names the built-ins.
        let advert = advert_in(&data, Some(&project)).unwrap();
        assert!(advert.ends_with(": docs (The project's)."), "{advert}");
        assert_eq!(advert_in(&project, None), None);
    }

    #[test]
    fn claude_tool_names_map_to_ours() {
        let text = "---\nname: checker\ntools: Read, Grep, Glob, Bash, Edit, Write, WebFetch, WebSearch, NotebookEdit\ndisallowedTools: Write\npermissionMode: plan\nmaxTurns: 12\nmodel: sonnet\n---\nCheck.";
        let def = parse(text, "checker", true).unwrap();
        let usable: Vec<&str> = AVAILABLE
            .iter()
            .copied()
            .filter(|tool| def.allows(tool))
            .collect();
        // Write is disallowed, NotebookEdit has no counterpart, and an MCP
        // server's tool, as in Claude Code, passes only when the list names it.
        assert_eq!(
            usable,
            [
                "read_file",
                "list_dir",
                "search",
                "edit_file",
                "run_command",
                "web_fetch",
                "search_web"
            ]
        );
        assert!(def.plan);
        assert_eq!(def.rounds, Some(12));
        assert_eq!(def.model, None);
        let named = parse("---\ntools: Read, mcp__docs__look_up\n---\n", "named", true).unwrap();
        assert!(named.allows("mcp__docs__look_up"));
        // Only a file written for Claude Code has its names put into ours, and
        // in our own an MCP server's tool passes unless it is denied.
        let ours = parse(text, "checker", false).unwrap();
        assert!(!ours.allows("read_file"));
        assert!(ours.allows("mcp__docs__look_up"));
        // Reading an MCP server's resources is looking, under either name.
        let resources = "---\ntools: ListMcpResourcesTool, ReadMcpResourceTool\n---\n";
        let resources = parse(resources, "resources", true).unwrap();
        let explore = built_ins().into_iter().find(|def| def.name == "explore");
        for def in [resources, explore.unwrap()] {
            assert!(def.allows("list_mcp_resources") && def.allows("read_mcp_resource"));
        }
    }

    #[test]
    fn a_type_kept_from_writing_or_editing_is_kept_from_patching() {
        let def = |disallowed: &str| {
            let text = format!("---\nname: looker\ndisallowedTools: {disallowed}\n---\nLook.");
            parse(&text, "looker", true).unwrap()
        };
        assert!(!def("Write").allows("apply_patch"));
        assert!(!def("Edit").allows("apply_patch"));
        assert!(def("Bash").allows("apply_patch"));
        // A type with a list of its own gets a patch only when it names one.
        let listed = parse("---\ntools: Read, Edit\n---\n", "listed", true).unwrap();
        assert!(!listed.allows("apply_patch"));
    }

    #[test]
    fn a_definition_with_no_usable_tools_is_refused() {
        let data = scratch("refused");
        let project = data.join("project");
        let letronna = project.join(".letronna").join("agents");
        write(
            &letronna,
            "typo.md",
            "---\ntools: Raed, read_file\ndisallowed: read_file\n---\nLook.",
        );

        let refused = find_in(&data, Some(&project), "typo", AVAILABLE).unwrap_err();
        assert!(
            refused.contains("matched none (unknown: Raed)"),
            "{refused}"
        );
        assert!(
            refused.contains("typo.md or pick another type"),
            "{refused}"
        );
        let missing = find_in(&data, Some(&project), "nope", AVAILABLE).unwrap_err();
        assert_eq!(
            missing,
            "No helper type \"nope\". Available: general, explore, plan, review, tester, typo."
        );
        assert!(find_in(&data, Some(&project), "review", AVAILABLE).is_ok());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let text = "---\nname: tidy\ncolor: blue\ndescription: Tidies up.\nskills:\n  - lint\nmcpServers:\n  github:\n    tools: create_issue\n---\nTidy.";
        let expected = AgentDef {
            name: "tidy".into(),
            description: "Tidies up.".into(),
            prompt: "Tidy.".into(),
            ..Default::default()
        };
        assert_eq!(parse(text, "tidy", false), Some(expected));
    }
}
