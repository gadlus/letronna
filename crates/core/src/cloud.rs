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
    /// A thread to carry on, from an earlier run.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub thread: String,
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
        "Run a task in Letronna Cloud, on Letronna's servers, and get its answer. Where to build what the user asks for, in order: a page, chart, document or small app that one self-contained file can hold is written here as an .html or .svg file the user previews; anything else is built here, in the project, with the tools on this computer; only when this computer cannot (a tool it needs is not on PATH and cannot be installed, it would take more memory or time than this machine has, or the user asks for the cloud) send it here. The cloud cannot see this computer's files: give it everything it needs in `task`, or an https `repo` it clones. Say in your reply that it ran in the cloud. It costs the user's credit, so each call is asked about."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "required": ["task"], "properties": {
            "task": {"type": "string", "description": "What to do, whole: the cloud sees nothing else of this conversation."},
            "repo": {"type": "string", "description": "An https git address to clone and work in."},
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
        };
        if task.prompt.is_empty() {
            return Err("Say what to do in `task`.".into());
        }
        let started = start(&task)?;
        let run = loop {
            if !ctx.cancel.sleep(Duration::from_secs(2)) {
                let _ = stop(&started.id);
                return Err("Stopped; the cloud run was stopped too.".into());
            }
            let now = run(&started.id)?;
            if now.over() {
                break now;
            }
        };
        let said = format!(
            "Ran in Letronna Cloud as {} ({}, {} s); to carry on there, pass thread {}.",
            run.id, run.tier, run.seconds, run.thread
        );
        match run.status.as_str() {
            "done" => Ok(format!("{said}\n\n{}", run.answer.trim())),
            "stopped" => Err(format!("{said} It was stopped.")),
            _ => Err(format!("{said} It failed: {}", run.error.trim())),
        }
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
}
