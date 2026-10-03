#!/bin/bash
set -e

# Run-time harness script for Letronna's terminal agent, `letronna -p`.
#
# Letronna drives ClawBench's Chrome itself, over CDP: LETRONNA_BROWSER_URL
# points it there, and setup's browser_shared keeps its pages in the
# browser's own context, so the recorder sees every action. It gets the
# browser and reading tools only, as the other CLI harnesses do. Its session
# file is its transcript, copied to /data/agent-messages.jsonl at the end,
# and usage-emitter.py reads each call's tokens from it.

# Stands in for / in the tests; empty in the image.
ROOT="${LETRONNA_CLAWBENCH_ROOT:-}"
DATA="$ROOT/data"
WORKSPACE="$ROOT/root/workspace"
export AGENT_DATA_DIR="$ROOT/root/.letronna"

# Tool rounds a message gets. Letronna's own default of 25 ends a checkout
# flow before its last request, the one ClawBench scores; Hermes runs here
# with 90 turns, and 150 leaves the time limit, not the rounds, to end a
# slow run.
MAX_ROUNDS=150
# Seconds Letronna has after the interrupt to end the run and let go of the
# browser before it is terminated: it gives its commands two seconds, and a
# reply being streamed stops at once.
STOP_GRACE_S=10

"$ROOT/setup-letronna.sh"

# Anthropic and Gemini models go through LiteLLM, which setup configured.
PROXY_PID=""
if [ -f "$AGENT_DATA_DIR/litellm-config.yaml" ]; then
  echo "Starting API translation proxy (litellm)..."
  litellm --config "$AGENT_DATA_DIR/litellm-config.yaml" --port 4000 \
    > "$DATA/proxy.log" 2>&1 &
  PROXY_PID=$!
  for i in $(seq 1 30); do
    if curl -sf http://localhost:4000/health/liveliness > /dev/null 2>&1; then
      echo "API proxy ready"
      break
    fi
    if [ "$i" -eq 30 ]; then
      echo "API proxy not ready after 30s; check $DATA/proxy.log"
      echo "proxy_failed" > "$DATA/.stop-reason"
      exit 1
    fi
    sleep 1
  done
fi

# Copy /my-info/ into the workspace so the agent can access it via ./my-info/
mkdir -p "$WORKSPACE"
if [ -d "$ROOT/my-info" ]; then
  cp -r "$ROOT/my-info" "$WORKSPACE/my-info"
  echo "Copied /my-info to $WORKSPACE/my-info"
fi

echo "Waiting for Chrome CDP..."
for i in $(seq 1 30); do
  if [[ "$CLAWBENCH_BROWSER_CDP_URL" == ws://* || "$CLAWBENCH_BROWSER_CDP_URL" == wss://* ]]; then
    echo "Chrome CDP ready"
    break
  fi
  if curl -sf "${CLAWBENCH_BROWSER_CDP_URL%/}/json/version" > /dev/null 2>&1; then
    echo "Chrome CDP ready"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "Chrome CDP not ready after 30s, aborting"
    echo "chrome_cdp_timeout" > "$DATA/.stop-reason"
    exit 1
  fi
  sleep 1
done

# Restrict PATH to safe read-only commands
SAFE_BIN="$ROOT/tmp/safe-bin"
mkdir -p "$SAFE_BIN"
for cmd in ls cat find file head tail wc grep sort sh bash; do
  [ -x "$(command -v "$cmd" 2>/dev/null)" ] && ln -sf "$(command -v "$cmd")" "$SAFE_BIN/$cmd"
done
ln -sf "$(command -v letronna)" "$SAFE_BIN/letronna"

LETRONNA_ARGS=(-p "$INSTRUCTION" --output json --yes
  --tools browse,use_page,read_file,list_dir --max-rounds "$MAX_ROUNDS")
case "${THINKING_LEVEL:-}" in
  "")       ;;
  off)      LETRONNA_ARGS+=(--effort none) ;;
  adaptive) LETRONNA_ARGS+=(--effort auto) ;;
  *)        LETRONNA_ARGS+=(--effort "$THINKING_LEVEL") ;;
esac

cd "$WORKSPACE"
echo "Starting Letronna (model=${MODEL_NAME})..."
: > "$DATA/usage.jsonl"
# A script runs what it puts in the background with Ctrl-C ignored, and
# Letronna then leaves it ignored, so the interrupt that stops it below would
# do nothing. Job control gives it the default back, and a process group of
# its own to signal.
set -m
PATH="$SAFE_BIN" LETRONNA_BROWSER_URL="$CLAWBENCH_BROWSER_CDP_URL" LETRONNA_NO_LOGIN_ENV=1 \
  "$SAFE_BIN/letronna" "${LETRONNA_ARGS[@]}" \
  < /dev/null > "$DATA/letronna-result.json" 2> "$DATA/agent.log" &
