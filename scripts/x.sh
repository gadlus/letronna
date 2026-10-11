#!/usr/bin/env bash
# Runs a cargo command, then clears the build cache it left behind.
#   scripts/x.sh build --release
#   scripts/x.sh test
# Finished binaries, Letronna.app and the dmg stay; the intermediates
# (deps, build scripts, incremental state, fingerprints) go, which is
# nearly all of target/. KEEP_CACHE=1 skips the clean for fast rebuilds.
set -uo pipefail
cd "$(dirname "$0")/.."

cargo "$@"
status=$?

if [ "${KEEP_CACHE:-0}" != 1 ] && [ -d target ]; then
  before=$(du -sk target | cut -f1)
  find target -mindepth 2 -maxdepth 3 -type d \
    \( -name deps -o -name build -o -name incremental -o -name .fingerprint -o -name examples \) \
    -prune -exec rm -rf {} +
  rm -rf target/doc target/tmp target/package
  after=$(du -sk target | cut -f1)
  echo "cleared $(( (before - after) / 1024 )) MB of build cache" >&2
fi

exit $status
