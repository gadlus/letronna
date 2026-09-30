//! `/init`: the model studies the project and writes its AGENTS.md.
//!
//! It is one prompt sent as an ordinary turn rather than a tool of its own,
//! so the model reads the project with the tools it always has and the file
//! is written through the same approval card as any other write. Both front
//! ends send [`PROMPT`] for the command, and show the command rather than the
//! prompt as what the user said, the way they would have typed it.

use std::path::Path;

/// The command as it is typed.
pub const COMMAND: &str = "/init";

/// What `/init` asks of the model. It keeps an existing file as it is because
/// people write these files, and a rewrite would lose what they meant.
pub const PROMPT: &str = "\
Study this project and write its AGENTS.md: the instructions every future \
session in this folder reads before it starts. Look before you write -- the \
README, the build and package manifests, the CI workflows, the folder layout \
and a few representative source files -- and find out:

- how to build, run, lint, format and test it, including how to run a single test;
- how the code is laid out: the main parts, what each is for, and where to start reading;
- the conventions the code follows that no single file shows: naming, error handling, style, how commits are written;
- the gotchas: required setup, traps, and what must never be done.

Write only what you saw in the project. Never invent a command, script or path; \
leave out what you could not find rather than guess. Keep it short -- what a \
capable newcomer would get wrong without being told, not a tour of every file \
or advice that fits any project. Leave out secrets, keys and anything personal: \
the file is usually committed.

If AGENTS.md already exists at the project root, keep all of it as it is and \
only add what is missing; if something in it looks out of date, say so in your \
reply instead of changing it. If there is a CLAUDE.md, .cursorrules, \
.cursor/rules or .github/copilot-instructions.md, carry over what in it still \
holds.

Write the file as AGENTS.md at the root of the project folder and change no \
other file. Then say in a few lines what you wrote or added.";

/// The prompt `/init` sends in a session with `workspace` attached, or why it
/// sends nothing.
pub fn prompt(workspace: Option<&Path>) -> Result<&'static str, String> {
    match workspace {
        Some(_) => Ok(PROMPT),
        None => Err(
            "/init needs a project folder to write AGENTS.md in, and none is attached to this \
             session."
                .into(),
        ),
    }
}

/// What `line` sends when it is `/init`, as [`prompt`] says; `None` for any
/// other line.
pub fn command(line: &str, workspace: Option<&Path>) -> Option<Result<&'static str, String>> {
    (line.trim() == COMMAND).then(|| prompt(workspace))
}

/// What the user typed for `prompt`: `/init` for the prompt that command
/// sends, so a front end can show that while the model keeps the prompt.
pub fn typed(prompt: &str) -> Option<&'static str> {
    (prompt == PROMPT).then_some(COMMAND)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_maps_to_the_prompt() {
        let folder = Some(Path::new("project"));
        assert_eq!(command("/init", folder), Some(Ok(PROMPT)));
        assert_eq!(command("  /init \n", folder), Some(Ok(PROMPT)));
        assert_eq!(command("/init now", folder), None);
        assert_eq!(command("/initialize", folder), None);
        assert_eq!(command("init the project", folder), None);
    }

    #[test]
    fn with_no_project_folder_nothing_is_sent() {
        let Some(Err(why)) = command("/init", None) else {
            panic!("/init with no folder should be refused");
        };
        assert!(why.contains("project folder"), "{why}");
        assert_eq!(command("hello", None), None);
    }

    #[test]
    fn the_prompt_shows_as_the_command() {
        assert_eq!(typed(PROMPT), Some("/init"));
        assert_eq!(typed("/init"), None);
        assert_eq!(typed(&format!("{PROMPT} ")), None);
        assert_eq!(typed("write AGENTS.md"), None);
    }
}
