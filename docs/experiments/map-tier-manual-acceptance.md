# Map-tier signed-app manual acceptance

This is the evidence template and runbook for T035. **No scenario has been run yet.** Keep every
row `NOT RUN` until a human performs it against the identified signed and notarized app. Do not
substitute mocked capture, unit tests, or an ad-hoc-signed development bundle for an observation.

## Evidence header

| Field | Recorded value |
| --- | --- |
| Observer and date | NOT RUN |
| Git commit | NOT RUN |
| App artifact path / release URL | NOT RUN |
| App SHA-256 | NOT RUN |
| Signing identity | NOT RUN |
| Notarization / stapling result | NOT RUN |
| macOS version and hardware | NOT RUN |
| Microphone and selected target | NOT RUN |
| Actual audio scope stated by app | NOT RUN |
| Managed model path / size / SHA-256 | NOT RUN |
| Network-state evidence | NOT RUN |
| Reasoning selection / Codex / OpenAI state | NOT RUN |
| Redaction applied | NOT RUN |

Use a release-workflow artifact that contains the commit under test. The local
`scripts/dev-bundle.sh` output is useful for development but does not satisfy T035 when its
signature is ad hoc or it is not notarized.

```sh
set -eu
set -C
export T035_APP="/absolute/path/to/Sotto.app"
export T035_BIN="$T035_APP/Contents/MacOS/sotto"
export T035_RUN_DIR="$(mktemp -d /private/tmp/sotto-t035-run.XXXXXX)"
export T035_DB="$T035_RUN_DIR/sotto.sqlite3"
export T035_LOG="$T035_RUN_DIR/sotto.log"
export T035_SAFE_PATH="/usr/bin:/bin:/usr/sbin:/sbin"

require_absolute() {
  case "$2" in
    /*) ;;
    *) echo "FAIL: $1 must be an absolute path: $2" >&2; return 1 ;;
  esac
}
move_no_clobber() {
  test -e "$1" || { echo "FAIL: move source is absent: $1" >&2; return 1; }
  test ! -e "$2" || { echo "FAIL: refusing to overwrite: $2" >&2; return 1; }
  mv -n "$1" "$2"
  test ! -e "$1" || { echo "FAIL: no-clobber move did not consume: $1" >&2; return 1; }
  test -e "$2" || { echo "FAIL: no-clobber move did not create: $2" >&2; return 1; }
}
move_if_present_no_clobber() {
  test ! -e "$1" || move_no_clobber "$1" "$2"
}

require_absolute T035_APP "$T035_APP"
require_absolute T035_BIN "$T035_BIN"
require_absolute T035_RUN_DIR "$T035_RUN_DIR"
require_absolute T035_DB "$T035_DB"
require_absolute T035_LOG "$T035_LOG"
test -d "$T035_APP"
test -x "$T035_BIN"
test -d "$T035_RUN_DIR"

git rev-parse HEAD
codesign --verify --deep --strict --verbose=2 "$T035_APP"
codesign -dvvv "$T035_APP" 2>&1
spctl --assess --type execute --verbose=4 "$T035_APP"
xcrun stapler validate "$T035_APP"
shasum -a 256 "$T035_BIN"
sw_vers
system_profiler SPHardwareDataType
```

Record the output or an exact artifact path beside the header. Participant content, raw audio,
unredacted frames, API keys, and credentials do not belong in this document.

## Safe model-cache preparation

The managed default is
`$HOME/Library/Application Support/Sotto/models/ggml-base.en.bin`: 147,964,211 bytes with SHA-256
`a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002`. Its resumable and quarantine
siblings are `ggml-base.en.bin.partial` and `ggml-base.en.bin.corrupt`. Preserve existing files by
moving them to a dedicated backup directory; do not delete them.

