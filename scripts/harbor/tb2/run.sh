#!/usr/bin/env bash
# All of Terminal-Bench 2.0 through Harbor with Letronna's agent, on one
# Linux host: the tasks whose images are 6-9 GB compressed one at a time,
# the rest TB2_N at a time, with janitor.py removing each task's image once
# its trial is done. relay.py must already be answering on TB2_RELAY_PORT.
#
#   LETRONNA_BIN=/path/to/static/letronna scripts/harbor/tb2/run.sh JOB [-- harbor run args]
#
#   TB2_WORK        where the tasks and the job folders go (./tb2-work)
#   TB2_N           trials at a time for the smaller tasks (3)
#   TB2_MODEL       the model, as Harbor names it (openai/deepseek/deepseek-v4.1-flash)
#   TB2_RELAY_PORT  the relay's port on this host (18080)
#   TB2_CA          a CA bundle for task containers whose TLS this network
#                   re-terminates; unset, the containers are left as they are
#   TB2_ALONE       tasks run one at a time (the four largest images)
#   TB2_AGENT=oracle  runs each task's own solution instead, with no model or
#                   relay: which tasks this host can pass at all
#
# Trials resume with `harbor job resume -p TB2_WORK/jobs/JOB` (and JOB-alone).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
job=${1:?a job name}
shift
[ "${1:-}" = "--" ] && shift

work=$(mkdir -p "${TB2_WORK:-tb2-work}" && cd "${TB2_WORK:-tb2-work}" && pwd)
n=${TB2_N:-3}
model=${TB2_MODEL:-openai/deepseek/deepseek-v4.1-flash}
port=${TB2_RELAY_PORT:-18080}
read -r -a alone <<<"${TB2_ALONE:-mteb-leaderboard mteb-retrieve hf-model-inference pytorch-model-recovery}"
if [ "${TB2_AGENT:-}" = oracle ]; then
  agent=(-a oracle)
else
  bin=${LETRONNA_BIN:?the static Linux letronna to run, as LETRONNA_BIN}
  curl -fsS "http://127.0.0.1:$port/v1/models" >/dev/null ||
    { echo "Nothing answers on port $port: start relay.py first." >&2; exit 1; }
  agent=(
    -a harness_letronna.agent:Letronna -m "$model" --ak "binary=$bin"
    --ae "OPENAI_BASE_URL=http://host.docker.internal:$port/v1" --ae LETRONNA_ALLOW_HTTP=1
  )
fi
tasks="$work/tasks/terminal-bench"
[ -d "$tasks" ] || harbor download terminal-bench@2.0 -o "$work/tasks"

# host.docker.internal is the host on Linux too, and Letronna sends to it
# over plain HTTP since its name ends in .internal.
overlay="$work/overlay.yaml"
{
  echo "services:"
  echo "  main:"
  echo "    extra_hosts:"
  echo '      - "host.docker.internal:host-gateway"'
  if [ -n "${TB2_CA:-}" ]; then
    echo "    volumes:"
    echo "      - $TB2_CA:/etc/tb2-ca-bundle.crt:ro"
    echo "    environment:"
    for v in SSL_CERT_FILE REQUESTS_CA_BUNDLE CURL_CA_BUNDLE PIP_CERT NODE_EXTRA_CA_CERTS \
      GIT_SSL_CAINFO CARGO_HTTP_CAINFO AWS_CA_BUNDLE; do
      echo "      $v: /etc/tb2-ca-bundle.crt"
    done
    echo '      UV_NATIVE_TLS: "true"'
  fi
} >"$overlay"

mkdir -p "$work/jobs"
python3 "$here/janitor.py" "$tasks" "$work/jobs" >>"$work/jobs/$job.janitor.log" 2>&1 &
janitor=$!
# Stopped at the end, then one last sweep for the trials it had not seen end.
trap 'kill $janitor 2>/dev/null || true; python3 "$here/janitor.py" --once "$tasks" "$work/jobs" >>"$work/jobs/$job.janitor.log" 2>&1 || true' EXIT

common=(-o "$work/jobs" --extra-docker-compose "$overlay" -q "${agent[@]}")
skip=() only=()
for t in "${alone[@]}"; do skip+=(-x "$t"); only+=(-i "$t"); done

# The relay holds the key; this one only satisfies Harbor's check for one.
export OPENAI_API_KEY=held-by-the-relay
export PYTHONPATH="$here/..${PYTHONPATH:+:$PYTHONPATH}"
echo "$(date -u +%FT%TZ) $job: ${#alone[@]} tasks alone after the rest $n at a time"
harbor run -p "$tasks" "${skip[@]}" -n "$n" --job-name "$job" "${common[@]}" "$@" || true
harbor run -p "$tasks" "${only[@]}" -n 1 --job-name "$job-alone" "${common[@]}" "$@" || true
echo "$(date -u +%FT%TZ) $job done"
python3 "$here/report.py" "$work/jobs/$job" "$work/jobs/$job-alone"
