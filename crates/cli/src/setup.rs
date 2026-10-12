//! The first thing anybody sees: their account, who the model is, and where
//! it all goes.
//!
//! The one-line installers fetch Letronna and hand straight over to this, so
//! the script only delivers and every decision is made here, in the program,
//! where it can be asked properly and changed later with the same commands.
//! It also runs by itself the first time Letronna starts with nothing set up,
//! which is most of the reason a script needs so little in it.

use letronna_core::auth;
use letronna_core::gateway::{self, gateways};
use letronna_core::store;

use crate::render;
use crate::session::Runner;

/// Steps, for the heading over each one. Knowing how far there is to go is
/// most of what makes a setup feel short.
const STEPS: usize = 3;

/// Walk through it. True when a model is ready to talk to at the end.
/// `installer` is true for `letronna setup`, which ends here rather than
/// going on into the conversation, so the last line says how to start it.
pub fn run(runner: &mut Runner, installer: bool) -> bool {
    render::blank();
    render::line(&format!(
        "{}  {}",
        render::bold(&render::blue(letronna_core::BRAND)),
        render::dim(concat!("setup  ·  ", env!("CARGO_PKG_VERSION")))
    ));
    render::blank();
    render::said(&render::blue("●"), &render::bold("Welcome"));
    render::line(&render::dim(
        "Three questions, then you are talking to it. All of it can be changed later.",
    ));

    if connect(runner) {
        choose_model(runner);
    }
    crate::install::offer(runner, 3, STEPS);

    let ready = !runner.store.settings.model.is_empty();
    render::blank();
    if ready {
        render::said(&render::blue("●"), &render::bold("Ready"));
        render::line(&render::dim(if installer {
            "Start it with `letronna` in a new terminal. `letronna setup` asks all this again."
        } else {
            "Ask it something. /model, /login and /effort change any of this later."
        }));
    } else {
        render::said(&render::blue("●"), &render::bold("Nearly"));
        render::line(&render::dim(
            "No model yet. /login or `letronna setup` connects one, /model picks it.",
        ));
    }
    ready
}

fn step(number: usize, title: &str) {
    render::blank();
    render::said(
        &render::blue("●"),
        &format!(
            "{}  {}",
            render::bold(title),
            render::dim(&format!("{number} of {STEPS}"))
        ),
    );
}

/// One question for how to reach a model: the account, a key, this
/// computer, or another server. False when it was skipped.
fn connect(runner: &mut Runner) -> bool {
    step(1, "Connect a model");
    let signed_in = auth::account();
    let account = match &signed_in {
        Some(a) => format!("Letronna account  —  signed in as {}", a.name),
        None => "Letronna account  —  hosted models, free starter credit. Recommended.".into(),
    };
    // Asked before anything is opened: a browser appearing unbidden is the
    // sort of thing that makes people close an installer.
    let picked = runner.bar.choose(
        "How should Letronna reach a model?",
        Vec::new(),
        &[
            account.as_str(),
            "My own API key  —  OpenAI, Anthropic or OpenRouter.",
            "On this computer  —  Ollama. Works offline, no key.",
            "Another server  —  anything OpenAI-compatible, by its URL.",
            "Skip for now",
        ],
    );
    let name = match picked {
        Some(0) => gateway::LETRONNA,
        Some(1) => {
            let keyed: Vec<_> = gateways().into_iter().filter(|g| g.needs_key).collect();
            let labels: Vec<String> = keyed
                .iter()
                .map(|g| format!("{}  —  {}", g.name, g.blurb))
                .collect();
            let options: Vec<&str> = labels.iter().map(String::as_str).collect();
            match runner.bar.choose("Which provider?", Vec::new(), &options) {
                Some(i) => keyed[i].name,
                None => return skipped(),
            }
        }
        Some(2) => "Ollama",
        Some(3) => "Custom",
        _ => return skipped(),
    };
    let Some(chosen) = gateways().into_iter().find(|g| g.name == name) else {
        return skipped();
    };

    let mut base = chosen.base.to_owned();
    if base.is_empty() {
        base = runner
            .bar
            .ask("base URL, like http://localhost:8000/v1", false)
            .unwrap_or_default()
            .trim()
            .to_owned();
    }
    runner.store.settings.gateway = chosen.name.to_owned();
    runner.store.settings.endpoint = base.trim_end_matches('/').to_owned();
    if chosen.base.is_empty() {
        runner.store.settings.custom_endpoint = runner.store.settings.endpoint.clone();
    }

    // The hosted pool is paid for by the account, so it needs one.
    if chosen.name == gateway::LETRONNA && signed_in.is_none() {
        render::line(&render::dim("Opening your browser to sign in."));
        crate::commands::sign_in(runner);
    }

    if chosen.needs_key {
        let slot = gateway::key_slot(chosen.name).to_owned();
        let keys = store::load_keys();
        let have = keys.get(&slot).is_some_and(|k| !k.trim().is_empty());
        render::line(&render::dim(if have {
            "A key is already saved. Paste a new one, or enter to keep it."
        } else {
            "Paste your key. It is shown as dots and stays on this machine."
        }));
        let key = runner
            .bar
            .ask("API key", true)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !key.is_empty() {
            if let Err(error) = store::save_key(&slot, Some(&key)) {
                render::line(&render::red(&format!("Could not save the key: {error}")));
            } else {
                render::line(&render::dim("Key saved."));
            }
        }
    }
    runner.persist();
    render::line(&format!(
        "{} {}",
        render::dim("connected through"),
        render::bold(chosen.name)
    ));
    true
}

fn skipped() -> bool {
    render::line(&render::dim("Skipped. `letronna setup` asks again."));
    false
}

fn choose_model(runner: &mut Runner) {
    step(2, "Pick a model");
    crate::commands::pick_model(runner, None, "", true);
    // A provider that would not list its models -- a local server, or a key
    // it did not like -- still needs something to talk to.
    if runner.store.settings.model.is_empty() {
        render::line(&render::dim(
            "Type a model id instead, or enter to leave it for now.",
        ));
        let typed = runner
            .bar
            .ask("model id", false)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !typed.is_empty() {
            runner.store.settings.model = typed;
            runner.persist();
        }
    }
}
