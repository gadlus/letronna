//! What `letronna` was asked for on the command line.

use letronna_core::gate::Mode;

pub const HELP: &str = "\
Letronna — a small agent in your terminal

  letronna                 open a conversation
  letronna -p \"question\"    ask once and exit
  letronna --continue      pick up this folder's most recent session
  letronna --resume        choose from the recent sessions
  letronna --resume <id>   open that session, by the id /where shows
  letronna gui             open the window instead
  letronna serve           answer chat apps and run schedules, no window
  letronna setup           name, sign in, provider, model, install
  letronna update          install the newest version
  letronna doctor          check the setup, the disk and the gateway
  letronna mcp             add, list, get or remove MCP servers
  letronna mcp serve       offer the browser, media and search tools over MCP
  letronna acp             serve an editor over the Agent Client Protocol
  letronna --uninstall     take it off again

Options
  -p, --print <prompt>     ask one question, print the answer, exit
  -c, --continue           resume this folder's most recent session
  -r, --resume [id]        choose a session, or open the one with this id
      --mode <mode>        how much runs without asking: default, accept-edits,
                           plan, auto, dont-ask or bypass
      --yes                --mode bypass: run tools without asking, bar risky ones
      --plan               --mode plan: read and search only; never write or run
      --model <id>         this session's model, leaving the default as it is
      --gateway <name>     the gateway that model is on
      --effort <level>     how hard it thinks in this session, or auto
      --offline            with doctor: leave the network alone
  -h, --help               this
  -V, --version            which version this is

With -p
      --output <form>      text (the answer), json, or stream-json
      --max-rounds <n>     tool rounds a message gets before it stops (25)
      --model <name>       use this model instead of the one set up
      --tools <a,b>        only these tools, by name
      --budget <tokens>    stop once this many tokens are spent, over every
                           message the run is sent
      --input-format <form>
                           text (the question), or stream-json: messages,
                           interrupts and answers as JSON lines on stdin
      --permission-prompt-tool stdio
                           with stream-json in, ask stdin about each call
                           that needs a yes
";

/// What to open with.
#[derive(Debug, PartialEq)]
pub enum Start {
    /// A session of its own.
    Fresh,
    /// The one used last, without being asked.
    Last,
    /// Pick from the recent ones.
    Choose,
    /// A named one, by the id in its file name.
    Named(u64),
}

/// How `-p` gives its answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Output {
    /// The answer alone, as it would read in a conversation.
    Text,
    /// One JSON object with the answer and what it took.
    Json,
    /// A JSON line each time the run moves on, then that object as the last line.
    StreamJson,
}

/// Where `-p` takes what to say from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// The question after `-p`.
    Text,
    /// JSON lines on stdin: messages, interrupts and answers to approvals.
    StreamJson,
}

pub struct Args {
    pub prompt: Option<String>,
    pub start: Start,
    /// The mode from `--mode`, `--yes` or `--plan`, or why `--mode` named
    /// none.
    pub mode: Result<Mode, String>,
    /// The session's own model, gateway and effort, for this run and after.
    pub model: Option<String>,
    pub gateway: Option<String>,
    pub effort: Option<String>,
    pub help: bool,
    pub version: bool,
    pub gui: bool,
    /// `letronna serve`: the chat apps and the schedules, with no window.
    pub serve: bool,
    pub install: bool,
    pub uninstall: bool,
    pub update: bool,
    pub doctor: bool,
    pub offline: bool,
    pub output: Output,
    pub input: Input,
    /// Whether a call that needs a yes is asked about on stdout and answered
    /// on stdin, from `--permission-prompt-tool stdio`.
    pub ask_stdio: bool,
    pub max_rounds: Option<usize>,
    pub tools: Option<Vec<String>>,
    pub budget: Option<u64>,
    /// The words after `letronna mcp`, which manage MCP servers.
    pub mcp: Option<Vec<String>>,
    /// `letronna acp`: speak the Agent Client Protocol on stdin and stdout.
    pub acp: bool,
}

