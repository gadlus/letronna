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

pub struct Args {
    pub prompt: Option<String>,
    pub start: Start,
    pub yes: bool,
    pub plan: bool,
    pub help: bool,
    pub gui: bool,
    pub install: bool,
    pub uninstall: bool,
    pub update: bool,
}

pub fn parse(mut raw: impl Iterator<Item = String>) -> Args {
    let mut args = Args {
        prompt: None,
        start: Start::Fresh,
        yes: false,
        plan: false,
        help: false,
        gui: false,
        install: false,
        uninstall: false,
        update: false,
    };
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "gui" => args.gui = true,
            "update" => args.update = true,
            "-p" | "--print" => args.prompt = raw.next(),
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
            "setup" | "--install" => args.install = true,
            "--uninstall" => args.uninstall = true,
            "-h" | "--help" => args.help = true,
            // A bare word is the prompt, so `letronna "what is this"` works.
            other if !other.starts_with('-') && args.prompt.is_none() => {
                args.prompt = Some(other.to_owned())
            }
            _ => {}
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn args(words: &[&str]) -> super::Args {
        parse(words.iter().map(|w| w.to_string()))
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
    }
}
