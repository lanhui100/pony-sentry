pub mod fingerprint;
pub mod parser;
pub mod project;
pub mod sanitizer;
pub mod validate;

pub use fingerprint::{compute_issue_fingerprint, normalize_environment};
pub use parser::{
    normalize_stacktrace, normalize_trace_id, parse_first_frame, Exception, Frame, RawEvent,
    Stacktrace, STACKTRACE_TEXT_MAX_CHARS,
};
pub use project::{infer_project_name, project_name_from_extra};
pub use sanitizer::{RedactionCounts, SanitizationPipeline};
pub use validate::{
    guard_extra, normalize_dimension, normalize_grafana_url, normalize_level, normalize_loki_query,
    normalize_source_kind, normalize_tags, parse_timestamp, truncate_chars, validate_event_id,
    FieldError, DEGRADED_LEVEL, MESSAGE_MAX_CHARS, VALID_LEVELS,
};
