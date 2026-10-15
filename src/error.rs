use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    TaskNotFound,
    TaskNotStartable,
    NestedWorkerDepthExceeded,
    WorkerIdentityChanged,
    CallerFenced,
    NotInHerdr,
    RemoteUnsupported,
    ConsumerFenced,
    WaiterExists,
    StoreAccessDenied,
    SourceChanged,
    InactiveDispatch,
    ClosureReasonRequired,
    ClosureTargetRequired,
    QuestionAnswered,
    RecipientNotLive,
    EscalationEvidenceRequired,
    GateEvidenceRequired,
    HerdrUnreachable,
    HerdrError,
    TerminalBusy,
    UnknownAgent,
    LaunchMappingUnsupported,
    CallerMismatch,
    InvalidRecipient,
    QuestionClosed,
    TaskGuardRefused,
    ResetRequiresForce,
    LimitReached,
    BudgetExceeded,
    Usage,
}

impl ErrorCode {
    pub const ALL: &[ErrorCode] = &[
        ErrorCode::TaskNotFound,
        ErrorCode::TaskNotStartable,
        ErrorCode::NestedWorkerDepthExceeded,
        ErrorCode::WorkerIdentityChanged,
        ErrorCode::CallerFenced,
        ErrorCode::NotInHerdr,
        ErrorCode::RemoteUnsupported,
        ErrorCode::ConsumerFenced,
        ErrorCode::WaiterExists,
        ErrorCode::StoreAccessDenied,
        ErrorCode::SourceChanged,
        ErrorCode::InactiveDispatch,
        ErrorCode::ClosureReasonRequired,
        ErrorCode::ClosureTargetRequired,
        ErrorCode::QuestionAnswered,
        ErrorCode::RecipientNotLive,
        ErrorCode::EscalationEvidenceRequired,
        ErrorCode::GateEvidenceRequired,
        ErrorCode::HerdrUnreachable,
        ErrorCode::HerdrError,
        ErrorCode::TerminalBusy,
        ErrorCode::UnknownAgent,
        ErrorCode::LaunchMappingUnsupported,
        ErrorCode::CallerMismatch,
        ErrorCode::InvalidRecipient,
        ErrorCode::QuestionClosed,
        ErrorCode::TaskGuardRefused,
        ErrorCode::ResetRequiresForce,
        ErrorCode::LimitReached,
        ErrorCode::BudgetExceeded,
        ErrorCode::Usage,
    ];
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct PodError {
    pub code: ErrorCode,
    pub message: String,
    pub next_steps: Vec<String>,
    /// Extra `data` fields beside `nextSteps` (errors.md: `status` and
    /// `unmetDependencies`, `guard`, `liveWorkers`).
    pub data: Map<String, Value>,
}

impl PodError {
    pub fn new(code: ErrorCode, message: impl Into<String>, next_steps: &[&str]) -> Self {
        Self {
            code,
            message: message.into(),
            next_steps: next_steps.iter().map(|step| (*step).to_owned()).collect(),
            data: Map::new(),
        }
    }

    pub fn with_data(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.data.insert(key.to_owned(), value.into());
        self
    }

    pub fn usage(msg: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Usage,
            message: msg.into(),
            next_steps: vec!["Run the command with --help to see valid arguments.".into()],
            data: Map::new(),
        }
    }

    pub fn store(message: impl Into<String>) -> Self {
        let next_steps = if std::env::var_os("CODEX_SESSION_ID").is_some() {
            vec!["Add the state directory to the Codex sandbox's writable roots (for the workspace-write sandbox, an additional writable root), or run Codex with a sandbox mode that allows it. Restart the agent after changing it.".into()]
        } else if agent_environment("CLAUDECODE", "CLAUDE_CODE_") {
            vec!["Allow writes to the pod state directory in Claude Code's sandbox settings (the filesystem write allowlist), then restart the session. Alternatively set POD_STATE_DIR to a directory inside the project that the sandbox already permits.".into()]
        } else if std::env::vars_os().any(|(name, _)| name.to_string_lossy().starts_with("CODEX_"))
        {
            vec!["Add the state directory to the Codex sandbox's writable roots (for the workspace-write sandbox, an additional writable root), or run Codex with a sandbox mode that allows it. Restart the agent after changing it.".into()]
        } else {
            vec!["Ensure the pod state directory is writable, then retry.".into()]
        };
        Self {
            code: ErrorCode::StoreAccessDenied,
            message: message.into(),
            next_steps,
            data: Map::new(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        if self.code == ErrorCode::Usage { 2 } else { 1 }
    }
}

fn agent_environment(exact: &str, prefix: &str) -> bool {
    std::env::var_os(exact).is_some()
        || std::env::vars_os().any(|(name, _)| name.to_string_lossy().starts_with(prefix))
}

impl From<rusqlite::Error> for PodError {
    fn from(value: rusqlite::Error) -> Self {
        Self::store(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, PodError>;