pub fn parse(raw: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut raw = raw.peekable();
    let mut print = false;
    let (mut mode, mut yes, mut plan) = (None, false, false);
    let mut args = Args {
        prompt: None,
        start: Start::Fresh,
        mode: Ok(Mode::Default),
        model: None,
        gateway: None,
        effort: None,
        help: false,
        version: false,
        gui: false,
        serve: false,
        install: false,
        uninstall: false,
        update: false,
        doctor: false,
        offline: false,
        output: Output::Text,
        input: Input::Text,
        ask_stdio: false,
        max_rounds: None,
        tools: None,
        budget: None,
        mcp: None,
        acp: false,
    };
    // Only as the first word, so `-p mcp` still asks about it.
    if raw.next_if(|first| first == "mcp").is_some() {
        args.mcp = Some(raw.collect());
        return Ok(args);
    }
    // Only as the first word too, since an editor starts it with nothing
    // after and `letronna acp` anywhere else is a question about it.
    if raw.next_if(|first| first == "acp").is_some() {
        args.acp = true;
        return Ok(args);
    }
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "gui" => args.gui = true,
            "serve" => args.serve = true,
            "update" => args.update = true,
            "doctor" => args.doctor = true,
            "--offline" => args.offline = true,
            // A flag straight after, short or long, is not the question, so
            // `-p --output json "what is this"` and `-p -c "what is this"`
            // take the bare word after it. A flag is one word, so a question
            // can still start with a dash, as in `-p "-5 or 5?"`.
            "-p" | "--print" => {
                print = true;
                let flag = |next: &String| next.starts_with('-') && !next.contains(' ');
                if raw.peek().is_some_and(|next| !flag(next)) {
                    args.prompt = raw.next();
                }
            }
            "-c" | "--continue" => args.start = Start::Last,
            // `--resume` on its own offers the recent sessions; with an id it
            // opens that one, which is what the id in `/where` is for. Any
            // other word is refused rather than dropped, which left `-p` to
            // answer in a new session as if nothing was wrong.
            "-r" | "--resume" => {
                args.start = match raw.next_if(|next| !next.starts_with('-')) {
                    Some(id) => Start::Named(
                        id.parse()
                            .map_err(|_| "--resume takes a session id, like --resume 42.")?,
                    ),
                    None => Start::Choose,
                }
            }
            "--mode" => mode = Some(Mode::parse(&raw.next().unwrap_or_default())),
            "--yes" => yes = true,
            "--plan" => plan = true,
            "--output" => {
                args.output = match raw.next().as_deref() {
                    Some("text") => Output::Text,
                    Some("json") => Output::Json,
                    Some("stream-json") => Output::StreamJson,
                    _ => return Err("--output takes text, json or stream-json.".into()),
                }
            }
            "--input-format" => {
                args.input = match raw.next().as_deref() {
                    Some("text") => Input::Text,
                    Some("stream-json") => Input::StreamJson,
                    _ => return Err("--input-format takes text or stream-json.".into()),
                }
            }
            // Named as other agents' command lines name it, so a program that
            // drives one of them needs fewer changes to drive this; stdio is
            // the only way to ask.
            "--permission-prompt-tool" => match raw.next().as_deref() {
                Some("stdio") => args.ask_stdio = true,
                _ => return Err("--permission-prompt-tool takes stdio.".into()),
            },
            "--max-rounds" => {
                args.max_rounds = Some(above_zero(
                    raw.next(),
                    "--max-rounds takes a number of tool rounds above 0, like --max-rounds 10.",
                )?)
            }
            "--model" => {
                let name = raw
                    .next()
                    .filter(|name| !name.is_empty() && !name.starts_with('-'));
                args.model = Some(name.ok_or("--model takes the name of a model.")?);
            }
            "--tools" => args.tools = Some(tool_names(raw.next())?),
            "--budget" => {
                args.budget = Some(above_zero(
                    raw.next(),
                    "--budget takes a number of tokens above 0, like --budget 200000.",
                )?)
            }
            "--gateway" => args.gateway = raw.next(),
            "--effort" => args.effort = raw.next(),
            "setup" | "--install" => args.install = true,
            "--uninstall" => args.uninstall = true,
            "-h" | "--help" => args.help = true,
            "-V" | "--version" => args.version = true,
            // A bare word is the prompt, so `letronna "what is this"` works.
            other if !other.starts_with('-') && args.prompt.is_none() => {
                args.prompt = Some(other.to_owned())
            }
            _ => {}
        }
    }
    args.mode = picked(mode, yes, plan);
    let streamed = args.input == Input::StreamJson;
    if streamed && !print {
        return Err("--input-format stream-json only works with -p.".into());
    }
    if streamed && args.prompt.is_some() {
        return Err(
            "With --input-format stream-json the messages come on stdin, so -p takes no question."
                .into(),
        );
    }
    if streamed && args.output != Output::StreamJson {
        return Err("--input-format stream-json needs --output stream-json.".into());
    }
    if args.ask_stdio && !streamed {
        return Err(
            "--permission-prompt-tool stdio needs --input-format stream-json, which carries the answers."
                .into(),
        );
    }
    if print && args.prompt.is_none() && !streamed {
        return Err("-p takes the question, like -p \"what is this\".".into());
    }
    // The list to choose from is only shown in a conversation; `-p` would
    // answer in a new session instead.
    if args.once() && args.start == Start::Choose {
        return Err("With -p, --resume takes a session id, like --resume 42.".into());
    }
    let shaped = args.output != Output::Text
        || args.max_rounds.is_some()
        || args.tools.is_some()
        || args.budget.is_some();
    if shaped && !args.once() {
        return Err("--output, --max-rounds, --tools and --budget only work with -p.".into());
    }
    Ok(args)
}

