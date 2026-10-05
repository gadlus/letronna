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

#[cfg(test)]
mod tests {
    use super::*;

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
