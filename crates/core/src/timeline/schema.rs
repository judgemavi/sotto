/// SQLite schema contract implemented by the `rag` crate.
///
/// Durations are stored as integer nanoseconds and payloads as serde JSON. T008 must
/// enable `PRAGMA foreign_keys = ON` on every SQLite connection; SQLite disables
/// foreign-key enforcement by default and schema declaration alone is insufficient.
pub const SQLITE_SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE sessions (
    id TEXT PRIMARY KEY NOT NULL,
    started_at_unix_ms INTEGER NOT NULL,
    ended_at_unix_ms INTEGER,
    capture_target_bundle_id TEXT,
    capture_target_display_name TEXT NOT NULL,
    capture_target_window_title TEXT,
    capture_target_kind TEXT NOT NULL CHECK (
        capture_target_kind IN ('application', 'window')
    ),
    capture_target_audio_scoped INTEGER NOT NULL CHECK (
        capture_target_audio_scoped IN (0, 1)
    )
);

CREATE TABLE events (
    id INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL,
    supersedes INTEGER,
    payload TEXT NOT NULL,
    PRIMARY KEY (session_id, id),
    FOREIGN KEY (session_id) REFERENCES sessions(id),
    FOREIGN KEY (session_id, supersedes) REFERENCES events(session_id, id)
);

CREATE INDEX events_session_ts ON events(session_id, ts);
CREATE INDEX events_session_kind ON events(session_id, kind);
"#;
