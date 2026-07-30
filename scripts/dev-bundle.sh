#!/bin/bash
set -euo pipefail

# Wraps a built binary in a signed Sotto.app so it has a real bundle identity.
#
# Two things need this. SCContentSharingPicker is presented by the system on
# behalf of an application, and Screen & System Audio Recording permission is
# granted per code identity rather than per file — so a bare `cargo run` binary
# both may fail to present the picker and re-prompts TCC after every rebuild.
#
# The executable can be run directly from inside the bundle, which keeps
# command-line arguments working:
#
#   ./target/Sotto.app/Contents/MacOS/sotto 600

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly INFO_PLIST="${SCRIPT_DIR}/Info.plist"
readonly BUNDLE_PATH="${REPO_ROOT}/target/Sotto.app"

usage() {
  cat >&2 <<'USAGE'
usage: scripts/dev-bundle.sh [binary]

  binary  Path to an already-built executable. Defaults to target/release/app,
          which is built if it is missing.

environment:
  SOTTO_DEV_IDENTITY  Code-signing identity. Defaults to "-" (ad-hoc).
USAGE
}

if [[ $# -gt 1 ]]; then
  usage
  exit 64
fi

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ ! -f "${INFO_PLIST}" ]]; then
  echo "missing ${INFO_PLIST}" >&2
  exit 66
fi

if [[ $# -eq 1 ]]; then
  SOURCE_BINARY="$1"
  if [[ ! -f "${SOURCE_BINARY}" ]]; then
    echo "binary does not exist: ${SOURCE_BINARY}" >&2
    exit 66
  fi
else
  SOURCE_BINARY="${REPO_ROOT}/target/release/app"
  if [[ ! -f "${SOURCE_BINARY}" ]]; then
    echo "building ${SOURCE_BINARY}"
    (cd "${REPO_ROOT}" && cargo build --release -p app)
  fi
fi
readonly SOURCE_BINARY

# CFBundleExecutable must match the file name under Contents/MacOS, so read it
# from the plist rather than assuming the crate's binary name.
EXECUTABLE_NAME="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "${INFO_PLIST}")"
BUNDLE_IDENTIFIER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "${INFO_PLIST}")"
readonly EXECUTABLE_NAME BUNDLE_IDENTIFIER

# Guard the rm: this path is derived, and deleting the wrong tree would be bad.
if [[ "${BUNDLE_PATH}" != "${REPO_ROOT}/target/"* ]]; then
  echo "refusing to remove a bundle outside target/: ${BUNDLE_PATH}" >&2
  exit 70
fi
rm -rf "${BUNDLE_PATH}"
mkdir -p "${BUNDLE_PATH}/Contents/MacOS" "${BUNDLE_PATH}/Contents/Resources"

cp "${SOURCE_BINARY}" "${BUNDLE_PATH}/Contents/MacOS/${EXECUTABLE_NAME}"
chmod +x "${BUNDLE_PATH}/Contents/MacOS/${EXECUTABLE_NAME}"
cp "${INFO_PLIST}" "${BUNDLE_PATH}/Contents/Info.plist"

MACOS_SIGNING_IDENTITY="${SOTTO_DEV_IDENTITY:--}" "${SCRIPT_DIR}/sign.sh" "${BUNDLE_PATH}"

echo
echo "bundled ${SOURCE_BINARY}"
echo "  ${BUNDLE_PATH}/Contents/MacOS/${EXECUTABLE_NAME}"
echo "  identifier ${BUNDLE_IDENTIFIER}"

if [[ "${SOTTO_DEV_IDENTITY:--}" == "-" ]]; then
  cat <<'WARNING'

Signed ad-hoc. An ad-hoc signature has no certificate, so TCC identifies the
app by its code hash — which changes on every rebuild. Expect to re-grant
Screen & System Audio Recording each time you rebuild and re-bundle.

To keep one grant across rebuilds, sign with a certificate whose identity is
stable. A self-signed code-signing certificate in the login keychain is enough:
create one in Keychain Access (Certificate Assistant > Create a Certificate,
type "Code Signing"), then re-run with SOTTO_DEV_IDENTITY set to its name.
WARNING
fi
