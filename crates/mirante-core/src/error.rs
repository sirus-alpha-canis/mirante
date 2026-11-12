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
    #[error("turn index {idx} already exists in session {session_id}")]
    DuplicateTurn { session_id: String, idx: i64 },
    #[error("conversation {conversation_id} does not belong to task {task_id}")]
    ConversationMismatch { conversation_id: String, task_id: String },
    #[error("session {0} has ended")]
    SessionEnded(String),
    #[error("database journal mode is {0:?}, expected \"wal\"")]
    JournalMode(String),
    #[error("worktree path {path} is outside the managed root {root}")]
    OutsideRoot { path: String, root: String },
    #[error("{}", dirty_message(path, changes, *more))]
    DirtyWorktree { path: String, changes: Vec<String>, more: usize },
    /// A workspace task's folder or repo selection that cannot be used (`crate::workspace`).
    #[error("{0}")]
    InvalidWorkspace(String),
    /// Archiving a workspace task with uncommitted work in some of its repos (or files left
    /// in the workspace folder itself): `(repo name, changes, more)` per dirty one.
    #[error("{}", dirty_workspace_message(dirty))]
    DirtyWorkspace { dirty: Vec<(String, Vec<String>, usize)> },
    #[error("claude binary not found: {0}")]
    ClaudeNotFound(String),
    /// A bounded queue is full (e.g. pending memory proposals).
    #[error("{0}")]
    Limit(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// The archive refusal for a worktree with uncommitted work: what changed (git's status
/// codes, at most ten paths) and what to do about it.
fn dirty_message(path: &str, changes: &[String], more: usize) -> String {
    let mut s = format!("the worktree {path} has uncommitted changes:");
    for c in changes {
        s.push_str("\n  ");
        s.push_str(c);
    }
    if more > 0 {
        s.push_str(&format!("\n  … and {more} more"));
    }
    s.push_str("\ncommit them, stash them (git stash -u) or discard them in the worktree, then archive again");
    s
}

/// The archive refusal for a workspace task: each dirty repo and what changed in it.
fn dirty_workspace_message(dirty: &[(String, Vec<String>, usize)]) -> String {
    let names: Vec<&str> = dirty.iter().map(|(n, _, _)| n.as_str()).collect();
    let mut s = format!("uncommitted changes in {}:", names.join(", "));
    for (name, changes, more) in dirty {
        s.push_str(&format!("\n{name}:"));
        for c in changes {
            s.push_str("\n  ");
            s.push_str(c);
        }
        if *more > 0 {
            s.push_str(&format!("\n  … and {more} more"));
        }
    }
    s.push_str("\ncommit them, stash them (git stash -u) or discard them in each worktree, then archive again; nothing was removed");
    s
}