impl Args {
    /// Whether this is a `-p` run, answered and left rather than a conversation.
    pub fn once(&self) -> bool {
        self.prompt.is_some() || self.input == Input::StreamJson
    }
}

/// The number after a flag, which has to be a whole one above zero.
fn above_zero<T: std::str::FromStr + PartialOrd + From<u8>>(
    value: Option<String>,
    error: &str,
) -> Result<T, String> {
    value
        .and_then(|value| value.parse::<T>().ok())
        .filter(|n| *n > T::from(0))
        .ok_or_else(|| error.to_owned())
}

/// The tool names after `--tools`, split at the commas.
fn tool_names(value: Option<String>) -> Result<Vec<String>, String> {
    let names: Vec<String> = value
        .filter(|value| !value.starts_with('-'))
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        return Err(
            "--tools takes tool names split by commas, like --tools read_file,search.".into(),
        );
    }
    Ok(names)
}

/// The mode the flags pick, or which flag to drop when `--mode` and a
/// shorthand for one both pick it.
fn picked(mode: Option<Result<Mode, String>>, yes: bool, plan: bool) -> Result<Mode, String> {
    let drop = |flag: &str, name: &str| {
        Err(format!(
            "`--mode` and `{flag}` both pick the mode; drop `{flag}`, which is `--mode {name}`."
        ))
    };
    match mode {
        Some(_) if yes => drop("--yes", "bypass"),
        Some(_) if plan => drop("--plan", "plan"),
        Some(mode) => mode,
        // Before modes, `--plan --yes` planned with no cards; it never
        // wrote, so it does not become a mode that does.
        None if plan => Ok(Mode::Plan),
        None if yes => Ok(Mode::Bypass),
        None => Ok(Mode::Default),
    }
}

#[cfg(test)]
mod tests {
    use super::{Input, Output, parse};
    use letronna_core::gate::Mode;

    fn args(words: &[&str]) -> super::Args {
        parse(words.iter().map(|w| w.to_string())).unwrap()
    }

