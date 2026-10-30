//! Letronna Cloud: tasks run on our servers instead of this computer, for
//! when it is off or cannot run them. A task goes to cloud.letronna.ai with
//! the signed-in account's token, runs as `letronna -p` in a container there,
//! and its answer comes back here. A thread carries a task on: its folder and
//! session wait on the server between messages.
//!
//! `LETRONNA_CLOUD` points it elsewhere, such as `http://localhost:8080`.

use serde::{Deserialize, Serialize};
use std::time::Duration;

const CLOUD: &str = "https://cloud.letronna.ai";

fn url(path: &str) -> String {
    let base = std::env::var("LETRONNA_CLOUD").unwrap_or_else(|_| CLOUD.to_owned());
    format!("{}{path}", base.trim_end_matches('/'))
}

/// The memory a run may use, and so how much it costs per second.
pub const TIERS: [&str; 3] = ["light", "normal", "heavy"];

/// A task to start.
#[derive(Debug, Default, Serialize)]
pub struct Task {
    pub prompt: String,
    /// An https:// git repository the run works in, cloned first.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub branch: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tier: String,
    /// The model to run on: the session's, which the cloud swaps for its
    /// default when empty or not one it hosts.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// A thread to carry on, from an earlier run.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub thread: String,
    /// The project folder on this computer, packed by `pack` as a gzipped
    /// tar in base64, unpacked into the run's folder in place of a clone.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub files: String,
}

/// The most a packed folder may come to, gzipped: what the cloud takes.
pub const PACK_MAX: usize = 25 * 1024 * 1024;

/// `folder` as a gzipped tar in base64, for `Task::files`: every file the
/// project's .gitignore keeps, hidden ones such as .env included, but never
/// .git, so a private or local-only repository reaches the cloud without
/// anyone's git sign-in.
pub fn pack(folder: &std::path::Path) -> Result<String, String> {
    use base64::Engine;
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.follow_symlinks(false);
    let walk = ignore::WalkBuilder::new(folder)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walk {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let Ok(inside) = path.strip_prefix(folder) else {
            continue;
        };
        if inside.as_os_str().is_empty() || !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        tar.append_path_with_name(path, inside)
            .map_err(|e| format!("Could not pack {}: {e}", inside.display()))?;
        if tar.get_ref().get_ref().len() > PACK_MAX {
            return Err(too_big());
        }
    }
    let packed = tar
        .into_inner()
        .and_then(|gz| gz.finish())
        .map_err(|e| e.to_string())?;
    if packed.len() > PACK_MAX {
        return Err(too_big());
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(packed))
}

fn too_big() -> String {
    format!(
        "The project folder is over {} MB packed; push it to a repository the cloud can clone, or leave large files out with .gitignore.",
        PACK_MAX / 1024 / 1024
    )
}

/// A run as the cloud tells it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Run {
    pub id: String,
    pub thread: String,
    pub tier: String,
    /// queued, running, done, failed or stopped.
    pub status: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub seconds: i64,
    #[serde(default)]
    pub created_at: i64,
    /// Where what it made can be seen, once it is done and made a page.
    #[serde(default)]
    pub preview: Option<String>,
}

impl Run {
    /// Whether it has ended, one way or another.
    pub fn over(&self) -> bool {
        matches!(self.status.as_str(), "done" | "failed" | "stopped")
    }
}

