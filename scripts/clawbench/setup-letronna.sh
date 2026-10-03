#!/bin/bash
set -e

# Writes Letronna's settings and key into its data folder, AGENT_DATA_DIR,
# from the model the test driver hands over (models.yaml): MODEL_NAME,
# BASE_URL, API_TYPE, and API_KEYS or API_KEY. `letronna -p` with no model
# set up stops at once, and the terminal has no flag or variable for the
# endpoint, so the settings file is the only way in.
if [ -z "$BASE_URL" ] || [ -z "$MODEL_NAME" ] || [ -z "$API_TYPE" ] || [ -z "$AGENT_DATA_DIR" ]; then
  echo "ERROR: BASE_URL, MODEL_NAME, API_TYPE and AGENT_DATA_DIR must be set"
  exit 1
fi

if [ -n "$TEMPERATURE" ]; then
  echo "WARN: Letronna has no temperature flag; TEMPERATURE='$TEMPERATURE' will be ignored."
fi
if [ -n "$MAX_TOKENS" ]; then
  echo "WARN: Letronna has no max-tokens flag; MAX_TOKENS='$MAX_TOKENS' will be ignored."
fi

python3 - <<'PYEOF'
import json
import os
import urllib.request
from pathlib import Path

base_url = os.environ["BASE_URL"].rstrip("/")
model_name = os.environ["MODEL_NAME"]
api_type = os.environ["API_TYPE"]
data = Path(os.environ["AGENT_DATA_DIR"])

# One key, the first of API_KEYS, else API_KEY.
key = ""
try:
    keys = json.loads(os.environ.get("API_KEYS") or "[]")
except json.JSONDecodeError:
    keys = []
if isinstance(keys, list) and keys:
    key = str(keys[0])
    if len(keys) > 1:
        print(f"WARN: Letronna does not rotate keys; using the first of {len(keys)}")
key = key or os.environ.get("API_KEY", "")
if not key:
    raise SystemExit("ERROR: no API key provided (API_KEYS or API_KEY)")

# OpenRouter wants the full id, such as qwen/qwen3.5-397b-a17b, where
# models.yaml may name the model without its maker.
model = model_name
is_openrouter = "openrouter.ai" in base_url
if is_openrouter:
    try:
        request = urllib.request.Request(
            f"{base_url}/models", headers={"Authorization": f"Bearer {key}"}
        )
        listed = json.loads(urllib.request.urlopen(request, timeout=10).read())
        for row in listed.get("data", []):
            if row["id"] == model_name or row["id"].endswith(f"/{model_name}"):
                model = row["id"]
                break
    except Exception as e:
        print(f"WARN: could not resolve the OpenRouter model id: {e}")

data.mkdir(parents=True, exist_ok=True)
# Letronna speaks the OpenAI chat API to any server through its Custom
# gateway, so an OpenAI-compatible one is reached directly. LiteLLM, which
# the run script starts on this port, translates for the others.
litellm_port = 4000
if is_openrouter or api_type in ("openai-completions", "openai-responses"):
    endpoint = base_url
    letronna_key = key
elif api_type in ("anthropic-messages", "google-generative-ai"):
    prefix = "anthropic" if api_type == "anthropic-messages" else "gemini"
    native = (
        "https://api.anthropic.com"
        if prefix == "anthropic"
        else "https://generativelanguage.googleapis.com"
    )
    params = {"model": f"{prefix}/{model_name}", "api_key": key}
    if not base_url.startswith(native):
        params["api_base"] = base_url
    proxy = {
        "model_list": [{"model_name": model_name, "litellm_params": params}],
        # Parameters one API has and the other lacks are left out rather
        # than refused.
        "litellm_settings": {"drop_params": True},
    }
    # JSON is YAML too. In the data folder, which Letronna's tools refuse to
    # read, since it holds the key.
    config = data / "litellm-config.yaml"
    config.write_text(json.dumps(proxy, indent=2))
    config.chmod(0o600)
    endpoint = f"http://127.0.0.1:{litellm_port}"
    # LiteLLM here takes no key of its own.
    letronna_key = ""
else:
    raise SystemExit(f"ERROR: unsupported api_type for the letronna harness: {api_type}")

if letronna_key:
    keys_file = data / "keys.json"
    keys_file.write_text(json.dumps({"Custom": letronna_key}))
    keys_file.chmod(0o600)

settings = {
    "endpoint": endpoint,
    "custom_endpoint": endpoint,
    "gateway": "Custom",
    "model": model,
    # Never a keyring, which the container does not have.
    "keys_in": "files",
    # Every conversation in the browser's own context, where ClawBench's
    # recorder extension and stealth profile are, rather than in a private
    # one of Letronna's, which has neither.
    "browser_shared": True,
    # Each review is a model call the task did not ask for.
    "memory_review": False,
    "manual_updates": True,
    "notifications": False,
}
(data / "settings.json").write_text(json.dumps(settings, indent=2))
print(f"Letronna config: model={model}, endpoint={endpoint}")
PYEOF
