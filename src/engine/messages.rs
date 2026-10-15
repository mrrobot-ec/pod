use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::engine::{Engine, groups, questions, receipts, tasks};
use crate::error::{ErrorCode, PodError, Result};
use crate::identity::Caller;
use crate::ids::{DispatchId, MessageId, RequestId, RunId, TaskId, new_id};
use crate::store::Tx;
use crate::store::rows::{dispatches, messages, runs, tasks as task_rows};

pub use crate::engine::tasks::Settlement;
pub use crate::store::rows::dispatches::DispatchRow;
pub use crate::store::rows::messages::MessageRow;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SendInput {
    pub to: Option<String>,
    pub subject: String,
    pub body: Option<String>,
    pub r#type: String,
    pub priority: String,
    pub thread_id: Option<String>,
    pub payload: Option<Value>,
    pub task_id: Option<TaskId>,
    pub dispatch_id: Option<DispatchId>,
    pub outcome: Option<String>,
    pub reason: Option<String>,
    pub reason_target: Option<String>,
    pub files_modified: Vec<String>,
    pub report_path: Option<String>,
    pub phase: Option<String>,
    pub evidence: Option<String>,
    pub recommendation: Option<String>,
    pub from: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SendReceipt {
    pub message_ids: Vec<MessageId>,
    pub mailbox: String,
    pub settled: Option<Settlement>,
    pub duplicate: bool,
    pub warnings: Vec<String>,
    pub rejected_reason: Option<String>,
}

/// What one `send` transaction committed: a receipt, or a refusal whose
/// rewritten status message (for lifecycle types) was stored anyway. A replay
/// of the request returns the same refusal without storing a second message.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendOutcome {
    Stored(SendReceipt),
    Rejected(Rejection),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Rejection {
    pub code: ErrorCode,
    pub message: String,
    pub next_steps: Vec<String>,
    pub data: Map<String, Value>,
}

impl From<PodError> for Rejection {
    fn from(error: PodError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            next_steps: error.next_steps,
            data: error.data,
        }
    }
}

impl From<Rejection> for PodError {
    fn from(rejection: Rejection) -> Self {
        PodError {
            code: rejection.code,
            message: rejection.message,
            next_steps: rejection.next_steps,
            data: rejection.data,
        }
    }
}

impl SendOutcome {
    pub fn into_result(self) -> Result<SendReceipt> {
        match self {
            Self::Stored(receipt) => Ok(receipt),
            Self::Rejected(rejection) => Err(rejection.into()),
        }
    }
}

pub fn send(engine: &Engine, input: SendInput) -> Result<SendReceipt> {
    send_with_receipt(engine, input, None).map(|(receipt, _)| receipt)
}

/// `send` under an optional `--retry-request` receipt, and whether the
/// receipt was replayed; runs the commit hook for the Run the message reached.
pub fn send_with_receipt(
    engine: &Engine,
    input: SendInput,
    request: Option<&RequestId>,
) -> Result<(SendReceipt, bool)> {
    validate_input(&input)?;
    let caller = engine.caller.as_ref().ok_or_else(not_in_herdr)?;
    let hash = receipts::payload_hash(&serde_json::to_value(&input).unwrap_or_default());
    let now = engine.clock.now();
    let (outcome, replayed) =
        receipts::with_receipt_replay(engine, request, "send", &hash, |tx| {
            send_in(engine, tx, now, caller, &input)
        })?;
    if let SendOutcome::Stored(receipt) = &outcome {
        let rows = engine
            .store
            .read(|tx| messages::get_many(tx, &receipt.message_ids))?;
        if let Some(row) = rows.first() {
            let mut mailboxes = rows
                .iter()
                .map(|row| row.mailbox.clone())
                .collect::<Vec<_>>();
            mailboxes.sort();
            mailboxes.dedup();
            engine.after_commit(&row.run_id, &mailboxes)?;
        }
    }
    outcome.into_result().map(|receipt| (receipt, replayed))
}

pub fn mailbox_for(engine: &Engine, to: Option<&str>) -> Result<String> {
    let caller = engine.caller.as_ref();
    engine.store.read(|tx| {
        let context = sender_context(tx, caller.map(|caller| caller.terminal_id.as_str()))?;
        if to.is_none() && context.is_none() {
            return Err(PodError::usage(match caller {
                None => "outside herdr there is no caller Run; pass --run <id>",
                Some(_) => "this terminal is bound to no Run; pass --run <id>",
            }));
        }
        resolve_mailbox(tx, context.as_ref(), to).map(|(mailbox, _)| mailbox)
    })
}

