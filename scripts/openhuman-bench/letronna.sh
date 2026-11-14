#!/usr/bin/env bash
# Letronna's adapter for OpenHuman's benchmark rig, in the contract of its
# bundles/adapters/*.sh: _common.sh gives PROMPT, a fresh HOME and the task's
# working directory, and runner/entry.mjs runs this under TASK_TIMEOUT_S with
# stdout and stderr going to the task's harness.log. rig.patch puts a copy in
# the rig as bundles/adapters/letronna.sh.
. "$(dirname "$0")/_common.sh"

# A data folder of its own with nothing set up: the route below is the
# environment's, saved nowhere, so the key lives in this process alone.
export AGENT_DATA_DIR="$HOME/letronna-data"
mkdir -p "$AGENT_DATA_DIR"

# The meter proxy. Under Harbor (Terminal-Bench) it is a unix socket, reached
# through a loopback forwarder; otherwise the compose service by name, which
# Letronna sends a key to over plain HTTP only when told the host is on a
# local network. The key it sends is the rig's dummy one either way.
if [ -n "${PROXY_SOCKET:-}" ]; then
  node /opt/harness/forward.mjs 18080 "$PROXY_SOCKET" &
  fwd=$!
  trap 'kill $fwd 2>/dev/null || true' EXIT
  for _ in $(seq 1 50); do (echo > /dev/tcp/127.0.0.1/18080) 2>/dev/null && break; sleep 0.1; done
  proxy=http://127.0.0.1:18080
else
  proxy="$PROXY_URL"
  export LETRONNA_ALLOW_HTTP=1
fi
# BENCH_PROXY_PREFIX tags each call with its trial, so Harbor trials can run
# side by side.
export AGENT_BASE_URL="$proxy${BENCH_PROXY_PREFIX:-}/v1"
export AGENT_API_KEY="$DUMMY_API_KEY"
export AGENT_MODEL="$BENCH_MODEL"

# The rig's turn budget (runner/turn-budget.mjs): the task's budget less
# BENCH_TURN_MARGIN_S, 120 s unless set, but at least half of it. At 80% of
# it Letronna is told to write its outputs, and at the end it stops with its
# result line, before the runner's kill. A budget of 0, which Harbor gives
# when the task's own is unclear, sets none; a LETRONNA_DEADLINE given wins.
budget="${BENCH_TURN_BUDGET_S:-${TASK_TIMEOUT_S:-0}}"
budget="${budget%%.*}"
if [ -z "${LETRONNA_DEADLINE:-}" ] && [ "$budget" -gt 0 ] 2>/dev/null; then
  left=$((budget - ${BENCH_TURN_MARGIN_S:-120}))
  half=$((budget / 2))
  deadline=$((left > half ? left : half))
  export LETRONNA_DEADLINE=$((deadline > 0 ? deadline : 1))
fi

# Seconds a reply may stay silent: Letronna's own 300 would give up on a
# call the provider is slow to start under load, which the other harnesses
# wait out.
export LETRONNA_READ_TIMEOUT="${LETRONNA_READ_TIMEOUT:-1200}"
# The task container is thrown away after the task, so an unwatched Bypass
# run may do what is hard to take back, such as `rm -r` or `git reset
# --hard`, as the other harnesses' permission bypasses do.
export LETRONNA_DISPOSABLE=1
# Its files are thrown away with it, so a run that has read a web page or
# one of the task's documents may still move, write over and delete the
# task's files; nothing that sends data off the machine is let through.
export LETRONNA_DISPOSABLE_FILES=1
# The agent's PATH and python are the task image's, not a login shell's.
export LETRONNA_NO_LOGIN_ENV=1
# Nothing may open a sign-in page or any other in a browser.
export BROWSER=/usr/bin/false
export NO_COLOR=1

# 300 tool rounds, as nobody is there to say "continue" and a DeepSWE task
# can take more than 150. The result line goes to harness.log with the rest,
# and to letronna.json beside it on its own.
/opt/harness/letronna/letronna -p "$PROMPT" --output json --mode bypass --max-rounds 300 \
  | tee "${RESULT_DIR:-/results}/letronna.json"
