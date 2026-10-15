//! herdr-dagr run contract v3 types, as pod writes them (spec section 12).
//! Field names and enums follow `CONTRACT.md` of herdr-dagr 0.3.1 (its
//! Schema v3 field reference section); `dagr check --strict` on the export
//! is the proof.

use serde::Serialize;

pub const CONTRACT_VERSION: u32 = 3;

#[derive(Serialize)]
pub struct Board {
    pub dagr: u32,
    pub generated_at: String,
    pub run: RunCard,
    pub projects: Vec<Project>,
    pub tasks: Vec<TaskCard>,
    pub events: Vec<EventCard>,
}

#[derive(Serialize)]
pub struct RunCard {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub orchestrator: Orchestrator,
}

/// Where dagr's operator messages go. pod records the coordinator's herdr
/// agent handle; its terminal id is not a pane id, so `pane` is omitted.
#[derive(Serialize)]
pub struct Orchestrator {
    pub agent: String,
}

#[derive(Serialize)]
pub struct Project {
    pub id: String,
    pub title: String,
}

#[derive(Serialize)]
pub struct TaskCard {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub state: &'static str,
    pub deps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unblock: Option<String>,
    /// A user gate's question (dagr renders a `question` task's `criteria`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criteria: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub attempts: Vec<Attempt>,
}

#[derive(Serialize)]
pub struct Attempt {
    pub id: String,
    pub n: u32,
    pub cause: Cause,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<Locator>,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<Liveness>,
}

#[derive(Serialize)]
pub struct Locator {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Serialize)]
pub struct Cause {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Serialize)]
pub struct Outcome {
    /// Always equal to the attempt's `state` (dagr E141).
    pub result: &'static str,
    pub evidence: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
}

#[derive(Serialize)]
pub struct Progress {
    pub note: String,
}

#[derive(Serialize)]
pub struct Liveness {
    pub prompt_acknowledged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_output_at: Option<String>,
    pub queued_input: u32,
}

#[derive(Serialize)]
pub struct EventCard {
    pub at: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    pub actor: String,
    pub detail: String,
}

/// Minimal task input for `sidebar_status`.
#[derive(Clone, Debug)]
pub struct TaskView {
    pub id: String,
    pub display_name: Option<String>,
    pub title: Option<String>,
}

/// Minimal dispatch input for `sidebar_status`.
#[derive(Clone, Debug)]
pub struct DispatchView {
    pub status: String,
    pub last_phase: Option<String>,
}

/// Task status to dagr state (spec section 12), before the attempt record
/// is taken into account (see `project_state`).
pub fn task_state(status: &str) -> &'static str {
    match status {
        "dispatched" => "working",
        "completed" => "done",
        "failed" => "failed",
        "blocked" => "blocked",
        _ => "queued",
    }
}

/// dagr holds a task's state to its attempt record (CONTRACT.md section 1,
/// E150). pod's Task status is the claim; the latest attempt decides where
/// the two disagree, so the board never shows a document dagr rejects:
/// a `working` task whose attempt was lost is `blocked` on the coordinator;
/// a `queued` task whose latest attempt is `done` is in `review`; a `done`
/// or `failed` task whose record disagrees gets a coordinator attempt.
pub fn project_state(status: &str, latest: Option<&str>, any_working: bool) -> &'static str {
    let claimed = task_state(status);
    match (claimed, latest) {
        ("blocked", _) => "blocked",
        ("working", Some("working")) => "working",
        ("working", _) => "blocked",
        // `export` appends a coordinator attempt when the record disagrees.
        ("done", _) => "done",
        ("failed", _) => "failed",
        ("queued", _) if any_working => "working",
        ("queued", Some("done")) => "review",
        _ => "queued",
    }
}

/// Whether a `done` or `failed` Task needs a coordinator attempt appended
/// so that its latest attempt matches (dagr E150): the Task was settled by
/// `task-update` or a gate, not by a worker's attempt.
pub fn needs_coordinator_attempt(state: &str, latest: Option<&str>) -> bool {
    match state {
        "done" => latest != Some("done"),
        "failed" => !matches!(latest, Some("failed" | "lost")),
        _ => false,
    }
}

/// pod event type to a dagr event type (open set in pod, closed in dagr).
pub fn event_kind(kind: &str, has_attempt: bool) -> &'static str {
    match kind {
        "dispatch.dispatched" if has_attempt => "attempt_started",
        "dispatch.completed"
        | "dispatch.failed"
        | "dispatch.start_failed"
        | "dispatch.stopped"
        | "dispatch.abandoned"
        | "dispatch.circuit_broken"
            if has_attempt =>
        {
            "attempt_settled"
        }
        "task.unblocked" | "task.promoted" => "promoted",
        _ => "note",
    }
}
