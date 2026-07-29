#!/bin/bash
set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly ENTITLEMENTS_PATH="${SCRIPT_DIR}/entitlements.plist"

usage() {
  echo "usage: MACOS_SIGNING_IDENTITY=<identity|-> $0 <binary-or-app>" >&2
}

if [[ $# -ne 1 ]]; then
  usage
  exit 64
fi

readonly SIGN_TARGET="$1"
: "${MACOS_SIGNING_IDENTITY:?MACOS_SIGNING_IDENTITY must be a Developer ID identity or - for local ad-hoc signing}"

if [[ ! -e "${SIGN_TARGET}" ]]; then
  echo "signing target does not exist: ${SIGN_TARGET}" >&2
  exit 66
fi

if [[ ! -f "${ENTITLEMENTS_PATH}" ]]; then
  echo "entitlements file does not exist: ${ENTITLEMENTS_PATH}" >&2
  exit 66
fi

TEMP_KEYCHAIN=""
TEMP_DIRECTORY=""
ORIGINAL_KEYCHAINS=()
cleanup() {
  if [[ ${#ORIGINAL_KEYCHAINS[@]} -gt 0 ]]; then
    security list-keychains -d user -s "${ORIGINAL_KEYCHAINS[@]}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${TEMP_KEYCHAIN}" ]]; then
    security delete-keychain "${TEMP_KEYCHAIN}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${TEMP_DIRECTORY}" ]]; then
    rm -f "${TEMP_DIRECTORY}/certificate.p12"
    rmdir "${TEMP_DIRECTORY}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [[ "${MACOS_SIGNING_IDENTITY}" != "-" ]]; then
  : "${MACOS_CERTIFICATE_BASE64:?MACOS_CERTIFICATE_BASE64 is required for Developer ID signing}"
  : "${MACOS_CERTIFICATE_PASSWORD:?MACOS_CERTIFICATE_PASSWORD is required for Developer ID signing}"
  : "${MACOS_KEYCHAIN_PASSWORD:?MACOS_KEYCHAIN_PASSWORD is required for Developer ID signing}"

  TEMP_DIRECTORY="$(mktemp -d "${TMPDIR:-/tmp}/sotto-signing.XXXXXX")"
  readonly CERTIFICATE_PATH="${TEMP_DIRECTORY}/certificate.p12"
  TEMP_KEYCHAIN="${TEMP_DIRECTORY}/signing.keychain-db"

  printf '%s' "${MACOS_CERTIFICATE_BASE64}" | base64 -D > "${CERTIFICATE_PATH}"
  security create-keychain -p "${MACOS_KEYCHAIN_PASSWORD}" "${TEMP_KEYCHAIN}"
  security set-keychain-settings -lut 21600 "${TEMP_KEYCHAIN}"
  security unlock-keychain -p "${MACOS_KEYCHAIN_PASSWORD}" "${TEMP_KEYCHAIN}"
  security import "${CERTIFICATE_PATH}" \
    -k "${TEMP_KEYCHAIN}" \
    -P "${MACOS_CERTIFICATE_PASSWORD}" \
    -T /usr/bin/codesign \
    -T /usr/bin/security
  security set-key-partition-list \
    -S apple-tool:,apple:,codesign: \
    -s \
    -k "${MACOS_KEYCHAIN_PASSWORD}" \
    "${TEMP_KEYCHAIN}"

  while IFS= read -r keychain; do
    keychain="${keychain#*\"}"
    keychain="${keychain%\"*}"
    ORIGINAL_KEYCHAINS+=("${keychain}")
  done < <(security list-keychains -d user)
  security list-keychains -d user -s "${TEMP_KEYCHAIN}" "${ORIGINAL_KEYCHAINS[@]}"
fi

codesign_args=(
  --force
  --deep
  --options runtime
  --entitlements "${ENTITLEMENTS_PATH}"
  --sign "${MACOS_SIGNING_IDENTITY}"
)
if [[ "${MACOS_SIGNING_IDENTITY}" != "-" ]]; then
  codesign_args+=(--timestamp)
fi

codesign "${codesign_args[@]}" "${SIGN_TARGET}"
codesign --verify --deep --strict --verbose=2 "${SIGN_TARGET}"