```sh
export T035_MODEL_DIR="$HOME/Library/Application Support/Sotto/models"
export T035_MODEL="$T035_MODEL_DIR/ggml-base.en.bin"
export T035_PARTIAL="$T035_MODEL.partial"
export T035_CORRUPT="$T035_MODEL.corrupt"
export T035_MODEL_BACKUP="$(mktemp -d /private/tmp/sotto-t035-model-backup.XXXXXX)"
export T035_ORIGINAL_MODEL="$T035_MODEL_BACKUP/original-ggml-base.en.bin"
export T035_ORIGINAL_PARTIAL="$T035_MODEL_BACKUP/original-ggml-base.en.bin.partial"
export T035_ORIGINAL_CORRUPT="$T035_MODEL_BACKUP/original-ggml-base.en.bin.corrupt"

require_absolute T035_MODEL_DIR "$T035_MODEL_DIR"
require_absolute T035_MODEL_BACKUP "$T035_MODEL_BACKUP"
test -d "$T035_MODEL_BACKUP"
mkdir -p "$T035_MODEL_DIR"
ls -la "$T035_MODEL_DIR"
move_if_present_no_clobber "$T035_MODEL" "$T035_ORIGINAL_MODEL"
move_if_present_no_clobber "$T035_PARTIAL" "$T035_ORIGINAL_PARTIAL"
move_if_present_no_clobber "$T035_CORRUPT" "$T035_ORIGINAL_CORRUPT"
```

Keep `SOTTO_WHISPER_MODEL` unset during every managed-model scenario. Start each scenario with a
new database in the run directory. The app is launched from inside the bundle so the process logs
and PID remain observable while bundle identity is retained.

```sh
env -u SOTTO_WHISPER_MODEL \
  PATH="$T035_SAFE_PATH" \
  SOTTO_DATABASE="$T035_DB" \
  "$T035_BIN" >"$T035_LOG" 2>&1 &
export T035_PID=$!
tail -F "$T035_LOG"
```

Stop `tail` with Control-C; it does not stop Sotto. Use Sotto's visible Stop control for the user
Stop scenario. Do not use `kill` as evidence for any terminal path.

## Scenario record

For every row, include wall-clock start/end, session id if one exists, exact user action, expected
result, observed result, and evidence path. `PASS`, `FAIL`, and `NOT RUN` are the only statuses.

| ID | Scenario | Status | Session / timestamp | Evidence and observation |
| --- | --- | --- | --- | --- |
| M01 | Cold launch stays idle until Start | NOT RUN | — | No picker, capture, model provisioning, session resume, Codex process, or OpenAI request |
| M02 | Start then cancel system picker | NOT RUN | — | Returns idle; no session, capture, download, or automatic retry |
| M03 | No-cache download; cancel once; resume; verify integrity | NOT RUN | — | Progress, retained `.partial`, resumed progress, final size/digest |
| M04 | Corrupt final is quarantined and replacement is attempted | NOT RUN | — | `.corrupt` evidence; if offline, `OfflineNoCache`, not a fictional corrupt-cache error |
| M05 | Verified cache is reused while offline | NOT RUN | — | No download; successful transcription from cached model |
| M06 | Real mic plus selected-target audio; partial/final transcript | NOT RUN | — | Target and actual audio scope; timestamped redacted examples; You/Meeting audio attribution |
| M07 | User Stop terminal path | NOT RUN | — | One terminal transition; indicator clears; idle; no restart |
| M08 | Selected target closes | NOT RUN | — | One terminal transition; indicator clears; idle; no restart |
| M09 | macOS Stop Sharing | NOT RUN | — | One terminal transition; indicator clears; idle; no restart |
| M10 | Safe failure path | NOT RUN | — | State, actionable error, indicator clearing, no restart; describe how induced |
| M11 | Target/end/tail persist and reload | NOT RUN | — | CaptureTarget, non-null end, last live and persisted event ids/kinds |
| M12 | Live and post-call transcript readability | NOT RUN | — | Complete rubric below and give a subjective verdict |
| M13 | Ten-minute no-key/no-Codex/OpenAI session | NOT RUN | — | Start through persisted review with no reasoning error state |
| M14 | CPU/RSS/transcript observations at idle, 1, and 10 minutes | NOT RUN | — | Complete measurement table and Follow live observation |

### M01-M05: cold start and managed model

1. Launch the signed app before pressing Start. Observe for at least 30 seconds and record UI,
   process, filesystem, and log state. Press Start and cancel the system picker for M02.
2. With all three managed model files safely absent, Start again, choose a real target, and cancel
   from Sotto while download is progressing. Record the `.partial` size. Start again and record
   that progress resumes rather than restarting at zero. After completion:

   ```sh
   stat -f '%N %z bytes' "$T035_MODEL" "$T035_PARTIAL" 2>/dev/null
   shasum -a 256 "$T035_MODEL"
   ```

