//! What `letronna` was asked for on the command line.

pub const HELP: &str = "\
Letronna — a small agent in your terminal

  letronna                 open a conversation
  letronna -p \"question\"    ask once and exit
  letronna --continue      pick up the most recent session
  letronna --resume        choose from the recent sessions
  letronna --resume <id>   open that session, by the id /where shows
  letronna gui             open the window instead
  letronna setup           name, sign in, provider, model, install
  letronna update          install the newest version
  letronna --uninstall     take it off again

Options
  -p, --print <prompt>     ask one question, print the answer, exit
  -c, --continue           resume the most recent session
  -r, --resume [id]        choose a session, or open the one with this id
      --yes                run tools without asking (careful)
      --plan               read and search only; never write or run
  -h, --help               this
  -V, --version            which version this is

With -p
      --output <form>      text (the answer), json, or stream-json
      --max-rounds <n>     tool rounds before it stops (25)
      --model <name>       use this model instead of the one set up
      --tools <a,b>        only these tools, by name
      --budget <tokens>    stop once this many tokens are spent
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

pub struct Args {
    pub prompt: Option<String>,
    pub start: Start,
    pub yes: bool,
    pub plan: bool,
    pub help: bool,
    pub version: bool,
    pub gui: bool,
    pub install: bool,
    pub uninstall: bool,
    pub update: bool,
    pub output: Output,
    pub max_rounds: Option<usize>,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub budget: Option<u64>,
}

pub fn parse(raw: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut raw = raw.peekable();
    let mut print = false;
    let mut args = Args {
        prompt: None,
        start: Start::Fresh,
        yes: false,
        plan: false,
        help: false,
        version: false,
        gui: false,
        install: false,
        uninstall: false,
        update: false,
        output: Output::Text,
        max_rounds: None,
        model: None,
        tools: None,
        budget: None,
    };
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "gui" => args.gui = true,
            "update" => args.update = true,
            // A flag straight after is not the question, so `-p --output
            // json "what is this"` takes the bare word after it.
            "-p" | "--print" => {
                print = true;
                if raw.peek().is_some_and(|next| !next.starts_with("--")) {
                    args.prompt = raw.next();
                }
            }
            "-c" | "--continue" => args.start = Start::Last,
            // `--resume` on its own offers the recent sessions; with an id it
            // opens that one, which is what the id in `/where` is for.
            "-r" | "--resume" => {
                args.start = match raw.next().as_deref().map(str::parse::<u64>) {
                    Some(Ok(id)) => Start::Named(id),
                    _ => Start::Choose,
                }
            }
            "--yes" => args.yes = true,
            "--plan" => args.plan = true,
            "--output" => {
                args.output = match raw.next().as_deref() {
                    Some("text") => Output::Text,
                    Some("json") => Output::Json,
                    Some("stream-json") => Output::StreamJson,
                    _ => return Err("--output takes text, json or stream-json.".into()),
                }
            }
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
    if print && args.prompt.is_none() {
        return Err("-p takes the question, like -p \"what is this\".".into());
    }
    let shaped = args.output != Output::Text
        || args.max_rounds.is_some()
        || args.model.is_some()
        || args.tools.is_some()
        || args.budget.is_some();
    if shaped && args.prompt.is_none() {
        return Err(
            "--output, --max-rounds, --model, --tools and --budget only work with -p.".into(),
        );
    }
    Ok(args)
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

#[cfg(test)]
mod tests {
    use super::{Output, parse};

    fn args(words: &[&str]) -> super::Args {
        parse(words.iter().map(|w| w.to_string())).unwrap()
    }

    /// What parsing `words` refused them with.
    fn refused(words: &[&str]) -> String {
        match parse(words.iter().map(|w| w.to_string())) {
            Ok(_) => panic!("{words:?} parsed"),
            Err(error) => error,
        }
    }

    #[test]
    fn reads_the_forms_people_actually_type() {
        let bare = args(&[]);
        assert!(bare.prompt.is_none() && bare.start == super::Start::Fresh && !bare.yes);

        assert_eq!(args(&["-p", "hello"]).prompt.as_deref(), Some("hello"));
        assert_eq!(args(&["--print", "hello"]).prompt.as_deref(), Some("hello"));
        // A bare word is a prompt, so quoting alone is enough.
        assert_eq!(
            args(&["what is this"]).prompt.as_deref(),
            Some("what is this")
        );
        // A flag after it must not be swallowed as the prompt.
        let mixed = args(&["--plan", "look around"]);
        assert!(mixed.plan);
        assert_eq!(mixed.prompt.as_deref(), Some("look around"));

        assert_eq!(args(&["-c"]).start, super::Start::Last);
        assert_eq!(args(&["--continue"]).start, super::Start::Last);
        // Resume on its own asks which one; with an id it takes that one.
        assert_eq!(args(&["--resume"]).start, super::Start::Choose);
        assert_eq!(args(&["--resume", "137"]).start, super::Start::Named(137));
        // A word that is not a number is a prompt, not a session.
        let worded = args(&["--resume", "hello"]);
        assert_eq!(worded.start, super::Start::Choose);
        assert!(args(&["gui"]).gui);
        assert!(args(&["update"]).update && args(&["update"]).prompt.is_none());
        assert!(args(&["--yes"]).yes);
        assert!(args(&["-h"]).help);
        assert!(args(&["--version"]).version && args(&["-V"]).version);
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
            "--output, --max-rounds, --model, --tools and --budget only work with -p."
        );
    }
}
