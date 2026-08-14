#!/bin/sh
set -eu

if [ "${1:-}" = "--version" ]; then
  printf '%s\n' 'codex-cli 99.0.0-fake'
  exit 0
fi

if [ "${1:-}" = "login" ] && [ "${2:-}" = "status" ]; then
  printf '%s\n' 'Logged in using ChatGPT'
  exit 0
fi

args=" $* "
if env | grep -E '(^|_)(API_KEY|ACCESS_TOKEN|SECRET)=' >/dev/null 2>&1; then
  printf '%s\n' '{"type":"error","message":"secret environment leaked"}'
  exit 9
fi
required_flags='--json --ephemeral --ignore-user-config --ignore-rules --skip-git-repo-check'
for flag in $required_flags; do
  case "$args" in
    *" $flag "*) ;;
    *) printf '%s\n' "{\"type\":\"error\",\"message\":\"missing $flag\"}"; exit 9 ;;
  esac
done

case "$args" in
  *' --sandbox read-only '*) ;;
  *) printf '%s\n' '{"type":"error","message":"sandbox is not read-only"}'; exit 9 ;;
esac
case "$args" in
  *' -c web_search=disabled '*) ;;
  *) printf '%s\n' '{"type":"error","message":"web search was not disabled"}'; exit 9 ;;
esac
case "$args" in
  *' --disable code_mode_host '*)
    printf '%s\n' '{"type":"error","message":"code_mode_host must remain enabled"}'
    exit 9
    ;;
esac
case "$args" in
  *' --disable shell_tool '*) ;;
  *) printf '%s\n' '{"type":"error","message":"shell_tool was not disabled"}'; exit 9 ;;
esac
case "$args" in
  *' --disable unified_exec '*) ;;
  *) printf '%s\n' '{"type":"error","message":"unified_exec was not disabled"}'; exit 9 ;;
esac
case "$args" in
  *' --disable apps '*) ;;
  *) printf '%s\n' '{"type":"error","message":"apps were not disabled"}'; exit 9 ;;
esac
case "$args" in
  *' --disable browser_use '*) ;;
  *) printf '%s\n' '{"type":"error","message":"browser_use was not disabled"}'; exit 9 ;;
esac
case "$args" in
  *' --disable computer_use '*) ;;
  *) printf '%s\n' '{"type":"error","message":"computer_use was not disabled"}'; exit 9 ;;
esac
case "$args" in
  *' --disable hooks '*) ;;
  *) printf '%s\n' '{"type":"error","message":"hooks were not disabled"}'; exit 9 ;;
esac
case "$args" in
  *' --disable plugins '*) ;;
  *) printf '%s\n' '{"type":"error","message":"plugins were not disabled"}'; exit 9 ;;
esac

model=''
previous=''
for argument in "$@"; do
  if [ "$previous" = '--model' ]; then
    model=$argument
  fi
  previous=$argument
done

case "$model" in
  block-stdin:*)
    marker=${model#block-stdin:}
    printf '%s\n' started > "$marker.started"
    (sleep 1; printf '%s\n' survived > "$marker.survived") &
    sleep 30
    exit 0
    ;;
  prewrite-stderr)
    index=0
    while [ "$index" -lt 12000 ]; do
      printf '%s\n' 'diagnostic padding before stdin' >&2
      index=$((index + 1))
    done
    prompt=$(cat)
    printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"drained"}}'
    printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}'
    exit 0
    ;;
esac

prompt=$(cat)
case "$args" in
  *CASE_*) printf '%s\n' '{"type":"error","message":"prompt leaked into argv"}'; exit 9 ;;
esac

case "$prompt" in
  *CASE_NORMAL*)
    printf '%s\n' '{"type":"thread.started","thread_id":"fake-thread"}'
    printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"{\"topics\":["}}'
    printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"\"pricing\"]}"}}'
    printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":4}}'
    ;;
  *CASE_MALFORMED*)
    printf '%s\n' 'not-json'
    sleep 10
    ;;
  *CASE_OVERSIZED_NO_NEWLINE*)
    dd if=/dev/zero bs=1048577 count=1 2>/dev/null | tr '\000' 'x'
    sleep 10
    ;;
  *CASE_AUTH*)
    printf '%s\n' '{"type":"error","message":"Login required"}'
    exit 1
    ;;
  *CASE_FORBIDDEN_TOOL*)
    printf '%s\n' '{"type":"item.started","item":{"type":"command_execution","command":"cat secret"}}'
    sleep 10
    ;;
  *CASE_UNKNOWN_TOPLEVEL*)
    printf '%s\n' '{"type":"future.event","payload":{"safe":true}}'
    sleep 10
    ;;
  *CASE_UNKNOWN_ITEM*)
    printf '%s\n' '{"type":"item.started","item":{"type":"future_tool","arguments":"ignored"}}'
    sleep 10
    ;;
  *CASE_TRUNCATED*)
    printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"partial without terminal"}}'
    exit 0
    ;;
  *CASE_STDERR_AUTH_FLOOD*)
    printf '%s\n' 'Login required' >&2
    index=0
    while [ "$index" -lt 12000 ]; do
      printf '%s\n' 'diagnostic padding after auth' >&2
      index=$((index + 1))
    done
    exit 1
    ;;
  *CASE_CANCEL*)
    marker=$(printf '%s\n' "$prompt" | sed -n 's/^CASE_CANCEL_MARKER=//p' | head -n 1)
    (sleep 2; printf '%s\n' survived > "$marker") &
    printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"partial"}}'
    sleep 10
    ;;
  *)
    printf '%s\n' '{"type":"error","message":"unknown fake case"}'
    exit 8
    ;;
esac