fn token() -> Result<String, String> {
    crate::auth::account()
        .map(|account| account.token)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Sign in first: run letronna login.".to_owned())
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

/// The cloud's own words for a refusal, or the status when it gave none.
fn refused(res: reqwest::blocking::Response) -> String {
    let status = res.status();
    let body: serde_json::Value = res.json().unwrap_or_default();
    body["error"]
        .as_str()
        .map_or_else(|| format!("The cloud answered {status}."), str::to_owned)
}

fn send(request: reqwest::blocking::RequestBuilder) -> Result<reqwest::blocking::Response, String> {
    let res = request
        .bearer_auth(token()?)
        .send()
        .map_err(|e| format!("The cloud could not be reached: {e}"))?;
    if res.status().is_success() {
        Ok(res)
    } else {
        Err(refused(res))
    }
}

/// What a run says as it goes, as `follow` hears it.
#[derive(Clone, Debug, PartialEq)]
pub enum Heard {
    /// queued, running, done, failed or stopped.
    Status(String),
    /// The tool steps of the round under way, and rounds so far.
    Doing { rounds: u64, doing: String },
}

/// Follow run `id` to its end, telling `heard` what it says on the way, and
/// give it back as it ended. Its events come over SSE as they happen; if
/// that stream breaks, it is asked every two seconds instead. `stopped` is
/// looked at between events, at least every ten seconds, and stops the run.
pub fn follow(
    id: &str,
    heard: &mut dyn FnMut(Heard),
    stopped: &dyn Fn() -> bool,
) -> Result<Run, String> {
    if let Ok(run) = follow_events(id, heard, stopped) {
        return Ok(run);
    }
    loop {
        if stopped() {
            let _ = stop(id);
            return Err("Stopped; the cloud run was stopped too.".into());
        }
        let now = run(id)?;
        if now.over() {
            return Ok(now);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// One SSE event's data, as `Heard`; Ok(true) once it says the run ended.
fn heard_of(data: &str) -> Option<(Heard, bool)> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    match v["type"].as_str()? {
        "status" => Some((Heard::Status(v["status"].as_str()?.to_owned()), false)),
        "end" => Some((
            Heard::Status(v["status"].as_str().unwrap_or("done").to_owned()),
            true,
        )),
        "progress" => Some((
            Heard::Doing {
                rounds: v["rounds"].as_u64().unwrap_or(0),
                doing: v["doing"].as_str().unwrap_or_default().to_owned(),
            },
            false,
        )),
        _ => None,
    }
}

fn follow_events(
    id: &str,
    heard: &mut dyn FnMut(Heard),
    stopped: &dyn Fn() -> bool,
) -> Result<Run, String> {
    use std::io::BufRead;
    // No limit on the whole answer, which lasts as long as the run.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(None)
        .build()
        .map_err(|e| e.to_string())?;
    let res = send(client.get(url(&format!("/v1/runs/{id}/events"))))?;
    for line in std::io::BufReader::new(res).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if stopped() {
            let _ = stop(id);
            return Err("Stopped; the cloud run was stopped too.".into());
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        if let Some((said, end)) = heard_of(data.trim()) {
            heard(said);
            if end {
                return run(id);
            }
        }
    }
    Err("The run's events ended early.".into())
}

/// Start `task`; it waits in the queue until there is room for it.
pub fn start(task: &Task) -> Result<Run, String> {
    send(client()?.post(url("/v1/runs")).json(task))?
        .json()
        .map_err(|e| e.to_string())
}

/// One run, as it stands now.
pub fn run(id: &str) -> Result<Run, String> {
    send(client()?.get(url(&format!("/v1/runs/{id}"))))?
        .json()
        .map_err(|e| e.to_string())
}

/// The account's recent runs, newest first.
pub fn runs(limit: u32) -> Result<Vec<Run>, String> {
    #[derive(Deserialize)]
    struct Listed {
        runs: Vec<Run>,
    }
    send(client()?.get(url(&format!("/v1/runs?limit={limit}"))))?
        .json::<Listed>()
        .map(|listed| listed.runs)
        .map_err(|e| e.to_string())
}

/// Let Letronna's support read a run's task and answer, or take that back.
pub fn support(id: &str, allow: bool) -> Result<(), String> {
    send(
        client()?
            .post(url(&format!("/v1/runs/{id}/support")))
            .json(&serde_json::json!({ "allow": allow })),
    )
    .map(drop)
}

/// Have Letronna Cloud answer the paired Telegram bot while this computer is
/// off, or stop; the setting is kept with the chat apps' own.
pub fn relay(on: bool) -> Result<(), String> {
    let mut config = crate::messaging::load();
    if on {
        let telegram = &config.telegram;
        let chat = telegram
            .chat
            .filter(|_| !telegram.token.trim().is_empty())
            .ok_or("Set up Telegram and pair a chat first, in Messaging.")?;
        send(
            client()?
                .post(url("/v1/relay/telegram"))
                .json(&serde_json::json!({
                    "token": telegram.token.trim(),
                    "chat": chat,
                    "owner": telegram.owner.unwrap_or(0),
                })),
        )?;
    } else {
        send(client()?.post(url("/v1/relay/off")))?;
    }
    config.telegram.cloud = on;
    crate::messaging::save(&config)
}

/// Tell Letronna Cloud this computer is up and listening to its chat apps.
pub fn heartbeat() -> Result<(), String> {
    send(client()?.post(url("/v1/relay/heartbeat"))).map(drop)
}

/// An app or worker a cloud run left running.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct App {
    pub thread: String,
    /// app (wakes on a visit) or worker (stays up).
    pub kind: String,
    /// running, asleep, starting or stopped.
    pub state: String,
    pub mb: u32,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub failure: String,
}

/// The account's apps and workers.
pub fn apps() -> Result<Vec<App>, String> {
    #[derive(Deserialize)]
    struct Listed {
        apps: Vec<App>,
    }
    send(client()?.get(url("/v1/apps")))?
        .json::<Listed>()
        .map(|listed| listed.apps)
        .map_err(|e| e.to_string())
}

/// What the app or worker on `thread` printed last, and why it stopped if
/// it did.
pub fn app_logs(thread: &str) -> Result<(String, String), String> {
    let v: serde_json::Value = send(client()?.get(url(&format!("/v1/apps/{thread}/logs"))))?
        .json()
        .map_err(|e| e.to_string())?;
    Ok((
        v["logs"].as_str().unwrap_or_default().to_owned(),
        v["failure"].as_str().unwrap_or_default().to_owned(),
    ))
}

/// Take the app or worker on `thread` down, or (`up`) bring a worker back.
pub fn app_power(thread: &str, up: bool) -> Result<(), String> {
    let path = format!("/v1/apps/{thread}/{}", if up { "up" } else { "down" });
    send(client()?.post(url(&path))).map(drop)
}

/// Stop a run, queued or running.
pub fn stop(id: &str) -> Result<(), String> {
    send(client()?.post(url(&format!("/v1/runs/{id}/stop")))).map(drop)
}

/// Whether this process is itself a cloud run, which must not start another.
fn inside_a_run() -> bool {
    std::env::var_os("LETRONNA_DISPOSABLE").is_some_and(|v| v == "1")
}

/// `cloud_task`: the model's way to run a task in the cloud, chosen after an
/// artifact and this computer, as its description says.
pub struct CloudTask;

impl crate::tools::Tool for CloudTask {
    fn name(&self) -> &'static str {
        "cloud_task"
    }
    fn description(&self) -> &'static str {
        "Run a task in Letronna Cloud, on Letronna's servers, and get its answer. Where to build what the user asks for, in order: a page, chart, document or small app that one self-contained file can hold is written here as an .html or .svg file the user previews; anything else is built here, in the project, with the tools on this computer; only when this computer cannot (a tool it needs is not on PATH and cannot be installed, it would take more memory or time than this machine has, or the user asks for the cloud) send it here. It also hosts: a web app it leaves wakes on each visit, and a bot or worker the user wants kept online (a Discord or Telegram bot listening for events, a scheduler) runs 24/7 from their credit; say so in `task` only when the user asked for it to stay up, and put any keys it needs in `task` for /work/.env. cloud_apps shows what is left running and cloud_app_power takes it down or back up. The cloud cannot see this computer's files: pass folder: true to send it the project folder (what .gitignore keeps, without .git), which works for a local-only repository; or an https `repo` it clones. A https://github.com/<owner>/<name> repo the user connected to the Letronna GitHub App may be private: the task works on its own branch, letronna/<thread>, and what it commits is pushed there and opened as a pull request, never pushed to another branch. Any other repo must be public, and nothing is pushed. Say in your reply that it ran in the cloud, and give the branch and pull request it reports. It costs the user's credit, so each call is asked about."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "required": ["task"], "properties": {
            "task": {"type": "string", "description": "What to do, whole: the cloud sees nothing else of this conversation."},
            "folder": {"type": "boolean", "description": "Send this session's project folder to work in, instead of a repo."},
            "repo": {"type": "string", "description": "An https git address to clone and work in: public, or a GitHub repo connected to the Letronna GitHub App, whose commits are pushed to a branch of its own with a pull request."},
            "branch": {"type": "string"},
            "tier": {"type": "string", "enum": TIERS, "description": "Memory: light 512 MB (default), normal 1 GB, heavy 2 GB for builds."},
            "thread": {"type": "string", "description": "An earlier cloud run's thread, to carry on in its folder and conversation."}
        }})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        true
    }
    fn open_world(&self) -> bool {
        true
    }
    /// It runs on Letronna's servers, never this computer.
    fn machine(&self) -> bool {
        false
    }
    fn untrusted(&self) -> bool {
        true
    }
    fn offered(&self, _: &crate::chat::Connection) -> bool {
        !inside_a_run() && crate::auth::signed_in()
    }
    fn preview(&self, args: &serde_json::Value, _: &crate::tools::ToolContext) -> String {
        let field = |name: &str| args[name].as_str().unwrap_or_default().trim().to_owned();
        let mut card = format!(
            "Run in Letronna Cloud ({}):\n{}",
            or_light(&field("tier")),
            field("task")
        );
        if !field("repo").is_empty() {
            card.push_str(&format!("\nin {}", field("repo")));
        }
        if args["folder"].as_bool() == Some(true) {
            card.push_str("\nwith this project folder");
        }
        card
    }
    fn run(
        &self,
        args: &serde_json::Value,
        ctx: &crate::tools::ToolContext,
    ) -> Result<String, String> {
        let field = |name: &str| args[name].as_str().unwrap_or_default().trim().to_owned();
        let task = Task {
            prompt: field("task"),
            repo: field("repo"),
            branch: field("branch"),
            tier: field("tier"),
            thread: field("thread"),
            // The session's own model, which the cloud swaps for its
            // default when it hosts no model by that name.
            model: (ctx.caller.connection.as_deref())
                .map(|c| c.model.trim().to_owned())
                .unwrap_or_default(),
            files: String::new(),
        };
        if task.prompt.is_empty() {
            return Err("Say what to do in `task`.".into());
        }
        let mut task = task;
        if args["folder"].as_bool() == Some(true) {
            if !task.repo.is_empty() {
                return Err("Pass folder or repo, not both.".into());
            }
            let folder = match &ctx.workspace {
                Some(folder) if !ctx.fallback => folder,
                _ => return Err("This session has no project folder to send.".into()),
            };
            task.files = pack(folder)?;
        }
        let started = start(&task)?;
        // What the run does, shown under the call while it goes, and kept as
        // its last steps for the result.
        let here = ctx.conversation;
        let mut steps: Vec<String> = Vec::new();
        crate::tool_progress::say(
            here,
            format!(
                "Cloud run {}: {} · {}",
                started.id,
                started.status,
                run_page(&started.id)
            ),
        );
        let followed = follow(
            &started.id,
            &mut |heard| {
                let line = progress_line(&started.id, heard, &mut steps);
                crate::tool_progress::say(here, line);
            },
            &|| ctx.cancel.is_set(),
        );
        crate::tool_progress::clear(here);
        let run = followed?;
        let mut said = format!(
            "Ran in Letronna Cloud as {} ({}, {} s); to carry on there, pass thread {}.",
            run.id, run.tier, run.seconds, run.thread
        );
        said.push_str(&format!(
            " The user can follow and reread it at {}; give them that link.",
            run_page(&run.id)
        ));
        if let Some(preview) = &run.preview {
            said.push_str(&format!(
                " What it made can be seen at {preview}; give the user that link."
            ));
        }
        // The last steps it took, so a failure says how far it got.
        const STEPS: usize = 8;
        if !steps.is_empty() {
            let from = steps.len().saturating_sub(STEPS);
            said.push_str(&format!(" Its last steps: {}.", steps[from..].join("; ")));
        }
        match run.status.as_str() {
            "done" => Ok(format!("{said}\n\n{}", run.answer.trim())),
            "stopped" => Err(format!("{said} It was stopped.")),
            _ => Err(format!("{said} It failed: {}", run.error.trim())),
        }
    }
}

