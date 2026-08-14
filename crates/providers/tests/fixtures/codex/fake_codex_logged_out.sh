#!/bin/sh
set -eu

if [ "${1:-}" = "--version" ]; then
  printf '%s\n' 'codex-cli 99.0.0-fake'
  exit 0
fi

if [ "${1:-}" = "login" ] && [ "${2:-}" = "status" ]; then
  printf '%s\n' 'Not logged in'
  exit 1
fi

exit 2
