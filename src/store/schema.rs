use rusqlite::Transaction;

use crate::error::Result;

pub const LATEST_VERSION: u32 = 3;

pub fn migrate(transaction: &Transaction<'_>, current: u32) -> Result<()> {
    if current < 1 {
        transaction.execute_batch(MIGRATION_1)?;
        transaction.pragma_update(None, "user_version", 1)?;
    }
    if current < 2 {
        transaction.execute_batch(MIGRATION_2)?;
        transaction.pragma_update(None, "user_version", 2)?;
    }
    if current < 3 {
        transaction.execute_batch(MIGRATION_3)?;
        transaction.pragma_update(None, "user_version", 3)?;
    }
    Ok(())
}

const MIGRATION_1: &str = r#"
CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    objective TEXT NOT NULL,
    coordinator_terminal_id TEXT NOT NULL,
    coordinator_handle TEXT NOT NULL,
    consumer_generation INTEGER NOT NULL,
    home_cwd TEXT NOT NULL,
    depth INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    parent_id TEXT REFERENCES tasks(id),
    title TEXT,
    display_name TEXT,
    spec TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','ready','dispatched','completed','failed','blocked')),
    deps TEXT NOT NULL,
    result TEXT,
    created_by_terminal_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE TABLE dispatches (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('pending','dispatched','completed','failed','circuit_broken')),
    retry_of_dispatch_id TEXT REFERENCES dispatches(id),
    failure_count INTEGER NOT NULL,
    last_failure TEXT,
    termination_reason TEXT CHECK (termination_reason IS NULL OR termination_reason IN ('no_follow_on','handed_off','blocked_on','denied','canceled','escalated','superseded')),
    termination_target TEXT,
    depth INTEGER NOT NULL,
    assignee_handle TEXT,
    assignee_terminal_id TEXT,
    assignee_pid INTEGER,
    agent_kind TEXT,
    launch_requested TEXT,
    launch_effective TEXT,
    dispatched_at TEXT,
    last_heartbeat_at TEXT,
    last_phase TEXT,
    completed_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE workers (
    dispatch_id TEXT PRIMARY KEY REFERENCES dispatches(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('starting','ready','start_unknown','failed','succeeded','stopping','stop_unknown','stopped','abandoned')),
    stage TEXT,
    placement TEXT,
    worktree_path TEXT,
    workspace_id TEXT,
    tab_id TEXT,
    pane_id TEXT,
    terminal_id TEXT,
    owns_pane INTEGER NOT NULL DEFAULT 0 CHECK (owns_pane IN (0,1)),
    setup_state TEXT,
    setup_pane_id TEXT,
    preamble_input TEXT CHECK (preamble_input IS NULL OR preamble_input IN ('delivered','rendered_unconfirmed','skipped','failed')),
    effects TEXT,
    residual_resources TEXT,
    terminal_state TEXT NOT NULL CHECK (terminal_state IN ('active','reclaimable','retained','release_pending','release_unknown','released')),
    retained_reason TEXT,
    session_id TEXT,
    transcript_path TEXT,
    transcript_proof TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE worker_archives (
    dispatch_id TEXT NOT NULL REFERENCES dispatches(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (dispatch_id, kind)
);

CREATE TABLE observations (
    dispatch_id TEXT NOT NULL REFERENCES dispatches(id) ON DELETE CASCADE,
    liveness TEXT NOT NULL CHECK (liveness IN ('live','unverifiable','exited')),
    activity TEXT NOT NULL CHECK (activity IN ('working','idle_at_prompt','unknown')),
    presence TEXT CHECK (presence IS NULL OR presence IN ('present','exited','absent')),
    needs_input TEXT NOT NULL,
    evidence_source TEXT NOT NULL CHECK (evidence_source IN ('self_report','herdr_state')),
    reason TEXT,
    herdr_status TEXT,
    attention TEXT,
    observed_at TEXT NOT NULL
);

CREATE TABLE messages (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    mailbox TEXT NOT NULL,
    from_handle TEXT NOT NULL,
    subject TEXT NOT NULL,
    body TEXT,
    type TEXT NOT NULL CHECK (type IN ('worker_done','heartbeat','escalation','decision_gate','status','merge_ready','handoff','dispatch','question')),
    priority TEXT NOT NULL CHECK (priority IN ('normal','high','urgent')),
    thread_id TEXT,
    payload TEXT,
    read INTEGER NOT NULL DEFAULT 0 CHECK (read IN (0,1)),
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    sender_terminal_id TEXT,
    rejected_reason TEXT,
    evidence TEXT,
    recommendation TEXT
);

CREATE TABLE deliveries (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    mailbox TEXT NOT NULL,
    consumer_generation INTEGER NOT NULL,
    message_ids TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('outstanding','acknowledged','fenced')),
    created_at TEXT NOT NULL,
    acknowledged_at TEXT
);

CREATE TABLE wake_attempts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    mailbox TEXT NOT NULL,
    delivery_id TEXT REFERENCES deliveries(id),
    attempt INTEGER NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('delivered','rendered_unconfirmed','skipped','failed')),
    detail TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE questions (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    dispatch_id TEXT REFERENCES dispatches(id),
    asker_handle TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','answered','closed')),
    answer_message_id TEXT REFERENCES messages(id),
    answer_body TEXT,
    created_at TEXT NOT NULL,
    answered_at TEXT,
    closed_at TEXT
);

CREATE TABLE gates (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    task_id TEXT REFERENCES tasks(id),
    audience TEXT NOT NULL CHECK (audience IN ('coordinator','user')),
    question TEXT NOT NULL,
    options TEXT,
    evidence TEXT,
    recommendation TEXT,
    status TEXT NOT NULL CHECK (status IN ('pending','resolved')),
    resolution TEXT,
    created_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE TABLE receipts (
    caller_fingerprint TEXT NOT NULL,
    request_id TEXT NOT NULL,
    method TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','completed')),
    receipt TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (caller_fingerprint, request_id)
);

CREATE TABLE events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    at TEXT NOT NULL,
    type TEXT NOT NULL,
    task_id TEXT REFERENCES tasks(id),
    dispatch_id TEXT REFERENCES dispatches(id),
    actor TEXT NOT NULL,
    detail TEXT
);

CREATE UNIQUE INDEX dispatches_one_active_per_task
    ON dispatches(task_id) WHERE status IN ('pending','dispatched');
CREATE UNIQUE INDEX dispatches_one_active_per_terminal
    ON dispatches(assignee_terminal_id) WHERE status IN ('pending','dispatched');
CREATE UNIQUE INDEX deliveries_one_outstanding_per_mailbox
    ON deliveries(mailbox) WHERE status = 'outstanding';
CREATE INDEX tasks_by_run_status ON tasks(run_id, status);
CREATE INDEX dispatches_by_run_created ON dispatches(run_id, created_at);
CREATE INDEX messages_by_mailbox_sequence ON messages(mailbox, sequence);
CREATE INDEX wake_attempts_by_mailbox_attempt ON wake_attempts(mailbox, attempt);
CREATE INDEX events_by_run_sequence ON events(run_id, sequence);
"#;

const MIGRATION_2: &str = r#"
ALTER TABLE workers ADD COLUMN user_owned INTEGER NOT NULL DEFAULT 0 CHECK (user_owned IN (0,1));
ALTER TABLE workers ADD COLUMN user_owned_reason TEXT;
"#;

const MIGRATION_3: &str = "ALTER TABLE runs ADD COLUMN budget_usd REAL;";