/// The website's page for run `id`, where its owner watches it and reads
/// it back later.
fn run_page(id: &str) -> String {
    format!("{}/cloud/run/{id}", crate::auth::dashboard())
}

/// The line shown under a cloud call for what run `id` says now, noting in
/// `steps` each new set of tools it turns to.
fn progress_line(id: &str, heard: Heard, steps: &mut Vec<String>) -> String {
    let now = match heard {
        Heard::Status(status) => status,
        Heard::Doing { rounds, doing } if doing.is_empty() => format!("round {rounds}"),
        Heard::Doing { rounds, doing } => {
            if steps.last() != Some(&doing) {
                steps.push(doing.clone());
            }
            format!("round {rounds}: {doing}")
        }
    };
    format!("Cloud run {id}: {now}")
}

/// `cloud_apps`: what the user's cloud runs left running, and what one
/// printed. Reading only, so never asked about.
pub struct CloudApps;

impl crate::tools::Tool for CloudApps {
    fn name(&self) -> &'static str {
        "cloud_apps"
    }
    fn description(&self) -> &'static str {
        "The apps and 24/7 workers (bots and the like) the user's Letronna Cloud runs left running. With no `thread`, lists each with its thread, state and address; with one, shows what it printed last and why it stopped. Use it when the user asks about or checks on something hosted in the cloud; cloud_app_power takes one down or brings it back."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {
            "thread": {"type": "string", "description": "One app's or worker's thread, to read what it printed."}
        }})
    }
    fn open_world(&self) -> bool {
        true
    }
    fn machine(&self) -> bool {
        false
    }
    fn untrusted(&self) -> bool {
        true
    }
    fn offered(&self, _: &crate::chat::Connection) -> bool {
        !inside_a_run() && crate::auth::signed_in()
    }
    fn run(
        &self,
        args: &serde_json::Value,
        _: &crate::tools::ToolContext,
    ) -> Result<String, String> {
        let thread = args["thread"].as_str().unwrap_or_default().trim();
        if !thread.is_empty() {
            let (logs, failure) = app_logs(thread)?;
            let logs = if logs.trim().is_empty() {
                "(nothing printed yet)"
            } else {
                logs.trim_end()
            };
            return Ok(if failure.is_empty() {
                logs.to_owned()
            } else {
                format!(
                    "{failure}

{logs}"
                )
            });
        }
        let all = apps()?;
        if all.is_empty() {
            return Ok("Nothing is left running in Letronna Cloud.".into());
        }
        Ok(all
            .iter()
            .map(|a| {
                let mut line = format!("{} {} {} ({} MB)", a.thread, a.kind, a.state, a.mb);
                if let Some(at) = &a.address {
                    line.push_str(&format!(" at {at}"));
                }
                if let Some(why) = a.failure.lines().next().filter(|w| !w.is_empty()) {
                    line.push_str(&format!(": {why}"));
                }
                line
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            ))
    }
}

