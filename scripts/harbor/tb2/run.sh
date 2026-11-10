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
#   TB2_RELAY_TOKEN the token relay.py was started with (required)
#   TB2_CA          a CA bundle for task containers whose TLS this network
#                   re-terminates; unset, the containers are left as they are
#   TB2_ALONE       tasks run one at a time (the four largest images)
#   TB2_ALONE_START how many times Harbor's start timeout, 600 s for most
#                   tasks, those get to pull their image (4)
#   TB2_AGENT=oracle  runs each task's own solution instead, with no model or
#                   relay: which tasks this host can pass at all
#   TB2_METER       the relay's meters, space-separated (TB2_WORK/meter.jsonl)
#   TB2_BUDGET      dollars: Harbor is stopped once spend.py finds the meters'
#                   charges reach them, and no more trials start
#   TB2_RERUNS      how many times a task is run again when the infrastructure
#                   cost its trial, as aside.py judges (2)
#   TB2_TASKS       a file naming the tasks to run, one a line, with # for a
#                   comment; unset, all of them. subsets/dev.txt is a dozen
#                   that Letronna's own handling has decided, for comparing
#                   two builds (pair.sh) without a whole run
#
# Running this again resumes both jobs, setting aside and rerunning their
# cancelled trials; `harbor job resume -p TB2_WORK/jobs/JOB` resumes one.
# Harbor refuses to resume a job whose config or overlay has changed.
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
read -r -a meters <<<"${TB2_METER:-$work/meter.jsonl}"
started=$(date +%s)
# A budget spend.py cannot check would leave the guard below off unseen.
if [ -n "${TB2_BUDGET:-}" ]; then
  python3 "$here/spend.py" "${meters[@]}" --budget "$TB2_BUDGET" >/dev/null || [ $? = 3 ] ||
    { echo "spend.py cannot check TB2_BUDGET, for the reason above." >&2; exit 1; }
fi
if [ "${TB2_AGENT:-}" = oracle ]; then
  agent=(-a oracle)
else
  bin=${LETRONNA_BIN:?the static Linux letronna to run, as LETRONNA_BIN}
  : "${TB2_RELAY_TOKEN:?the token relay.py was started with, as TB2_RELAY_TOKEN}"
  curl -fsS -H "Authorization: Bearer $TB2_RELAY_TOKEN" "http://127.0.0.1:$port/v1/models" >/dev/null ||
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
  fi
  # uv, which installs each verifier's tests, gives up on a read stalled for
  # 30 s, and the trial then scores 0 with no test run.
  echo "    environment:"
  echo '      UV_HTTP_TIMEOUT: "120"'
  if [ -n "${TB2_CA:-}" ]; then
    for v in SSL_CERT_FILE REQUESTS_CA_BUNDLE CURL_CA_BUNDLE PIP_CERT NODE_EXTRA_CA_CERTS \
      GIT_SSL_CAINFO CARGO_HTTP_CAINFO AWS_CA_BUNDLE; do
      echo "      $v: /etc/tb2-ca-bundle.crt"
    done
    echo '      UV_NATIVE_TLS: "true"'
  fi
} >"$overlay"

# The largest image takes 9 GB compressed, and more once unpacked.
root=$(docker info -f '{{.DockerRootDir}}' 2>/dev/null) || root=
free=$(df -Pk "${root:-/var/lib/docker}" 2>/dev/null | awk 'NR == 2 {print int($4 / 1048576)}') || free=
if [ -n "$free" ] && [ "$free" -lt 20 ]; then
  echo "Docker has $free GB free under ${root:-/var/lib/docker}: under 20 GB, the largest tasks may not start." >&2
fi
# A relay that is no stub meters the call for its models above, just after
# answering it, so a meter still missing is most likely one it does not
# write, and the budget and aside.py's gateway rule would read nothing from it.
if [ "${TB2_AGENT:-}" != oracle ]; then
  for m in "${meters[@]}"; do
    for _ in 1 2 3; do [ -e "$m" ] && break; sleep 1; done
    [ -e "$m" ] ||
      echo "No meter at $m yet: unless TB2_METER names the relay's --meter, TB2_BUDGET and the gateway reruns see no call." >&2
  done
fi

mkdir -p "$work/jobs"
python3 "$here/janitor.py" "$tasks" "$work/jobs" >>"$work/jobs/$job.janitor.log" 2>&1 &
janitor=$!
guard=

# Whether the meters' charges reach TB2_BUDGET.
spent() {
  local status=0
  [ -n "${TB2_BUDGET:-}" ] || return 1
  python3 "$here/spend.py" "${meters[@]}" --budget "$TB2_BUDGET" >/dev/null || status=$?
  [ "$status" -eq 3 ]
}
# Whether the upstream refused the last call of this run with 402, as it
# would refuse every rerun until the key is topped up. Calls from before
# this run are left out, so a run after a top-up gets going.
out_of_credit() {
  local status=0
  [ "${TB2_AGENT:-}" != oracle ] || return 1
  python3 "$here/spend.py" "${meters[@]}" --since "$started" --credit >/dev/null || status=$?
  [ "$status" -eq 4 ]
}
# Harbor ends a run on SIGTERM as on Ctrl-C, writing the trials it stops as
# cancelled; running this again sets those aside and runs them. One signal
# is enough, as no Harbor run starts once the budget is spent, and a second
# would cut short Harbor's teardown. The guard then sleeps on, under its own
# PID, so the kill at the end finds it and not a process given that PID since.
if [ -n "${TB2_BUDGET:-}" ]; then
  (
    while sleep 60; do if spent && pkill -TERM -P $$ -f '(^|/)harbor run '; then break; fi; done
    exec sleep infinity
  ) &
  guard=$!
