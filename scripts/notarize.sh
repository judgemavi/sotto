#!/bin/bash
set -euo pipefail

usage() {
  echo "usage: $0 <submission.zip-or-dmg> [staple-target]" >&2
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage
  exit 64
fi

readonly SUBMISSION_PATH="$1"
readonly STAPLE_TARGET="${2:-$1}"
: "${APPLE_API_KEY_ID:?APPLE_API_KEY_ID is required}"
: "${APPLE_API_ISSUER_ID:?APPLE_API_ISSUER_ID is required}"
: "${APPLE_API_PRIVATE_KEY_PATH:?APPLE_API_PRIVATE_KEY_PATH is required}"

for required_path in "${SUBMISSION_PATH}" "${STAPLE_TARGET}" "${APPLE_API_PRIVATE_KEY_PATH}"; do
  if [[ ! -e "${required_path}" ]]; then
    echo "required path does not exist: ${required_path}" >&2
    exit 66
  fi
done

readonly RESULT_PATH="$(mktemp "${TMPDIR:-/tmp}/sotto-notary-result.XXXXXX.json")"
cleanup() {
  rm -f "${RESULT_PATH}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

set +e
xcrun notarytool submit "${SUBMISSION_PATH}" \
  --key "${APPLE_API_PRIVATE_KEY_PATH}" \
  --key-id "${APPLE_API_KEY_ID}" \
  --issuer "${APPLE_API_ISSUER_ID}" \
  --wait \
  --output-format json > "${RESULT_PATH}"
readonly SUBMIT_STATUS=$?
set -e

cat "${RESULT_PATH}"
readonly SUBMISSION_ID="$(plutil -extract id raw -o - "${RESULT_PATH}" 2>/dev/null || true)"
readonly NOTARY_STATUS="$(plutil -extract status raw -o - "${RESULT_PATH}" 2>/dev/null || true)"

if [[ ${SUBMIT_STATUS} -ne 0 || "${NOTARY_STATUS}" != "Accepted" ]]; then
  echo "notarization failed with status: ${NOTARY_STATUS:-unknown}" >&2
  if [[ -n "${SUBMISSION_ID}" ]]; then
    xcrun notarytool log "${SUBMISSION_ID}" \
      --key "${APPLE_API_PRIVATE_KEY_PATH}" \
      --key-id "${APPLE_API_KEY_ID}" \
      --issuer "${APPLE_API_ISSUER_ID}" || true
  fi
  exit 1
fi

xcrun stapler staple "${STAPLE_TARGET}"
xcrun stapler validate "${STAPLE_TARGET}"