/// The handle of the worker whose active Dispatch belongs to the caller's
/// terminal, if any.
pub fn caller_worker_handle(engine: &Engine) -> Result<Option<String>> {
    let Some(caller) = engine.caller.as_ref() else {
        return Ok(None);
    };
    engine.store.read(|tx| {
        Ok(dispatches::active_for_terminal(tx, &caller.terminal_id)?
            .filter(|dispatch| dispatch.status == "dispatched")
            .and_then(|dispatch| dispatch.assignee_handle))
    })
}

/// The sender's Run and handle: an active worker's, else the Run its
/// terminal coordinates.
#[derive(Clone)]
struct SenderContext {
    run_id: RunId,
    handle: String,
}

fn sender_context(tx: &Tx<'_>, terminal: Option<&str>) -> Result<Option<SenderContext>> {
    let Some(terminal) = terminal else {
        return Ok(None);
    };
    if let Some(dispatch) = dispatches::active_for_terminal(tx, terminal)?
        .filter(|dispatch| dispatch.status == "dispatched")
    {
        return Ok(Some(SenderContext {
            run_id: dispatch.run_id,
            handle: dispatch
                .assignee_handle
                .unwrap_or_else(|| terminal.to_owned()),
        }));
    }
    Ok(
        runs::bound_to_terminal(tx, terminal)?.map(|run| SenderContext {
            run_id: run.id,
            handle: run.coordinator_handle,
        }),
    )
}