/// `cloud_app_power`: take an app or worker down, or bring a worker back.
pub struct CloudAppPower;

impl crate::tools::Tool for CloudAppPower {
    fn name(&self) -> &'static str {
        "cloud_app_power"
    }
    fn description(&self) -> &'static str {
        "Take one of the user's Letronna Cloud apps or 24/7 workers down (`up` false), or bring a stopped worker back (`up` true; billed from credit again while it runs). Its thread comes from cloud_apps."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "required": ["thread", "up"], "properties": {
            "thread": {"type": "string"},
            "up": {"type": "boolean", "description": "true to bring it back, false to take it down."}
        }})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        true
    }
    fn open_world(&self) -> bool {
        true
    }
    fn machine(&self) -> bool {
        false
    }
    fn offered(&self, _: &crate::chat::Connection) -> bool {
        !inside_a_run() && crate::auth::signed_in()
    }
    fn preview(&self, args: &serde_json::Value, _: &crate::tools::ToolContext) -> String {
        let thread = args["thread"].as_str().unwrap_or_default();
        if args["up"].as_bool() == Some(true) {
            format!("Bring {thread} back up in Letronna Cloud, billed from credit")
        } else {
            format!("Take {thread} down in Letronna Cloud")
        }
    }
    fn run(
        &self,
        args: &serde_json::Value,
        _: &crate::tools::ToolContext,
    ) -> Result<String, String> {
        let thread = args["thread"].as_str().unwrap_or_default().trim();
        if thread.is_empty() {
            return Err("Say which one in `thread`; cloud_apps lists them.".into());
        }
        let up = args["up"].as_bool() == Some(true);
        app_power(thread, up)?;
        Ok(if up {
            format!("{thread} is starting again; it is billed from credit while it runs.")
        } else {
            format!("{thread} is down.")
        })
    }
}

