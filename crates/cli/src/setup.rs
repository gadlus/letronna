//! The first thing anybody sees: their account, who the model is, and where
//! it all goes.
//!
//! The one-line installers fetch Letronna and hand straight over to this, so
//! the script only delivers and every decision is made here, in the program,
//! where it can be asked properly and changed later with the same commands.
//! It also runs by itself the first time Letronna starts with nothing set up,
//! which is most of the reason a script needs so little in it.

use letronna_core::auth;
use letronna_core::gateway::{self, GATEWAYS};
use letronna_core::store;

use crate::render;
use crate::session::Runner;

/// Steps, for the heading over each one. Knowing how far there is to go is
/// most of what makes a setup feel short.
const STEPS: usize = 4;

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
        "A few questions, then you are talking to it. All of it can be changed later.",
    ));

    sign_in(runner);
    if !choose_provider(runner) {
        return false;
    }
    choose_model(runner);
    crate::install::offer(runner, 4, STEPS);

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
            "No model is chosen yet. /model picks one once the provider answers.",
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

fn sign_in(runner: &mut Runner) {
    step(1, "Sign in");
    if let Some(account) = auth::account() {
        render::line(&render::dim(&format!(
            "Already signed in as {}.",
            account.name
        )));
        return;
    }
    // Asked before anything is opened: a browser appearing unbidden is the
    // sort of thing that makes people close an installer.
    let picked = runner.bar.choose(
        "Sign in to a Letronna account?",
        vec!["Hosted models and cloud runs need one. Your own keys do not.".into()],
        &["Sign in in the browser", "Skip"],
    );
    match picked {
        Some(0) => crate::commands::sign_in(runner),
        _ => render::line(&render::dim("Skipped. /login whenever you like.")),
    }
}

/// Pick where messages go, and the key for it if it wants one.
fn choose_provider(runner: &mut Runner) -> bool {
    step(2, "Who should answer?");
    let labels: Vec<String> = GATEWAYS
        .iter()
        .map(|g| format!("{}  —  {}", g.name, g.blurb))
        .collect();
    let options: Vec<&str> = labels.iter().map(String::as_str).collect();
    let Some(index) = runner.bar.choose("Pick a provider", Vec::new(), &options) else {
        render::line(&render::dim("Left it. Run `letronna setup` to come back."));
        return false;
    };
    let chosen = &GATEWAYS[index];

    let mut base = chosen.base.to_owned();
    if base.is_empty() {
        render::line(&render::dim(
            "Any OpenAI-compatible server: paste its base URL.",
        ));
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
    render::line(&format!(
        "{} {}",
        render::dim("provider"),
        render::bold(chosen.name)
    ));

    // The hosted pool is paid for by the account, so it needs one.
    if chosen.name == gateway::LETRONNA && auth::account().is_none() {
        render::line(&render::dim(
            "Letronna's models run on your account. Sign in to use them.",
        ));
        crate::commands::sign_in(runner);
    }

    if chosen.needs_key {
        let slot = gateway::key_slot(chosen.name).to_owned();
        let mut keys = store::load_keys();
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
            keys.insert(slot, key);
            if let Err(error) = store::save_keys(&keys) {
                render::line(&render::red(&format!("Could not save the key: {error}")));
            } else {
                render::line(&render::dim("Key saved."));
            }
        }
    }
    runner.persist();
    true
}

fn choose_model(runner: &mut Runner) {
    step(3, "Which model?");
    crate::commands::switch_model(runner, "");
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
