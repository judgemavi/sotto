//! SeaORM rows for Sotto's durable SQLite tables.
//!
//! These types describe storage, not domain invariants. Conversion back to `core` types remains
//! explicit, especially for capture scope and the recording state machine.

macro_rules! entity_without_relations {
    ($module:ident, $table:literal, { $($field:tt)* }) => {
        pub(crate) mod $module {
            use sea_orm::entity::prelude::*;

            #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
            #[sea_orm(table_name = $table)]
            pub(crate) struct Model {
                $($field)*
            }

            #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
            pub(crate) enum Relation {}

            impl ActiveModelBehavior for ActiveModel {}
        }
    };
}

entity_without_relations!(sessions, "sessions", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub started_at_unix_ms: i64,
    pub ended_at_unix_ms: Option<i64>,
    pub capture_target_bundle_id: Option<String>,
    pub capture_target_display_name: String,
    pub capture_target_window_title: Option<String>,
    pub capture_target_kind: String,
    pub capture_target_audio_scoped: i32,
});

entity_without_relations!(events, "events", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    pub ts: i64,
    pub kind: String,
    pub supersedes: Option<i64>,
    pub payload: String,
});

entity_without_relations!(entries, "entries", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub created_at_unix_ms: i64,
    pub title: Option<String>,
});

entity_without_relations!(entry_sessions, "entry_sessions", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    pub entry_id: String,
});

entity_without_relations!(documents, "documents", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub kind: String,
    pub title: String,
    pub source_path: Option<String>,
    pub collection_id: Option<String>,
    pub source_session_id: Option<String>,
    pub provenance_status: String,
    pub ingested_at: i64,
    pub content_hash: String,
});

entity_without_relations!(chunks, "chunks", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub doc_id: String,
    pub ordinal: i64,
    pub text: String,
    pub token_count: i64,
    pub metadata: String,
    pub embedding: Vec<u8>,
});

entity_without_relations!(derived_views, "derived_views", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub model: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub content_hash: String,
    pub artifact: String,
    pub usage: String,
    pub created_at: i64,
});

entity_without_relations!(grounded_derived_views, "grounded_derived_views", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub model: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub content_hash: String,
    pub artifact: String,
    pub usage: String,
    pub provider_model: String,
    pub grant_fingerprint: Option<String>,
    pub source_status: String,
    pub bundle: String,
    pub normalizations: String,
    pub created_at: i64,
});

pub(crate) mod session_recordings {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq, EnumIter, DeriveActiveEnum)]
    #[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
    pub(crate) enum RecordingState {
        #[sea_orm(string_value = "growing")]
        Growing,
        #[sea_orm(string_value = "available")]
        Available,
        #[sea_orm(string_value = "deleted")]
        Deleted,
        #[sea_orm(string_value = "pruned")]
        Pruned,
    }

    #[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "session_recordings")]
    pub(crate) struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub session_id: String,
        pub state: RecordingState,
        pub path: Option<String>,
        pub container: Option<String>,
        pub duration_ns: Option<i64>,
        pub byte_size: Option<i64>,
        pub session_origin_ns: Option<i64>,
        pub media_origin_ns: Option<i64>,
        pub rate_numerator: Option<i32>,
        pub rate_denominator: Option<i32>,
        pub finalization_error: Option<String>,
        pub updated_at_unix_ms: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub(crate) enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

entity_without_relations!(recording_retention_settings, "recording_retention_settings", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub singleton: i32,
    pub budget_bytes: i64,
});

entity_without_relations!(session_retranscriptions, "session_retranscriptions", {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    pub model: String,
    pub utterances: String,
    pub updated_at_unix_ms: i64,
});