    #[test]
    fn reads_the_forms_people_actually_type() {
        let bare = args(&[]);
        assert!(bare.prompt.is_none() && bare.start == super::Start::Fresh);
        assert_eq!(bare.mode, Ok(Mode::Default));

        assert_eq!(args(&["-p", "hello"]).prompt.as_deref(), Some("hello"));
        assert_eq!(args(&["--print", "hello"]).prompt.as_deref(), Some("hello"));
        // A bare word is a prompt, so quoting alone is enough.
        assert_eq!(
            args(&["what is this"]).prompt.as_deref(),
            Some("what is this")
        );
        // A flag after it must not be swallowed as the prompt.
        let mixed = args(&["--plan", "look around"]);
        assert_eq!(mixed.mode, Ok(Mode::Plan));
        assert_eq!(mixed.prompt.as_deref(), Some("look around"));

        assert_eq!(args(&["-c"]).start, super::Start::Last);
        assert_eq!(args(&["--continue"]).start, super::Start::Last);
        // Resume on its own asks which one; with an id it takes that one.
        assert_eq!(args(&["--resume"]).start, super::Start::Choose);
        assert_eq!(args(&["--resume", "137"]).start, super::Start::Named(137));
        assert!(args(&["gui"]).gui);
        assert!(args(&["serve"]).serve && args(&["serve"]).prompt.is_none());
        assert!(args(&["update"]).update && args(&["update"]).prompt.is_none());
        assert_eq!(args(&["--yes"]).mode, Ok(Mode::Bypass));
        let edits = args(&["--mode", "accept-edits", "tidy up"]);
        assert_eq!(edits.mode, Ok(Mode::AcceptEdits));
        assert_eq!(edits.prompt.as_deref(), Some("tidy up"));
        assert_eq!(args(&["--mode", "auto"]).mode, Ok(Mode::Auto));
        assert!(args(&["--mode"]).mode.is_err());
        assert!(args(&["-h"]).help);
        assert!(args(&["--version"]).version && args(&["-V"]).version);
        // `mcp` first hands every word after it on, flags and all.
        let mcp = args(&["mcp", "add", "fs", "-e", "K=V", "--", "npx", "-y"]);
        assert_eq!(
            mcp.mcp.unwrap(),
            ["add", "fs", "-e", "K=V", "--", "npx", "-y"]
        );
        assert_eq!(args(&["-p", "mcp"]).prompt.as_deref(), Some("mcp"));
        assert!(args(&["acp"]).acp && args(&["acp"]).prompt.is_none());
        assert_eq!(args(&["-p", "acp"]).prompt.as_deref(), Some("acp"));
    }

    #[test]
    fn plan_with_yes_stays_plan_and_mode_with_a_shorthand_says_which_to_drop() {
        assert_eq!(args(&["--plan", "--yes"]).mode, Ok(Mode::Plan));
        assert_eq!(args(&["--yes", "--plan", "look"]).mode, Ok(Mode::Plan));
        let yes = args(&["--mode", "plan", "--yes"]).mode.unwrap_err();
        assert!(yes.contains("drop `--yes`"), "{yes}");
        let plan = args(&["--plan", "--mode", "bypass"]).mode.unwrap_err();
        assert!(plan.contains("drop `--plan`"), "{plan}");
    }

    /// What parsing `words` refused them with.
    fn refused(words: &[&str]) -> String {
        match parse(words.iter().map(|w| w.to_string())) {
            Ok(_) => panic!("{words:?} parsed"),
            Err(error) => error,
        }
    }

