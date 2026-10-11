#!/usr/bin/env bash
# Runs a cargo command, then clears the build cache once target/ grows past
# a limit (CACHE_LIMIT_GB, default 10). Below it the cache stays, so rebuilds
# stay fast.
#   scripts/x.sh build --release
#   scripts/x.sh test
# Finished binaries, Letronna.app and the dmg stay; the intermediates
# (deps, build scripts, incremental state, fingerprints) go, which is
# nearly all of target/. KEEP_CACHE=1 never clears; CACHE_LIMIT_GB=0 always does.
set -uo pipefail
cd "$(dirname "$0")/.."

cargo "$@"
status=$?

limit_kb=$(( ${CACHE_LIMIT_GB:-10} * 1024 * 1024 ))
before=$( [ -d target ] && du -sk target | cut -f1 || echo 0 )
if [ "${KEEP_CACHE:-0}" != 1 ] && [ "$before" -ge "$limit_kb" ]; then
  find target -mindepth 2 -maxdepth 3 -type d \
    \( -name deps -o -name build -o -name incremental -o -name .fingerprint -o -name examples \) \
    -prune -exec rm -rf {} +
  rm -rf target/doc target/tmp target/package
  after=$(du -sk target | cut -f1)
  echo "cleared $(( (before - after) / 1024 )) MB of build cache" >&2
fi

exit $status