fn or_light(tier: &str) -> &str {
    if tier.is_empty() { "light" } else { tier }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    #[test]
    fn events_read_as_what_the_run_says() {
        assert_eq!(
            heard_of(r#"{"type":"status","status":"running"}"#),
            Some((Heard::Status("running".into()), false))
        );
        assert_eq!(
            heard_of(r#"{"type":"progress","rounds":2,"doing":"run_command: cargo build"}"#),
            Some((
                Heard::Doing {
                    rounds: 2,
                    doing: "run_command: cargo build".into()
                },
                false
            ))
        );
        assert_eq!(
            heard_of(r#"{"type":"end","status":"done"}"#),
            Some((Heard::Status("done".into()), true))
        );
        assert_eq!(heard_of("not json"), None);
    }

    #[test]
    fn the_cloud_tool_says_where_things_are_built_and_asks_first() {
        let tool = CloudTask;
        let said = tool.description();
        let order = [
            "self-contained file",
            "built here",
            "only when this computer cannot",
        ];
        let at: Vec<usize> = order.iter().map(|w| said.find(w).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{said}");
        assert!(tool.needs_approval() && !tool.machine());
        let card = tool.preview(
            &serde_json::json!({"task": "Build the site", "repo": "https://x/y"}),
            &Default::default(),
        );
        assert_eq!(
            card,
            "Run in Letronna Cloud (light):\nBuild the site\nin https://x/y"
        );
    }

    #[test]
    fn a_run_links_to_its_page_on_the_website() {
        let page = run_page("run_a");
        assert!(
            page.starts_with("http") && page.ends_with("/dashboard/cloud/run/run_a"),
            "{page}"
        );
    }

    #[test]
    fn a_task_sends_only_what_it_was_given() {
        let task = Task {
            prompt: "Fix the tests".into(),
            tier: "normal".into(),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&task).unwrap(),
            serde_json::json!({"prompt": "Fix the tests", "tier": "normal"})
        );
        let run: Run = serde_json::from_value(serde_json::json!({
            "id": "run_a", "thread": "th_b", "tier": "light", "status": "done", "answer": "Fixed."
        }))
        .unwrap();
        assert!(run.over() && run.answer == "Fixed.");
        assert!(
            !Run {
                status: "running".into(),
                ..run
            }
            .over()
        );
    }

    #[test]
    fn a_packed_folder_keeps_what_gitignore_keeps_and_never_git() {
        use base64::Engine;
        let dir = std::env::temp_dir().join(format!("letronna-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in [
            ("bot.py", "print(1)"),
            (".env", "TOKEN=x"),
            (".gitignore", "build/\n"),
            ("src/lib.py", "x = 1"),
            ("build/out.bin", "big"),
            (".git/config", "[core]"),
        ] {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let packed = base64::engine::general_purpose::STANDARD
            .decode(pack(&dir).unwrap())
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&packed[..]));
        let mut names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| {
                e.unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        names.sort();
        assert_eq!(names, [".env", ".gitignore", "bot.py", "src/lib.py"]);
    }

    #[test]
    fn a_runs_progress_reads_as_its_round_and_tools_and_keeps_new_steps() {
        let mut steps = Vec::new();
        let doing = |rounds, doing: &str| Heard::Doing {
            rounds,
            doing: doing.into(),
        };
        let lines = [
            progress_line("run_1", Heard::Status("running".into()), &mut steps),
            progress_line("run_1", doing(1, "run_command"), &mut steps),
            progress_line("run_1", doing(2, "run_command"), &mut steps),
            progress_line("run_1", doing(3, "write_file, run_command"), &mut steps),
            progress_line("run_1", doing(4, ""), &mut steps),
        ];
        assert_eq!(
            lines,
            [
                "Cloud run run_1: running",
                "Cloud run run_1: round 1: run_command",
                "Cloud run run_1: round 2: run_command",
                "Cloud run run_1: round 3: write_file, run_command",
                "Cloud run run_1: round 4",
            ]
        );
        assert_eq!(steps, ["run_command", "write_file, run_command"]);
    }
}