fi
# Stopped at the end, then one last sweep for the trials it had not seen end.
trap 'kill $janitor $guard 2>/dev/null || true; python3 "$here/janitor.py" --once "$tasks" "$work/jobs" >>"$work/jobs/$job.janitor.log" 2>&1 || true' EXIT
# A Ctrl-C ends the Harbor running, then this, not the next phase.
trap 'exit 130' INT

common=(-o "$work/jobs" --extra-docker-compose "$overlay" -q "${agent[@]}")
skip=() only=()
for t in "${alone[@]}"; do skip+=(-x "$t"); only+=(-i "$t"); done
# Only the tasks a file names: those among `alone` still one at a time in
# the second job, the rest in the first, and a job with none is not run.
if [ -n "${TB2_TASKS:-}" ]; then
  [ -r "$TB2_TASKS" ] || { echo "TB2_TASKS: cannot read $TB2_TASKS" >&2; exit 1; }
  skip=() only=()
  while IFS= read -r t || [ -n "$t" ]; do
    t=$(printf '%s' "${t%%#*}" | tr -d '[:space:]')
    [ -n "$t" ] || continue
    if printf '%s\n' "${alone[@]}" | grep -qxF -- "$t"; then only+=(-i "$t"); else skip+=(-i "$t"); fi
  done <"$TB2_TASKS"
  [ $((${#skip[@]} + ${#only[@]})) -gt 0 ] || { echo "TB2_TASKS: $TB2_TASKS names no task" >&2; exit 1; }
fi
judged=()
if [ "${TB2_AGENT:-}" != oracle ]; then
  for m in "${meters[@]}"; do judged+=(--meter "$m"); done
fi

# The relay holds the provider key; Letronna sends the relay's own token.
export OPENAI_API_KEY=${TB2_RELAY_TOKEN:-held-by-the-relay}
export PYTHONPATH="$here/..${PYTHONPATH:+:$PYTHONPATH}"

# Whether to start no more Harbor runs, and why.
halted() {
  if spent; then
    echo "$(date -u +%FT%TZ) $1: stopped, as the meters reach TB2_BUDGET (\$$TB2_BUDGET)"
  elif out_of_credit; then
    echo "$(date -u +%FT%TZ) $1: stopped, as the upstream answers 402: top up the key and run this again"
  else
    return 1
  fi
}

# One job, run again while aside.py sets aside trials the infrastructure
# cost, as Harbor resuming a job runs the trials it is missing. Trials are
# set aside only once Harbor has taken the job and will run again at once:
# a job it refuses, or a run halted, would leave them out of the report.
phase() {
  local name=$1 moved='' status=0
  shift
  halted "$name" && return
  while true; do
    harbor run -p "$tasks" --job-name "$name" "${common[@]}" "$@" || status=$?
    halted "$name" && return
    if [ "$status" != 0 ]; then
      echo "$(date -u +%FT%TZ) $name: stopped, as Harbor exited with status $status"
      return
    fi
    # A trial set aside goes out of the janitor's sight, so first it stops
    # what that trial left running.
    python3 "$here/janitor.py" --once "$tasks" "$work/jobs" >>"$work/jobs/$job.janitor.log" 2>&1 || true
    # aside.py names each trial as it moves it, so those it moved before
    # failing are run again too.
    moved=$(python3 "$here/aside.py" "$work/jobs/$name" --to "$work/aside" \
      --max "${TB2_RERUNS:-2}" "${judged[@]}") || true
    [ -n "$moved" ] || return 0
    printf '%s\n' "$moved" | sed "s|^|$(date -u +%FT%TZ) run again: |"
  done
}

if [ -n "${TB2_TASKS:-}" ]; then
  echo "$(date -u +%FT%TZ) $job: the $((${#skip[@]} / 2 + ${#only[@]} / 2)) tasks $TB2_TASKS names"
else
  echo "$(date -u +%FT%TZ) $job: ${#alone[@]} tasks alone after the rest $n at a time"
fi
reported=()
if [ -z "${TB2_TASKS:-}" ] || [ ${#skip[@]} -gt 0 ]; then
  phase "$job" "${skip[@]}" -n "$n" "$@"
  reported+=("$work/jobs/$job")
fi
if [ -z "${TB2_TASKS:-}" ] || [ ${#only[@]} -gt 0 ]; then
  phase "$job-alone" "${only[@]}" -n 1 --environment-build-timeout-multiplier "${TB2_ALONE_START:-4}" "$@"
  reported+=("$work/jobs/$job-alone")
fi
echo "$(date -u +%FT%TZ) $job done"
# A run halted before any trial ended has no report, but still its spend.
status=0
python3 "$here/report.py" "${reported[@]}" || status=$?
[ "${TB2_AGENT:-}" = oracle ] || python3 "$here/spend.py" "${meters[@]}"
exit "$status"
