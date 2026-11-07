use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },
    #[error("invalid {kind} transition: {from} -> {to}")]
    InvalidTransition { kind: &'static str, from: String, to: String },
    #[error("task {0} already has a live session")]
    TaskBusy(String),
    #[error("stale status for task {task_id}: expected {expected}, found {actual}")]
    StaleStatus { task_id: String, expected: String, actual: String },
    #[error("unknown {kind} value in database: {value}")]
    BadEnum { kind: &'static str, value: String },
    #[error("invalid branch name {name:?}: {reason}")]
    InvalidBranch { name: String, reason: String },
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("claude binary not found: {0}")]
    ClaudeNotFound(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