    #[test]
    fn the_new_flags_parse() {
        let run = args(&[
            "-p",
            "tidy up",
            "--output",
            "stream-json",
            "--max-rounds",
            "40",
            "--model",
            "local-model",
            "--tools",
            "read_file, search,,list_dir",
            "--budget",
            "200000",
        ]);
        assert_eq!(run.prompt.as_deref(), Some("tidy up"));
        assert_eq!(run.output, Output::StreamJson);
        assert_eq!(run.max_rounds, Some(40));
        assert_eq!(run.model.as_deref(), Some("local-model"));
        assert_eq!(
            run.tools,
            Some(vec!["read_file".into(), "search".into(), "list_dir".into()])
        );
        assert_eq!(run.budget, Some(200_000));
        // Before the prompt counts too, and none of them is taken for it.
        let json = args(&["--output", "json", "what is this"]);
        assert_eq!(json.output, Output::Json);
        assert_eq!(json.prompt.as_deref(), Some("what is this"));
        // So does a flag between -p and the question.
        let between = args(&["-p", "--output", "json", "what is this"]);
        assert_eq!(between.output, Output::Json);
        assert_eq!(between.prompt.as_deref(), Some("what is this"));
        let plain = args(&["-p", "hello"]);
        assert_eq!(plain.output, Output::Text);
        assert!(plain.max_rounds.is_none() && plain.tools.is_none() && plain.budget.is_none());

        assert_eq!(
            refused(&["-p", "x", "--output", "yaml"]),
            "--output takes text, json or stream-json."
        );
        assert_eq!(
            refused(&["-p", "x", "--max-rounds", "0"]),
            "--max-rounds takes a number of tool rounds above 0, like --max-rounds 10."
        );
        assert!(refused(&["-p", "x", "--max-rounds", "many"]).starts_with("--max-rounds takes"));
        assert!(refused(&["-p", "x", "--budget", "-5"]).starts_with("--budget takes"));
        // A flag where the value should be is a missing value, not a name.
        assert_eq!(
            refused(&["-p", "x", "--model", "--yes"]),
            "--model takes the name of a model."
        );
        assert!(refused(&["-p", "x", "--tools", ","]).starts_with("--tools takes"));
        assert!(refused(&["-p", "x", "--budget"]).starts_with("--budget takes"));
        assert!(refused(&["-p", "--output", "json"]).starts_with("-p takes the question"));
        assert_eq!(
            refused(&["--budget", "1000"]),
            "--output, --max-rounds, --tools and --budget only work with -p."
        );
    }

    #[test]
    fn a_resume_word_that_is_not_an_id_is_an_error() {
        let no_id = "--resume takes a session id, like --resume 42.";
        assert_eq!(refused(&["--resume", "hello"]), no_id);
        assert_eq!(refused(&["-p", "go on", "-r", "last"]), no_id);
        // A flag after it is the next flag, and the list is offered.
        let flagged = args(&["--resume", "--plan"]);
        assert_eq!(flagged.start, super::Start::Choose);
        assert_eq!(flagged.mode, Ok(Mode::Plan));
        // Nobody sees the list under -p, so there it needs the id.
        let listed = "With -p, --resume takes a session id, like --resume 42.";
        assert_eq!(refused(&["-p", "go on", "--resume"]), listed);
        assert_eq!(refused(&["go on", "-r"]), listed);
    }

    #[test]
    fn continue_after_print_is_not_taken_as_the_question() {
        let continued = args(&["-p", "-c", "what changed"]);
        assert_eq!(continued.start, super::Start::Last);
        assert_eq!(continued.prompt.as_deref(), Some("what changed"));
        let named = args(&["-p", "-r", "42", "go on"]);
        assert_eq!(named.start, super::Start::Named(42));
        assert_eq!(named.prompt.as_deref(), Some("go on"));
        // A question of more than one word that starts with a dash is still one.
        let dashed = args(&["-p", "-5 or 5?"]);
        assert_eq!(dashed.prompt.as_deref(), Some("-5 or 5?"));
    }

    #[test]
    fn stream_json_in_takes_no_question_and_needs_stream_json_out() {
        let run = args(&[
            "-p",
            "--input-format",
            "stream-json",
            "--output",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
        ]);
        assert_eq!(run.input, Input::StreamJson);
        assert!(run.ask_stdio && run.prompt.is_none() && run.once());
        assert!(!args(&[]).once() && args(&["-p", "hi"]).once());

        assert_eq!(
            refused(&["-p", "--input-format", "yaml"]),
            "--input-format takes text or stream-json."
        );
        assert!(
            refused(&["--input-format", "stream-json", "--output", "stream-json"])
                .ends_with("only works with -p.")
        );
        let asked = [
            "-p",
            "hi",
            "--input-format",
            "stream-json",
            "--output",
            "stream-json",
        ];
        assert!(refused(&asked).ends_with("so -p takes no question."));
        assert_eq!(
            refused(&["-p", "--input-format", "stream-json"]),
            "--input-format stream-json needs --output stream-json."
        );
        assert!(
            refused(&["-p", "hi", "--permission-prompt-tool", "stdio"])
                .starts_with("--permission-prompt-tool stdio needs")
        );
        assert_eq!(
            refused(&["-p", "hi", "--permission-prompt-tool", "mcp__ask"]),
            "--permission-prompt-tool takes stdio."
        );
    }
}
