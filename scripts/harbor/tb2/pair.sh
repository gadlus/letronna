#!/usr/bin/env bash
# Two builds of Letronna on the same Terminal-Bench tasks, one after the
# other with everything else the same, then pair.py's table of what moved.
#
#   scripts/harbor/tb2/pair.sh NAME BASE_BIN CANDIDATE_BIN [-- harbor run args]
#
# Takes run.sh's settings: the relay, TB2_MODEL, TB2_WORK and the rest.
# TB2_TASKS names the tasks; unset, subsets/dev.txt. The jobs are NAME-base
# and NAME-candidate, each resumed if this is run again. The base runs
# first, so anything that gets slower as the host fills up counts against
# the candidate, not for it.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
name=${1:?a name for the pair of jobs}
base=${2:?the build to compare against, a static Linux letronna}
candidate=${3:?the build to try}
shift 3
[ "${1:-}" = "--" ] && shift

export TB2_TASKS=${TB2_TASKS:-$here/subsets/dev.txt}
work=${TB2_WORK:-tb2-work}
for side in base candidate; do
  bin=$base
  [ "$side" = candidate ] && bin=$candidate
  # A run that ends with failed trials still has its results to pair.
  LETRONNA_BIN=$bin "$here/run.sh" "$name-$side" -- "$@" || true
done
jobs() { for job in "$work/jobs/$name-$1" "$work/jobs/$name-$1-alone"; do [ -d "$job" ] && printf '%s\n' "$job"; done; }
mapfile -t base_jobs < <(jobs base)
mapfile -t candidate_jobs < <(jobs candidate)
python3 "$here/pair.py" --base "${base_jobs[@]}" --candidate "${candidate_jobs[@]}" \
  --csv "$work/jobs/$name-pair.csv"