3. Preserve the verified final, then install an intentionally invalid final and Start. First
   record and verify the exact expected size/digest, then move it without overwrite. Corrupt
   injection is guarded by the app being stopped, the successful move, and an absent destination:

   ```sh
   if ps -p "$T035_PID" >/dev/null 2>&1; then
     echo 'FAIL: quit Sotto normally before modifying the managed model cache' >&2
     false
   fi
   export T035_VERIFIED_MODEL="$T035_MODEL_BACKUP/acceptance-ggml-base.en.bin.verified"
   export T035_VERIFIED_SIZE="$(stat -f '%z' "$T035_MODEL")"
   export T035_VERIFIED_HASH_LINE="$(shasum -a 256 "$T035_MODEL")"
   export T035_VERIFIED_SHA256="${T035_VERIFIED_HASH_LINE%% *}"
   test "$T035_VERIFIED_SIZE" = "147964211"
   test "$T035_VERIFIED_SHA256" = "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
   move_no_clobber "$T035_MODEL" "$T035_VERIFIED_MODEL"
   test -e "$T035_VERIFIED_MODEL"
   test ! -e "$T035_MODEL"
   umask 077
   printf 'T035 deliberate corrupt model\n' >"$T035_MODEL"
   shasum -a 256 "$T035_MODEL"
   ```

   Online, record quarantine plus replacement. Repeat corrupt-and-offline only if needed to observe
   reachable error semantics: the expected error is `OfflineNoCache`, with quarantine evidence if
   available. Restore the verified final before M05:

   ```sh
   export T035_FAILED_REPLACEMENT_DIR="$(mktemp -d "$T035_MODEL_BACKUP/failed-replacement.XXXXXX")"
   move_if_present_no_clobber "$T035_MODEL" \
     "$T035_FAILED_REPLACEMENT_DIR/ggml-base.en.bin"
   move_no_clobber "$T035_VERIFIED_MODEL" "$T035_MODEL"
   stat -f '%N %z bytes' "$T035_MODEL"
   shasum -a 256 "$T035_MODEL"
   ```

4. Disconnect networking with the normal macOS control and record how the offline state was
   established. Relaunch and complete a short captured transcription using the verified cache.
   Restore networking after the observation. Do not infer offline reuse from a mocked HTTP server.

### M06-M13: real scoped session and terminal paths

Use a real two-party target for at least ten minutes. The selected window/application name and the
app's stated audio scope are separate fields: never label display/system-wide audio as target-app
audio. Verify live partial text before its final replacement and retain only short redacted examples.

For the full Running interval, observe that the non-disableable indicator names the screen target
and distinguishes microphone from the actual captured-audio scope. Exercise M07-M10 in separate
sessions. M10 may use a safe, reversible permission or device-loss failure; record the method and
restore the system afterward. Each path must terminate once, clear the indicator, return to idle or
an actionable error, and never auto-restart.

The no-key session requires all three conditions: `No reasoning` selected, no OpenAI key configured
for Sotto, and no Codex executable usable by the app. The restricted launch `PATH` above excludes
common user-installed CLI locations. Verify both absence conditions without printing credentials:

```sh
if PATH="$T035_SAFE_PATH" command -v codex >/dev/null 2>&1; then
  echo 'FAIL: Codex is usable on the acceptance PATH'
else
  echo 'PASS: Codex is not usable on the acceptance PATH'
fi

if security find-generic-password -s dev.sotto.llm -a openai >/dev/null 2>&1; then
  echo 'FAIL: Sotto OpenAI Keychain entry exists'
else
  echo 'PASS: no Sotto OpenAI Keychain entry exists'
fi
```

Also capture the Settings view showing `No reasoning`; do not expose a credential or use
`security ... -w`. If the Keychain entry exists, use a clean acceptance account or the app's
explicit remove-key control and record that user action—do not delete it silently in this runbook.
Sotto must still complete Start, transcription, timeline, Stop, persistence, and review.

Inspect the durable record after each completed session:

```sh
sqlite3 -header -box "$T035_DB" \
  'SELECT id, started_at_unix_ms, ended_at_unix_ms, capture_target_bundle_id, capture_target_display_name, capture_target_window_title, capture_target_kind, capture_target_audio_scoped FROM sessions ORDER BY started_at_unix_ms;'
sqlite3 -header -box "$T035_DB" \
  'SELECT session_id, id, ts, kind, supersedes FROM events ORDER BY session_id, ts, id;'
sqlite3 -header -box "$T035_DB" \
  'SELECT session_id, COUNT(*) AS event_count, MAX(id) AS tail_id, MAX(ts) AS tail_ts FROM events GROUP BY session_id ORDER BY session_id;'
```