AGENT_PID=$!
set +m

# Watchdog: detect agent no action for 300s
IDLE_THRESHOLD=300
MAX_WAIT=${TIME_LIMIT_S:-1800}
ELAPSED=0
LAST_SIZE=0
IDLE=0
STOP_REASON=""

while kill -0 $AGENT_PID 2>/dev/null && [ "$ELAPSED" -lt "$MAX_WAIT" ]; do
  sleep 5
  ELAPSED=$((ELAPSED + 5))

  # Check if server requested stop (eval interceptor matched)
  if [ -f "$DATA/.stop-requested" ]; then
    echo "Stop requested by server (eval matched), stopping agent."
    STOP_REASON="eval_matched"
    break
  fi

  CURRENT_SIZE=$(wc -c < "$DATA/actions.jsonl" 2>/dev/null || echo 0)

  if [ "$CURRENT_SIZE" -gt 0 ] && [ "$CURRENT_SIZE" -eq "$LAST_SIZE" ]; then
    IDLE=$((IDLE + 5))
    if [ "$IDLE" -ge "$IDLE_THRESHOLD" ]; then
      echo "Agent idle for ${IDLE_THRESHOLD}s, assuming done."
      STOP_REASON="agent_idle"
      break
    fi
  else
    IDLE=0
  fi
  LAST_SIZE=$CURRENT_SIZE
done

# Determine stop reason if not set (loop exited without breaking). Letronna
# exits non-zero only when the run failed, such as on a model call that kept
# failing; said apart from a run it ended itself, though ClawBench still
# counts it as the model's.
if [ -z "$STOP_REASON" ]; then
  if kill -0 $AGENT_PID 2>/dev/null; then
    echo "Time limit (${MAX_WAIT}s) exceeded, stopping agent."
    STOP_REASON="time_limit_exceeded"
  else
    AGENT_STATUS=0
    wait $AGENT_PID || AGENT_STATUS=$?
    if [ "$AGENT_STATUS" -eq 0 ]; then
      STOP_REASON="agent_exited"
    else
      echo "Letronna exited with status ${AGENT_STATUS}; see $DATA/letronna-result.json."
      STOP_REASON="letronna_failed"
    fi
  fi
fi

echo "$STOP_REASON" > "$DATA/.stop-reason"

# Stop Letronna as Ctrl-C would, which ends the run and lets go of the
# browser and anything it started; then terminate and kill its group if it
# has not gone by the end of the grace.
if kill -INT -- -$AGENT_PID 2>/dev/null; then
  for _ in $(seq 1 "$STOP_GRACE_S"); do
    kill -0 $AGENT_PID 2>/dev/null || break
    sleep 1
  done
  if kill -TERM -- -$AGENT_PID 2>/dev/null; then
    sleep 2
    kill -KILL -- -$AGENT_PID 2>/dev/null || true
  fi
fi
if [ -n "$PROXY_PID" ]; then
  kill $PROXY_PID 2>/dev/null || true
  pkill -f "litellm" 2>/dev/null || true
fi
sleep 2

# Promote Letronna's session file to /data/agent-messages.jsonl. A fresh
# data folder holds the one session -p made, written as the run went, so it
# is there even when the run was killed.
LATEST_SESSION=$(ls -t "$AGENT_DATA_DIR"/sessions/*.jsonl 2>/dev/null | head -n 1)
if [ -n "$LATEST_SESSION" ]; then
  cp "$LATEST_SESSION" "$DATA/agent-messages.jsonl"
  python3 "$ROOT/usage-emitter.py" --input "$DATA/agent-messages.jsonl" --output "$DATA/usage.jsonl" || true
else
  echo "WARN: Letronna wrote no session file"
  : > "$DATA/agent-messages.jsonl"
fi

curl -sf -X POST http://localhost:7878/api/stop || true

# Clean up internal marker (created by /api/stop)
rm -f "$DATA/.stop-requested"

# Grace period: keep recording for 15s after agent is killed to capture end result
echo "Agent finished, recording grace period (15s)..."
sleep 15

# Stop recording
echo "Stopping recording..."
curl -sf -X POST http://localhost:7878/api/stop-recording || true
sleep 2
rm -f "$DATA"/*.log
echo "Done."
