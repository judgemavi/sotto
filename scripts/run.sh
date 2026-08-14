#!/bin/bash
set -euo pipefail

# Build, bundle, and run in one step.
#
# Running ./target/Sotto.app/Contents/MacOS/sotto directly is how five "verified" runs
# during T057 debugging were made against binaries that predated the fix being tested.
# Use this instead; cargo is a no-op when nothing changed.

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"${REPO_ROOT}/scripts/dev-bundle.sh" >/dev/null
exec "${REPO_ROOT}/target/Sotto.app/Contents/MacOS/sotto" "$@"
