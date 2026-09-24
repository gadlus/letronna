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

/// Where to send requests and what to send with them. For most gateways
/// that is the saved endpoint and key; the Letronna gateway uses the
/// account API and the token this computer was signed in with, so there is
/// nothing to paste and nothing to go stale.
pub fn resolve(name: &str, endpoint: &str, key: &str) -> (String, String) {
    if name == LETRONNA {
        let token = crate::auth::account().map(|a| a.token).unwrap_or_default();
        return (crate::auth::hosted_base(), token);
    }
    (endpoint.to_owned(), key.to_owned())
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
