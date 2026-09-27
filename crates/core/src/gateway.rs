use std::collections::HashMap;

/// The hosted pool on your Letronna account: no key, charged to your credit.
/// First in the list, and what a new install starts on.
pub const LETRONNA: &str = "Letronna";

pub struct Gateway {
    pub name: &'static str,
    pub base: &'static str,
    pub needs_key: bool,
    pub blurb: &'static str,
}

pub const GATEWAYS: &[Gateway] = &[
    Gateway {
        name: LETRONNA,
        base: "https://api.letronna.ai/v1",
        needs_key: false,
        blurb: "Your Letronna account. Hosted models, charged to your credit. No key.",
    },
    Gateway {
        name: "OpenAI",
        base: "https://api.openai.com/v1",
        needs_key: true,
        blurb: "OpenAI directly. Needs an OpenAI API key.",
    },
    Gateway {
        name: "OpenRouter",
        base: "https://openrouter.ai/api/v1",
        needs_key: true,
        blurb: "Hundreds of models behind one key.",
    },
    Gateway {
        name: "Ollama",
        base: "http://localhost:11434/v1",
        needs_key: false,
        blurb: "Local models on this machine. Works offline.",
    },
    Gateway {
        name: "Custom",
        base: "",
        needs_key: false,
        blurb: "Any OpenAI-compatible server. Paste the base URL.",
    },
];

/// Where to send requests and what to send with them. A built-in gateway
/// goes to its own URL with its saved key; Custom goes to `custom`, the URL
/// that was pasted for it; the Letronna gateway uses the account API and the
/// token this computer was signed in with, so there is nothing to paste and
/// nothing to go stale.
pub fn resolve(name: &str, custom: &str, key: &str) -> (String, String) {
    if name == LETRONNA {
        let token = crate::auth::account().map(|a| a.token).unwrap_or_default();
        return (crate::auth::hosted_base(), token);
    }
    let base = gateway(name).base;
    let base = if base.is_empty() { custom } else { base };
    (base.to_owned(), key.to_owned())
}

/// Every gateway set up on this computer, as (name, base URL, key): Letronna
/// once signed in, a keyed gateway once its key is saved, Custom once it has
/// a URL, and Ollama always (on this machine, a closed port answers at once).
pub fn ready(keys: &HashMap<String, String>, custom: &str) -> Vec<(&'static str, String, String)> {
    GATEWAYS
        .iter()
        .filter_map(|g| {
            let key = keys.get(key_slot(g.name)).cloned().unwrap_or_default();
            let on = match g.name {
                LETRONNA => crate::auth::account().is_some(),
                "Custom" => !custom.trim().is_empty(),
                "Ollama" => true,
                _ => !key.trim().is_empty(),
            };
            on.then(|| {
                let (base, key) = resolve(g.name, custom, &key);
                (g.name, base, key)
            })
        })
        .collect()
}

/// Where a gateway stands, for the settings list and `/gateways`:
/// (status line, working, set up). `models` is how many it answered with.
pub fn status(name: &str, keys: &HashMap<String, String>, custom: &str, models: usize) -> (String, bool, bool) {
    let g = gateway(name);
    let count = |n: usize| format!("{n} model{}", if n == 1 { "" } else { "s" });
    let key = keys.get(key_slot(name)).is_some_and(|k| !k.trim().is_empty());
    match name {
        LETRONNA if crate::auth::account().is_some() => (
            if models > 0 { format!("Signed in · {}", count(models)) } else { "Signed in".into() },
            true,
            true,
        ),
        LETRONNA => ("Not signed in".into(), false, false),
        "Ollama" if models > 0 => (format!("Running · {}", count(models)), true, true),
        "Ollama" => ("Not running".into(), false, false),
        "Custom" if custom.trim().is_empty() => ("Not set up".into(), false, false),
        _ if g.needs_key && !key => ("Not set up".into(), false, false),
        _ if models > 0 => (format!("Connected · {}", count(models)), true, true),
        _ => ("Set up · no models answered".into(), false, true),
    }
}

/// The models of every ready gateway, asked all at once, as (gateway, model).
/// A gateway that does not answer is left out and its error returned, except
/// Ollama, which is only there when it is running.
pub fn all_models(
    keys: &HashMap<String, String>,
    custom: &str,
) -> (Vec<(String, String)>, Vec<String>) {
    let asks: Vec<_> = ready(keys, custom)
        .into_iter()
        .map(|(name, base, key)| {
            (name, std::thread::spawn(move || crate::chat::list_models(&base, &key)))
        })
        .collect();
    let (mut models, mut errors) = (Vec::new(), Vec::new());
    for (name, ask) in asks {
        match ask.join().unwrap_or_else(|_| Err("stopped".into())) {
            Ok(list) => models.extend(list.into_iter().map(|m| (name.to_owned(), m))),
            Err(_) if name == "Ollama" => {}
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    (models, errors)
}

/// Where a gateway's key is saved: under its own name.
pub fn key_slot(name: &str) -> &str {
    name
}

pub fn gateway(name: &str) -> &'static Gateway {
    GATEWAYS
        .iter()
        .find(|g| g.name == name)
        .unwrap_or(&GATEWAYS[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_goes_to_its_own_gateway_and_custom_keeps_its_url() {
        assert_eq!(resolve("OpenRouter", "http://x/v1", "k").0, "https://openrouter.ai/api/v1");
        assert_eq!(resolve("Custom", "http://x/v1", "k"), ("http://x/v1".into(), "k".into()));

        let keys = HashMap::from([("OpenAI".to_string(), "sk".to_string())]);
        let names: Vec<_> = ready(&keys, "").into_iter().map(|r| r.0).collect();
        assert!(names.contains(&"OpenAI") && names.contains(&"Ollama"));
        assert!(!names.contains(&"OpenRouter") && !names.contains(&"Custom"));
    }

    #[test]
    fn status_says_what_is_set_up_and_working() {
        let none = HashMap::new();
        let keyed = HashMap::from([("OpenAI".to_string(), "sk".to_string())]);
        assert_eq!(status("OpenAI", &none, "", 0), ("Not set up".into(), false, false));
        assert_eq!(status("OpenAI", &keyed, "", 0), ("Set up · no models answered".into(), false, true));
        assert_eq!(status("OpenAI", &keyed, "", 3), ("Connected · 3 models".into(), true, true));
        assert_eq!(status("Ollama", &none, "", 1), ("Running · 1 model".into(), true, true));
        assert!(!status("Custom", &none, "", 0).2);
        assert_eq!(status("Custom", &none, "http://x/v1", 2).0, "Connected · 2 models");
    }
}
