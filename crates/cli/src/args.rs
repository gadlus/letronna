//! What `letronna` was asked for on the command line.

use letronna_core::gate::Mode;

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
  letronna doctor          check the setup, the disk and the gateway
  letronna --uninstall     take it off again

Options
  -p, --print <prompt>     ask one question, print the answer, exit
  -c, --continue           resume the most recent session
  -r, --resume [id]        choose a session, or open the one with this id
      --mode <mode>        how much runs without asking: default, accept-edits,
                           plan, dont-ask or bypass
      --yes                --mode bypass: run tools without asking, bar risky ones
      --plan               --mode plan: read and search only; never write or run
      --offline            with doctor: leave the network alone
  -h, --help               this
  -V, --version            which version this is
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
    /// The mode from `--mode`, `--yes` or `--plan`, or why `--mode` named
    /// none.
    pub mode: Result<Mode, String>,
    pub help: bool,
    pub version: bool,
    pub gui: bool,
    pub install: bool,
    pub uninstall: bool,
    pub update: bool,
    pub doctor: bool,
    pub offline: bool,
}

pub fn parse(mut raw: impl Iterator<Item = String>) -> Args {
    let (mut mode, mut yes, mut plan) = (None, false, false);
    let mut args = Args {
        prompt: None,
        start: Start::Fresh,
        mode: Ok(Mode::Default),
        help: false,
        version: false,
        gui: false,
        install: false,
        uninstall: false,
        update: false,
        doctor: false,
        offline: false,
    };
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "gui" => args.gui = true,
            "update" => args.update = true,
            "doctor" => args.doctor = true,
            "--offline" => args.offline = true,
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
            "--mode" => mode = Some(Mode::parse(&raw.next().unwrap_or_default())),
            "--yes" => yes = true,
            "--plan" => plan = true,
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
    args
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
    use super::parse;
    use letronna_core::gate::Mode;

    fn args(words: &[&str]) -> super::Args {
        parse(words.iter().map(|w| w.to_string()))
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
        // A word that is not a number is a prompt, not a session.
        let worded = args(&["--resume", "hello"]);
        assert_eq!(worded.start, super::Start::Choose);
        assert!(args(&["gui"]).gui);
        assert!(args(&["update"]).update && args(&["update"]).prompt.is_none());
        assert_eq!(args(&["--yes"]).mode, Ok(Mode::Bypass));
        let edits = args(&["--mode", "accept-edits", "tidy up"]);
        assert_eq!(edits.mode, Ok(Mode::AcceptEdits));
        assert_eq!(edits.prompt.as_deref(), Some("tidy up"));
        assert!(args(&["--mode", "auto"]).mode.is_err());
        assert!(args(&["--mode"]).mode.is_err());
        assert!(args(&["-h"]).help);
        assert!(args(&["--version"]).version && args(&["-V"]).version);
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
}