/// A sender bound to no Run may still address `run:<id>` or `dispatch:<id>`.
fn explicit_sender_context(
    tx: &Tx<'_>,
    recipient: Option<&str>,
    handle: &str,
) -> Result<Option<SenderContext>> {
    let Some(recipient) = recipient else {
        return Ok(None);
    };
    let run = if let Some(run) = recipient.strip_prefix("run:") {
        match run.parse::<RunId>() {
            Ok(run) => runs::get(tx, &run)?.map(|row| row.id),
            Err(_) => None,
        }
    } else if let Some(dispatch) = recipient.strip_prefix("dispatch:") {
        match dispatch.parse::<DispatchId>() {
            Ok(dispatch) => dispatches::get(tx, &dispatch)?.map(|row| row.run_id),
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(run.map(|run_id| SenderContext {
        run_id,
        handle: handle.to_owned(),
    }))
}

/// The whole `send` inside one transaction.
pub fn send_in(
    engine: &Engine,
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    caller: &Caller,
    input: &SendInput,
) -> Result<SendOutcome> {
    let lifecycle = is_lifecycle(&input.r#type);
    let worker_done = input.r#type == "worker_done";
    let context = match sender_context(tx, Some(&caller.terminal_id))? {
        Some(context) => Some(context),
        None => explicit_sender_context(
            tx,
            input.to.as_deref(),
            input.from.as_deref().unwrap_or(&caller.terminal_id),
        )?,
    };
    let sender = context.as_ref().map_or_else(
        || caller.terminal_id.clone(),
        |context| context.handle.clone(),
    );
    // A refused lifecycle message is still stored for the coordinator,
    // rewritten as a high-priority status (8.1). Other refusals store nothing.
    let refuse = |run: Option<&RunId>, from: &str, error: PodError| -> Result<SendOutcome> {
        if let (true, Some(run)) = (lifecycle, run) {
            let reason = error_code(&error.code);
            let mut row = message_row(
                run,
                format!("run:{run}"),
                from,
                Some(&caller.terminal_id),
                input,
                input.thread_id.as_deref(),
                input.dispatch_id.as_ref(),
                now,
            );
            row.r#type = "status".to_owned();
            row.priority = "high".to_owned();
            row.rejected_reason = Some(reason);
            messages::insert(tx, &row)?;
        }
        Ok(SendOutcome::Rejected(error.into()))
    };
    let context_run = context.as_ref().map(|context| context.run_id.clone());

    if let (Some(requested), Some(context)) = (&input.from, &context)
        && requested != &context.handle
    {
        return refuse(
            Some(&context.run_id),
            &sender,
            PodError::new(
                ErrorCode::CallerMismatch,
                "--from does not match the resolved caller",
                &["Leave out --from, or pass your own handle."],
            ),
        );
    }

    if let Some(group) = input.to.as_deref().filter(|to| to.starts_with('@')) {
        if lifecycle {
            return refuse(
                context_run.as_ref(),
                &sender,
                invalid_recipient("lifecycle messages cannot target groups"),
            );
        }
        let context = context.ok_or_else(unbound_sender)?;
        let targets = groups::resolve_group(tx, &context.run_id, group, &caller.terminal_id)?;
        let thread = input.thread_id.clone().unwrap_or_else(|| new_id("thread_"));
        let mut ids = Vec::with_capacity(targets.len());
        for target in targets {
            let row = message_row(
                &context.run_id,
                format!("dispatch:{target}"),
                &context.handle,
                Some(&caller.terminal_id),
                input,
                Some(&thread),
                None,
                now,
            );
            messages::insert(tx, &row)?;
            ids.push(row.id);
        }
        return Ok(SendOutcome::Stored(SendReceipt {
            message_ids: ids,
            mailbox: format!("group:{group}"),
            settled: None,
            duplicate: false,
            warnings: Vec::new(),
            rejected_reason: None,
        }));
    }

    if !lifecycle {
        let context = context.ok_or_else(unbound_sender)?;
        let (mailbox, run) = resolve_mailbox(tx, Some(&context), input.to.as_deref())?;
        let row = message_row(
            &run,
            mailbox.clone(),
            &context.handle,
            Some(&caller.terminal_id),
            input,
            input.thread_id.as_deref(),
            None,
            now,
        );
        messages::insert(tx, &row)?;
        return Ok(SendOutcome::Stored(SendReceipt {
            message_ids: vec![row.id],
            mailbox,
            settled: None,
            duplicate: false,
            warnings: Vec::new(),
            rejected_reason: None,
        }));
    }

    let dispatch = match input.dispatch_id.as_ref() {
        Some(id) => dispatches::get(tx, id)?,
        None => dispatches::latest_for_terminal(tx, &caller.terminal_id)?,
    };
    let Some(dispatch) = dispatch else {
        let error = if worker_done {
            inactive_dispatch()
        } else {
            caller_fenced("the lifecycle message does not name an active Dispatch")
        };
        return refuse(context_run.as_ref(), &sender, error);
    };
    // Identity first (3.4): only the Dispatch's own terminal may report on it,
    // whether the Dispatch is active, settled, or fenced.
    let owner = dispatch.assignee_terminal_id.as_deref() == Some(caller.terminal_id.as_str());
    let handle = dispatch.assignee_handle.clone().unwrap_or_default();
    let from = if owner { handle.clone() } else { sender };
    let refuse_here = |error: PodError| refuse(Some(&dispatch.run_id), &from, error);
    if !owner {
        return refuse_here(caller_fenced(
            "the caller terminal does not own this Dispatch",
        ));
    }
    if dispatch.status != "dispatched" {
        if worker_done {
            let previous = messages::first_worker_done(tx, dispatch.id.as_str())?
                .and_then(|row| row.payload)
                .and_then(|payload| {
                    payload
                        .get("outcome")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            if previous.is_some() && previous.as_deref() == input.outcome.as_deref() {
                return Ok(SendOutcome::Stored(SendReceipt {
                    message_ids: Vec::new(),
                    mailbox: format!("run:{}", dispatch.run_id),
                    settled: Some(tasks::settlement_of(tx, &dispatch.id)?),
                    duplicate: true,
                    warnings: Vec::new(),
                    rejected_reason: None,
                }));
            }
            return refuse_here(inactive_dispatch());
        }
        return refuse_here(caller_fenced("the Dispatch is no longer active"));
    }
    let warning = match process_identity(engine, &dispatch, caller.pid) {
        Identity::Verified => None,
        Identity::Fallback(warning) => Some(warning),
        Identity::Mismatch => {
            return refuse_here(PodError::new(
                ErrorCode::WorkerIdentityChanged,
                "the recorded agent process is not an ancestor of this pod process",
                &[
                    "Do not retry. The message was stored for the coordinator; stop work and go idle.",
                ],
            ));
        }
    };
    accept_lifecycle(tx, input, &dispatch, &handle, warning, now, &refuse_here)
}

/// Validates and applies an accepted lifecycle message from the Dispatch's
/// owner. `worker_done` settles through `tasks::settle_dispatch`, the only
/// place a worker report changes Task status.
fn accept_lifecycle(
    tx: &Tx<'_>,
    input: &SendInput,
    dispatch: &DispatchRow,
    handle: &str,
    warning: Option<String>,
    now: DateTime<Utc>,
    refuse: &dyn Fn(PodError) -> Result<SendOutcome>,
) -> Result<SendOutcome> {
    let rejection = |code: ErrorCode, message: &str| {
        refuse(PodError::new(
            code,
            message,
            &[
                "Correct the message and send it again; it was stored for the coordinator as a rejected status.",
            ],
        ))
    };
    if input.r#type == "escalation" && (input.evidence.is_none() || input.recommendation.is_none())
    {
        return rejection(
            ErrorCode::EscalationEvidenceRequired,
            "escalation requires evidence and a recommendation",
        );
    }
    let mut warnings = warning.into_iter().collect::<Vec<_>>();
    let mut settled = None;
    if input.r#type == "worker_done" {
        let outcome = tasks::Outcome::parse(
            input
                .outcome
                .as_deref()
                .ok_or_else(|| PodError::usage("worker_done requires --outcome"))?,
        )?;
        let reason = match (&input.reason, outcome) {
            (Some(reason), _) => reason.as_str(),
            (None, tasks::Outcome::Succeeded) => "no_follow_on",
            (None, tasks::Outcome::Failed) => {
                return rejection(
                    ErrorCode::ClosureReasonRequired,
                    "--outcome failed needs --reason",
                );
            }
        };
        if !matches!(
            reason,
            "no_follow_on"
                | "handed_off"
                | "blocked_on"
                | "denied"
                | "canceled"
                | "escalated"
                | "superseded"
        ) {
            return Err(PodError::usage("invalid closure reason"));
        }
        if matches!(reason, "handed_off" | "blocked_on" | "superseded")
            && input.reason_target.is_none()
        {
            return rejection(
                ErrorCode::ClosureTargetRequired,
                "this closure reason needs --reason-target",
            );
        }
        if let Some(target) = input.reason_target.as_deref()
            && !target_exists(tx, &dispatch.run_id, target)?
        {
            warnings.push(format!("reason target {target} was not found in this Run"));
        }
        let settlement = tasks::settle_dispatch(
            tx,
            now,
            &dispatch.id,
            outcome,
            reason,
            input.reason_target.as_deref(),
        )?;
        // Pending questions close when their Dispatch fails (8.4).
        if outcome == tasks::Outcome::Failed {
            questions::close_questions_for_dispatch(tx, now, &dispatch.id)?;
        }
        settled = Some(settlement);
    } else if input.r#type == "heartbeat" {
        dispatches::update_last_heartbeat_at(tx, &dispatch.id, Some(now))?;
        dispatches::update_last_phase(tx, &dispatch.id, input.phase.as_deref())?;
    }

    let mailbox = format!("run:{}", dispatch.run_id);
    let row = message_row(
        &dispatch.run_id,
        mailbox.clone(),
        handle,
        dispatch.assignee_terminal_id.as_deref(),
        input,
        input.thread_id.as_deref(),
        Some(&dispatch.id),
        now,
    );
    messages::insert(tx, &row)?;
    Ok(SendOutcome::Stored(SendReceipt {
        message_ids: vec![row.id],
        mailbox,
        settled,
        duplicate: false,
        warnings,
        rejected_reason: None,
    }))
}

#[allow(clippy::too_many_arguments)]
fn message_row(
    run: &RunId,
    mailbox: String,
    from: &str,
    sender_terminal: Option<&str>,
    input: &SendInput,
    thread: Option<&str>,
    stamped_dispatch: Option<&DispatchId>,
    now: DateTime<Utc>,
) -> MessageRow {
    MessageRow {
        sequence: None,
        id: MessageId::new(),
        run_id: run.clone(),
        mailbox,
        from_handle: from.to_owned(),
        subject: input.subject.clone(),
        body: input.body.clone(),
        r#type: input.r#type.clone(),
        priority: input.priority.clone(),
        thread_id: thread.map(str::to_owned),
        payload: payload(input, stamped_dispatch),
        read: false,
        created_at: now,
        delivered_at: None,
        sender_terminal_id: sender_terminal.map(str::to_owned),
        rejected_reason: None,
        evidence: input.evidence.clone(),
        recommendation: input.recommendation.clone(),
    }
}

/// The stored payload: a raw `--payload`, or the typed flags, with the
/// Dispatch stamped in for lifecycle messages.
fn payload(input: &SendInput, stamped_dispatch: Option<&DispatchId>) -> Option<Value> {
    let mut map = match input.payload.clone() {
        Some(Value::Object(map)) => map,
        Some(value) => {
            let mut map = Map::new();
            map.insert("value".into(), value);
            map
        }
        None => Map::new(),
    };
    if let Some(task) = &input.task_id {
        map.insert("task_id".into(), json!(task));
    }
    if let Some(dispatch) = stamped_dispatch.or(input.dispatch_id.as_ref()) {
        map.insert("dispatch_id".into(), json!(dispatch));
    }
    if let Some(outcome) = &input.outcome {
        map.insert("outcome".into(), json!(outcome));
    }
    if let Some(reason) = &input.reason {
        map.insert("reason".into(), json!(reason));
    }
    if let Some(target) = &input.reason_target {
        map.insert("reason_target".into(), json!(target));
    }
    if !input.files_modified.is_empty() {
        map.insert("files_modified".into(), json!(input.files_modified));
    }
    if let Some(path) = &input.report_path {
        map.insert("report_path".into(), json!(path));
    }
    if let Some(phase) = &input.phase {
        map.insert("phase".into(), json!(phase));
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

/// Whether a `--reason-target` names a Task or Dispatch of `run`.
fn target_exists(tx: &Tx<'_>, run: &RunId, target: &str) -> Result<bool> {
    if let Ok(task) = target.parse::<TaskId>() {
        return Ok(task_rows::get(tx, &task)?.is_some_and(|row| row.run_id == *run));
    }
    if let Ok(dispatch) = target.parse::<DispatchId>() {
        return Ok(dispatches::get(tx, &dispatch)?.is_some_and(|row| row.run_id == *run));
    }
    Ok(false)
}

/// Resolves a recipient to its mailbox and the Run that mailbox belongs to.
fn resolve_mailbox(
    tx: &Tx<'_>,
    context: Option<&SenderContext>,
    to: Option<&str>,
) -> Result<(String, RunId)> {
    let Some(to) = to else {
        return context
            .map(|context| (format!("run:{}", context.run_id), context.run_id.clone()))
            .ok_or_else(unbound_sender);
    };
    if to.starts_with("task:") {
        return Err(invalid_recipient("task recipients are not supported"));
    }
    if let Some(run) = to.strip_prefix("run:") {
        if let Ok(id) = run.parse::<RunId>()
            && runs::get(tx, &id)?.is_some()
        {
            return Ok((to.to_owned(), id));
        }
        return Err(PodError::usage(format!("unknown Run {run}")));
    }
    if let Some(dispatch) = to.strip_prefix("dispatch:") {
        if let Ok(id) = dispatch.parse::<DispatchId>()
            && let Some(row) = dispatches::get(tx, &id)?
        {
            return Ok((to.to_owned(), row.run_id));
        }
        return Err(PodError::usage(format!("unknown Dispatch {dispatch}")));
    }
    if to.starts_with('@') {
        return Err(invalid_recipient("a group has more than one mailbox"));
    }
    // A bound sender's handles resolve within its Run; read-only callers with
    // no Run (inbox outside herdr) take the handle's active Dispatch anywhere.
    let run = context.map(|context| &context.run_id);
    if let Some(dispatch) = dispatches::dispatched_by_handle(tx, run, to)? {
        return Ok((format!("dispatch:{}", dispatch.id), dispatch.run_id));
    }
    Err(invalid_recipient(
        "recipient is not an active worker in this Run",
    ))
}

fn error_code(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .expect("serialize error code")
        .as_str()
        .expect("error code is string")
        .to_owned()
}

fn is_lifecycle(message_type: &str) -> bool {
    matches!(
        message_type,
        "worker_done" | "heartbeat" | "escalation" | "decision_gate"
    )
}

fn validate_input(input: &SendInput) -> Result<()> {
    if !matches!(
        input.r#type.as_str(),
        "worker_done"
            | "heartbeat"
            | "escalation"
            | "decision_gate"
            | "status"
            | "merge_ready"
            | "handoff"
            | "dispatch"
    ) {
        return Err(PodError::usage(
            "--type must name a sendable message type; question is created by ask",
        ));
    }
    if !matches!(input.priority.as_str(), "normal" | "high" | "urgent") {
        return Err(PodError::usage(
            "--priority must be normal, high, or urgent",
        ));
    }
    let has_typed_payload = input.task_id.is_some()
        || input.dispatch_id.is_some()
        || input.outcome.is_some()
        || input.reason.is_some()
        || input.reason_target.is_some()
        || !input.files_modified.is_empty()
        || input.report_path.is_some()
        || input.phase.is_some()
        || input.evidence.is_some()
        || input.recommendation.is_some();
    if input.payload.is_some() && has_typed_payload {
        return Err(PodError::usage(
            "--payload cannot be combined with typed message fields",
        ));
    }
    if input.r#type != "worker_done"
        && (input.outcome.is_some() || input.reason.is_some() || input.reason_target.is_some())
    {
        return Err(PodError::usage(
            "--outcome, --reason, and --reason-target are valid only for worker_done",
        ));
    }
    if let Some(outcome) = input.outcome.as_deref() {
        tasks::Outcome::parse(outcome)?;
    }
    Ok(())
}

fn process_identity(engine: &Engine, dispatch: &DispatchRow, caller_pid: u32) -> Identity {
    let Some(agent_pid) = dispatch.assignee_pid else {
        return Identity::Fallback(
            "agent process ID unavailable; terminal identity fallback used".into(),
        );
    };
    match proc_ancestor(agent_pid, caller_pid) {
        Some(true) => Identity::Verified,
        Some(false) => Identity::Mismatch,
        // Section 19 item 2: the chain cannot be read (for example inside an
        // agent sandbox). herdr's pane process list may still corroborate it;
        // otherwise the terminal check alone stands, with a warning.
        None => {
            if let Some(pane) = engine
                .caller
                .as_ref()
                .and_then(|caller| caller.pane_id.as_deref())
                && let Ok(info) = engine.herdr.pane_process_info(pane)
                && info.pids.contains(&agent_pid)
                && info.pids.contains(&caller_pid)
            {
                return Identity::Verified;
            }
            Identity::Fallback(
                "process ancestry unavailable; terminal identity fallback used".into(),
            )
        }
    }
}

/// Whether `ancestor` is `child` or one of its ancestors; `None` when the
/// parent chain cannot be read.
fn proc_ancestor(ancestor: u32, mut child: u32) -> Option<bool> {
    for _ in 0..128 {
        if child == ancestor {
            return Some(true);
        }
        if child <= 1 {
            return Some(false);
        }
        child = parent_pid(child)?;
    }
    Some(false)
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    stat.get(close + 2..)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
fn parent_pid(pid: u32) -> Option<u32> {
    // `struct proc_bsdshortinfo` from <sys/proc_info.h>
    // (PROC_PIDT_SHORTBSDINFO_SIZE = 64). Unlike the full BSD info flavor it
    // is readable for root-owned ancestors such as `login`.
    #[repr(C)]
    struct ProcBsdShortInfo {
        pid: u32,
        ppid: u32,
        pgid: u32,
        status: u32,
        comm: [u8; 16],
        rest: [u32; 8],
    }
    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut std::ffi::c_void,
            size: i32,
        ) -> i32;
    }
    let raw_pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<ProcBsdShortInfo>::zeroed();
    let size = std::mem::size_of::<ProcBsdShortInfo>() as i32;
    // SAFETY: the buffer is a zeroed, writable `proc_bsdshortinfo` of `size`
    // bytes; libproc writes at most `size` bytes and returns how many it wrote.
    let written = unsafe {
        proc_pidinfo(
            raw_pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: every field is plain data and the struct started zeroed.
    let info = unsafe { info.assume_init() };
    (info.pid == pid).then_some(info.ppid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

fn inactive_dispatch() -> PodError {
    PodError::new(
        ErrorCode::InactiveDispatch,
        "worker_done arrived for an inactive Dispatch",
        &[
            "Do not retry or send another lifecycle message from this Dispatch. The report was stored for the coordinator; go idle.",
        ],
    )
}

fn caller_fenced(message: &str) -> PodError {
    PodError::new(
        ErrorCode::CallerFenced,
        message,
        &["Do not retry. The message was stored for the coordinator; stop work and go idle."],
    )
}

fn not_in_herdr() -> PodError {
    PodError::new(
        ErrorCode::NotInHerdr,
        "this mutation requires a caller inside herdr",
        &[
            "Run the command from a pane inside herdr. Read-only commands work outside herdr with --run <id>.",
        ],
    )
}

fn unbound_sender() -> PodError {
    invalid_recipient("this terminal is bound to no Run; address run:<id> or dispatch:<id>")
}

fn invalid_recipient(message: &str) -> PodError {
    PodError::new(
        ErrorCode::InvalidRecipient,
        message,
        &["Address run:<id>, dispatch:<id>, a worker handle, or a group with live workers."],
    )
}

enum Identity {
    Verified,
    Mismatch,
    Fallback(String),
}