Record live last event id/kind, persisted last event id/kind, and whether the final partial/final and
terminal tail were present. The database stores target and end time; terminal outcome is shown in
the app completion state and must be correlated by session and timestamp rather than invented as a
database column.

## Transcript readability rubric

Complete once live and once after persisted replay. Add defect identifiers and severity for every
`NO`; do not average the answers into a pass.

| Observation | Live YES/NO | Replay YES/NO | Evidence / defect |
| --- | --- | --- | --- |
| You / Meeting audio labels match the heard speakers | NOT RUN | NOT RUN | — |
| Timestamps and rows remain in chronological order | NOT RUN | NOT RUN | — |
| Rolling partial text replaces one live row instead of duplicating rows | NOT RUN | NOT RUN | — |
| Long utterances wrap and remain readable | NOT RUN | NOT RUN | — |
| Follow live tracks new content and yields on manual scroll | NOT RUN | NOT RUN | — |
| One obvious action restores Follow live | NOT RUN | NOT RUN | — |
| Persisted replay matches the settled live transcript | NOT RUN | NOT RUN | — |
| A note citation focuses the exact transcript evidence row | NOT RUN | NOT RUN | — |
| Ten minutes remains calm and readable | NOT RUN | NOT RUN | — |

**Required subjective verdict:** NOT RUN — state whether this transcript-and-notes view is genuinely
useful as a standalone meeting artifact and why.

## Process and transcript measurements

At idle and near every checkpoint, append a process sample and record the visible transcript row
count plus whether Follow live remained responsive:

```sh
date -u '+%Y-%m-%dT%H:%M:%SZ' >>"$T035_RUN_DIR/process-samples.txt"
ps -p "$T035_PID" -o pid=,etime=,%cpu=,rss=,command= >>"$T035_RUN_DIR/process-samples.txt"
export T035_SAMPLE_TAG="$(date -u '+%Y%m%dT%H%M%SZ')"
```

RSS is reported by `ps` in KiB on macOS. Take repeated `ps` samples across an idle interval and an
active append interval; a single instantaneous percentage is not both measurements.

| Checkpoint | Transcript state | Visible rows | Follow live | CPU % | RSS KiB | Evidence |
| --- | --- | ---: | --- | ---: | ---: | --- |
| Idle before Start | idle | 0 | N/A | NOT RUN | NOT RUN | — |
| 1 minute | active append | NOT RUN | NOT RUN | NOT RUN | NOT RUN | — |
| 10 minutes | active append | NOT RUN | NOT RUN | NOT RUN | NOT RUN | — |

## Final verdict and restoration

**T035 verdict:** NOT RUN

Required failures / follow-up implementation tasks: NOT RUN

T048 transcript evidence this run can support: NONE YET

After all model scenarios, first preserve acceptance-generated cache artifacts in a unique run
subdirectory. Then restore each pre-existing artifact with the same no-clobber helper. Any
destination collision stops the shell and must be reconciled manually; never weaken the helper or
delete a colliding file.

```sh
export T035_ACCEPTANCE_CACHE="$(mktemp -d "$T035_RUN_DIR/acceptance-cache.XXXXXX")"
move_if_present_no_clobber "$T035_MODEL" \
  "$T035_ACCEPTANCE_CACHE/ggml-base.en.bin"
move_if_present_no_clobber "$T035_PARTIAL" \
  "$T035_ACCEPTANCE_CACHE/ggml-base.en.bin.partial"
move_if_present_no_clobber "$T035_CORRUPT" \
  "$T035_ACCEPTANCE_CACHE/ggml-base.en.bin.corrupt"
move_if_present_no_clobber "$T035_ORIGINAL_MODEL" "$T035_MODEL"
move_if_present_no_clobber "$T035_ORIGINAL_PARTIAL" "$T035_PARTIAL"
move_if_present_no_clobber "$T035_ORIGINAL_CORRUPT" "$T035_CORRUPT"
```

Keep the run and backup directories until evidence has been transcribed and redacted. They may
contain model artifacts and private logs; do not attach either directory or raw contents to the
task.
