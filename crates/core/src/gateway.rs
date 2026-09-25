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
}
