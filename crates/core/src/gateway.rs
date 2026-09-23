pub const Upstream: &str = "https://pool.example/v1";

pub struct Gateway {
    pub name: &'static str,
    pub base: &'static str,
    pub needs_key: bool,
    pub blurb: &'static str,
}

pub const GATEWAYS: &[Gateway] = &[
    Gateway {
        name: "Upstream Free",
        base: Upstream,
        needs_key: false,
        blurb: "Guest access to the free catalog. No key, a few replies per session.",
    },
    Gateway {
        name: "Upstream",
        base: Upstream,
        needs_key: true,
        blurb: "Your Upstream account with an API key. Marketplace catalog and quota.",
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

/// Both Upstream gateways share one saved key.
pub fn key_slot(name: &str) -> &str {
    if gateway(name).base == Upstream {
        "Upstream"
    } else {
        name
    }
}

pub fn gateway(name: &str) -> &'static Gateway {
    GATEWAYS
        .iter()
        .find(|g| g.name == name)
        .unwrap_or(&GATEWAYS[0])
}
