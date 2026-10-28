use std::path::PathBuf;
use std::str::FromStr;

use clap::Args;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::cli::{Ctx, output};
use crate::engine::lifecycle::{self, Setup, StartInput, Worktree};
use crate::engine::{Engine, receipts};
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{DispatchId, RequestId, RunId, TaskId};
use crate::store::rows::{dispatches, receipts as receipt_rows};

#[derive(Clone, Debug, Args)]
#[command(group(clap::ArgGroup::new("task_source").required(true).multiple(false).args(["task", "spec"])), group(clap::ArgGroup::new("agent_target").required(true).multiple(false).args(["agent", "terminal"])))]
pub struct WorkerStartArgs {
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub spec: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub terminal: Option<String>,
    #[arg(long, default_value = "current")]
    pub worktree: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub base_branch: Option<String>,
    #[arg(long)]
    pub display_name: Option<String>,
    #[arg(long)]
    pub comment: Option<String>,
    #[arg(long)]
    pub setup: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long)]
    pub fast: bool,
    #[arg(long)]
    pub yolo: bool,
    #[arg(long)]
    pub task_title: Option<String>,
    #[arg(long)]
    pub deps: Option<String>,
    #[arg(long)]
    pub parent: Option<String>,
    #[arg(long)]
    pub retry_of: Option<String>,
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub on: Option<String>,
    /// Worker handle; default `<agent>-<worktree name>` or `<agent>`.
    #[arg(long)]
    pub handle: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct DispatchIdArgs {
    #[arg(long)]
    pub dispatch: String,
}

#[derive(Clone, Debug, Args)]
pub struct WorkerReadArgs {
    #[arg(long)]
    pub dispatch: String,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct WorkerListArgs {
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub terminal_state: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: Option<u32>,
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct WorkerRetainArgs {
    #[arg(long)]
    pub dispatch: String,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct DispatchArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long)]
    pub to: String,
    /// Injection is the default; the flag is accepted for Orca's contract.
    #[arg(long, conflicts_with = "no_inject")]
    pub inject: bool,
    /// Record the Dispatch and type nothing: the caller pastes the preamble.
    #[arg(long)]
    pub no_inject: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub return_preamble: bool,
}

#[derive(Clone, Debug, Args)]
pub struct WorkerGcArgs {
    #[arg(long)]
    pub run: Option<String>,
    /// Release retained workers too, as `worker-release` would.
    #[arg(long)]
    pub include_retained: bool,
}

#[derive(Clone, Debug, Args)]
pub struct DispatchShowArgs {
    #[arg(long)]
    pub task: String,
}

pub fn worker_start(ctx: &Ctx, args: WorkerStartArgs) -> Result<()> {
    if args.on.is_some() {
        return Err(PodError::new(
            ErrorCode::RemoteUnsupported,
            "remote worker placement is not supported",
            &["Remove --on and start the worker locally."],
        ));
    }
    let worktree = parse_worktree(&args.worktree)?;
    if args.setup.is_some() && !matches!(worktree, Worktree::NewChild | Worktree::NewTopLevel) {
        return Err(PodError::usage(
            "--setup is valid only with new-child or new-top-level",
        ));
    }
    let engine = ctx.engine()?;
    let input = StartInput {
        task: args.task.as_deref().map(TaskId::from_str).transpose()?,
        spec: args.spec,
        agent: args.agent,
        terminal: args.terminal,
        worktree,
        name: args.name,
        repo: args.repo,
        base_branch: args.base_branch,
        display_name: args.display_name,
        comment: args.comment,
        setup: parse_setup(args.setup.as_deref())?,
        model: args.model,
        effort: args.effort,
        fast: args.fast,
        yolo: args.yolo,
        task_title: args.task_title,
        deps: parse_deps(args.deps.as_deref())?,
        parent: args.parent.as_deref().map(TaskId::from_str).transpose()?,
        retry_of: args
            .retry_of
            .as_deref()
            .map(DispatchId::from_str)
            .transpose()?,
        timeout_ms: args.timeout_ms,
        run: args.run.as_deref().map(RunId::from_str).transpose()?,
        handle: args.handle,
    };
    let payload = serde_json::to_value(&input)
        .map_err(|error| PodError::store(format!("could not serialize worker-start: {error}")))?;
    let (receipt, replayed) = mutation(ctx, &engine, "worker-start", &payload, || {
        lifecycle::worker_start(&engine, input)
    })?;
    // Exit 0 only for a `ready` worker whose preamble was confirmed;
    // `dispatched_unobserved` exits 1 with the Dispatch still recorded.
    let ready = receipt.state == "ready" && receipt.outcome.is_none();
    let view = lifecycle::show(&engine, &receipt.dispatch_id)?;
    let mut value = serde_json::to_value(&receipt)
        .map_err(|error| PodError::store(format!("could not serialize worker-start: {error}")))?;
    value["dispatch"] = to_json(view.dispatch)?;
    value["worker"] = to_json(view.worker)?;
    super::emit_mutation(ctx, &camel_rows(value), replayed)?;
    if !ready {
        std::process::exit(1);
    }
    Ok(())
}

pub fn worker_show(ctx: &Ctx, args: DispatchIdArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let dispatch = DispatchId::from_str(&args.dispatch)?;
    output::emit_ok(ctx.json, &show_value(lifecycle::show(&engine, &dispatch)?)?);
    Ok(())
}

fn to_json(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| PodError::store(error.to_string()))
}

/// A row's snake_case keys in camelCase, the names Orca's `worker-show`
/// uses for its Dispatch, worker, and observation (nested values unchanged).
fn camel_keys(value: Value) -> Value {
    let Value::Object(fields) = value else {
        return value;
    };
    fields
        .into_iter()
        .map(|(key, value)| {
            let mut camel = String::with_capacity(key.len());
            let mut upper = false;
            for character in key.chars() {
                if character == '_' {
                    upper = true;
                } else if upper {
                    camel.push(character.to_ascii_uppercase());
                    upper = false;
                } else {
                    camel.push(character);
                }
            }
            (camel, value)
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// A worker receipt's `dispatch` and `worker` rows in `worker-show`'s casing
/// (camelCase, plus Orca's `task_id`), so every worker command reports one
/// spelling of each field (`terminalState`, never `terminal_state`).
fn camel_rows(mut value: Value) -> Value {
    if let Some(dispatch) = value.get_mut("dispatch").filter(|row| row.is_object()) {
        let task_id = dispatch.get("task_id").cloned();
        *dispatch = camel_keys(dispatch.take());
        if let Some(task_id) = task_id {
            dispatch["task_id"] = task_id;
        }
    }
    if let Some(worker) = value.get_mut("worker").filter(|row| row.is_object()) {
        *worker = camel_keys(worker.take());
    }
    value
}

/// The receipt of stop, abandon, retain, and release, rows as `camel_rows`.
fn emit_worker_receipt(
    ctx: &Ctx,
    receipt: &lifecycle::WorkerReceipt,
    replayed: bool,
) -> Result<()> {
    super::emit_mutation(ctx, &camel_rows(to_json(receipt)?), replayed)
}

/// `worker-show`: the Dispatch (camelCase, plus Orca's `task_id`), worker,
/// latest observation, projection, and wake ladder.
fn show_value(view: lifecycle::WorkerView) -> Result<Value> {
    let task_id = view.dispatch.task_id.clone();
    let mut dispatch = camel_keys(to_json(view.dispatch)?);
    dispatch["task_id"] = to_json(task_id)?;
    Ok(json!({
        "dispatch": dispatch,
        "worker": camel_keys(to_json(view.worker)?),
        "observation": view.observation.map(to_json).transpose()?.map(camel_keys),
        "projection": view.projection,
        "wakeLadder": view.wake_ladder,
        "briefPath": view.brief_path,
        // What the agent reported, `null` when none did: never a zero.
        "costUsd": view.cost_usd,
    }))
}

/// One `worker-list` row, in the names of Orca's rows.
fn list_row(view: lifecycle::WorkerView) -> Value {
    json!({
        "dispatchId": view.dispatch.id,
        "taskId": view.dispatch.task_id,
        "runId": view.dispatch.run_id,
        "workerState": view.worker.state,
        "dispatchStatus": view.dispatch.status,
        "agentTerminalHandle": view.dispatch.assignee_handle,
        "terminalState": view.worker.terminal_state,
        "terminationReason": view.dispatch.termination_reason,
        "terminationTarget": view.dispatch.termination_target,
        "projection": view.projection,
        "briefPath": view.brief_path,
        "costUsd": view.cost_usd,
    })
}

// BEGIN T9 worker-read
pub fn worker_read(ctx: &Ctx, args: WorkerReadArgs) -> Result<()> {
    use std::str::FromStr;
    let engine = ctx.read_engine()?;
    let dispatch = crate::ids::DispatchId::from_str(&args.dispatch)?;
    let source = read::Source::parse(args.source.as_deref())?;
    let limit = args.limit.unwrap_or(read::DEFAULT_LIMIT);
    let receipt = read::worker_read(&engine, &dispatch, source, limit, args.cursor.as_deref())?;
    crate::cli::output::emit_ok(ctx.json, &receipt);
    Ok(())
}

/// The engine side of `worker-read` (spec section 11). It lives in a module
/// because the command handler above already owns the name `worker_read`.
pub mod read {
    use std::path::PathBuf;

    use serde::Serialize;
    use sha2::{Digest, Sha256};

    use crate::engine::Engine;
    use crate::engine::transcripts::locate_with_reason;
    use crate::error::{ErrorCode, PodError, Result};
    use crate::ids::DispatchId;
    use crate::store::rows::{
        dispatches, worker_archives,
        workers::{self, WorkerRow},
    };
    use crate::transcript::{Row, screen::TOOL_OUTPUT_LIMIT};

    pub const DEFAULT_LIMIT: u32 = 50;
    const MAX_LIMIT: u32 = 500;
    const SCREEN_LINES: u32 = 2000;
    const SCREEN_PREFIX: &str = "screen-";

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Source {
        Auto,
        Transcript,
        Terminal,
    }

    impl Source {
        pub fn parse(value: Option<&str>) -> Result<Self> {
            match value {
                None | Some("auto") => Ok(Self::Auto),
                Some("transcript") => Ok(Self::Transcript),
                Some("terminal") => Ok(Self::Terminal),
                Some(other) => Err(PodError::usage(format!(
                    "--source must be auto, transcript, or terminal, not {other}"
                ))),
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Clipping {
        pub rows_trimmed: u32,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ReadReceipt {
        pub source: String,
        pub rows: Vec<Row>,
        pub cursor: Option<String>,
        pub content_complete: bool,
        pub clipping: Clipping,
        pub warnings: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub fallback_reason: Option<String>,
    }

    pub fn worker_read(
        engine: &Engine,
        dispatch: &DispatchId,
        source: Source,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<ReadReceipt> {
        if limit == 0 {
            return Err(PodError::usage("--limit must be at least 1"));
        }
        let limit = limit.min(MAX_LIMIT);
        let (kind, worker, archive) = engine.store.read(|tx| {
            let row = dispatches::get(tx, dispatch)?
                .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} was not found")))?;
            let worker = workers::get(tx, dispatch)?.ok_or_else(|| {
                PodError::usage(format!("Dispatch {dispatch} has no worker to read"))
            })?;
            let archive = worker_archives::get(tx, dispatch, "final_screen")?;
            Ok((row.agent_kind, worker, archive))
        })?;
        let mut warnings = Vec::new();
        // The archive is written at release. From then on the pane, if it is
        // still open, may belong to someone else, so the archive and the
        // stored session proof speak for this worker instead of herdr.
        let released = archive.is_some();

        let mut fallback = None;
        if source != Source::Terminal {
            match locate_with_reason(engine, dispatch, kind.as_deref(), released, &mut warnings)? {
                Ok(found) => {
                    check_pinned(cursor, false)?;
                    return read_transcript(found, cursor, limit, warnings);
                }
                Err(reason) if source == Source::Transcript => {
                    return Err(PodError::usage(format!(
                        "no readable transcript for {dispatch} ({reason}); use --source terminal"
                    )));
                }
                Err(reason) => fallback = Some(reason),
            }
        }

        check_pinned(cursor, true)?;
        let screen = match archive {
            Some(archive) => Some((archive.content, format!("archive:{dispatch}"))),
            None => live_screen(engine, &worker, source == Source::Terminal, &mut warnings)?,
        };
        let Some((text, identity)) = screen else {
            if source == Source::Terminal {
                return Err(PodError::usage(format!(
                    "Dispatch {dispatch} has no pane and no archived screen to read"
                )));
            }
            if cursor.is_some() {
                return Err(source_changed("there is no screen left to read"));
            }
            return Ok(ReadReceipt {
                source: "terminal".into(),
                rows: Vec::new(),
                cursor: None,
                content_complete: true,
                clipping: Clipping { rows_trimmed: 0 },
                warnings,
                fallback_reason: Some("no_terminal".into()),
            });
        };
        let mut receipt = page_screen(&text, &identity, cursor, limit, warnings)?;
        receipt.fallback_reason = fallback.map(str::to_owned);
        Ok(receipt)
    }

    /// A cursor made by one source is refused by the other.
    fn check_pinned(cursor: Option<&str>, terminal: bool) -> Result<()> {
        match cursor {
            Some(cursor) if cursor.starts_with(SCREEN_PREFIX) != terminal => {
                Err(source_changed("the cursor was issued for the other source"))
            }
            _ => Ok(()),
        }
    }

    fn source_changed(detail: &str) -> PodError {
        PodError::new(
            ErrorCode::SourceChanged,
            format!("the cursor does not belong to the source now being read: {detail}"),
            &["Restart reading without --cursor, or force the original source with --source."],
        )
    }

    fn read_transcript(
        (reader, path): (Box<dyn crate::transcript::Reader>, PathBuf),
        cursor: Option<&str>,
        limit: u32,
        mut warnings: Vec<String>,
    ) -> Result<ReadReceipt> {
        let page = reader.read(&path, cursor, limit)?;
        warnings.extend(page.warnings);
        let trimmed = if page.clipping {
            page.rows
                .iter()
                .filter(|row| {
                    row.kind == "tool_result" && row.text.chars().count() >= TOOL_OUTPUT_LIMIT
                })
                .count() as u32
        } else {
            0
        };
        Ok(ReadReceipt {
            source: "transcript".into(),
            rows: page.rows,
            cursor: page.cursor,
            content_complete: page.content_complete,
            clipping: Clipping {
                rows_trimmed: trimmed,
            },
            warnings,
            fallback_reason: None,
        })
    }

    /// The worker's live screen and its cursor identity, when herdr still
    /// knows the pane. A herdr failure is an error for a forced terminal read
    /// and a warning for `auto`.
    fn live_screen(
        engine: &Engine,
        worker: &WorkerRow,
        forced: bool,
        warnings: &mut Vec<String>,
    ) -> Result<Option<(String, String)>> {
        let Some(pane) = worker.pane_id.as_deref() else {
            return Ok(None);
        };
        let text = engine.herdr.pane_get(pane).and_then(|found| match found {
            Some(_) => engine.herdr.pane_read(pane, SCREEN_LINES).map(Some),
            None => Ok(None),
        });
        match text {
            Ok(text) => Ok(text.map(|text| (text, format!("pane:{pane}")))),
            Err(error) if forced => Err(error),
            Err(error) => {
                warnings.push(format!("herdr screen read failed: {}", error.message));
                Ok(None)
            }
        }
    }

    fn page_screen(
        text: &str,
        identity: &str,
        cursor: Option<&str>,
        limit: u32,
        warnings: Vec<String>,
    ) -> Result<ReadReceipt> {
        let hash = hex::encode(&Sha256::digest(identity.as_bytes())[..8]);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect();
        let start = match cursor {
            None => 0,
            Some(raw) => {
                let (head, offset) = raw
                    .strip_prefix(SCREEN_PREFIX)
                    .and_then(|rest| rest.split_once(':'))
                    .ok_or_else(|| PodError::usage("cursor must look like screen-<hash>:<line>"))?;
                if head != hash {
                    return Err(source_changed("it was issued for another pane or archive"));
                }
                let offset: usize = offset
                    .parse()
                    .map_err(|_| PodError::usage("cursor offset is not a number"))?;
                if offset > lines.len() {
                    return Err(source_changed("the screen is shorter than the cursor"));
                }
                offset
            }
        };
        let end = lines.len().min(start + limit as usize);
        let rows = lines[start..end]
            .iter()
            .map(|line| Row {
                role: "screen".into(),
                kind: "screen".into(),
                text: (*line).to_owned(),
                tool: None,
                at: None,
            })
            .collect();
        Ok(ReadReceipt {
            source: "terminal".into(),
            rows,
            cursor: Some(format!("{SCREEN_PREFIX}{hash}:{end}")),
            content_complete: end == lines.len(),
            clipping: Clipping { rows_trimmed: 0 },
            warnings,
            fallback_reason: None,
        })
    }
}
// END T9 worker-read

pub fn worker_list(ctx: &Ctx, args: WorkerListArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let run = args.run.as_deref().map(RunId::from_str).transpose()?;
    let list = lifecycle::list(
        &engine,
        run.as_ref(),
        args.terminal_state.as_deref(),
        args.limit.unwrap_or(100),
        args.cursor.as_deref(),
    )?;
    let mut counts = serde_json::Map::new();
    for state in [
        "active",
        "reclaimable",
        "retained",
        "release_pending",
        "release_unknown",
        "released",
    ] {
        counts.insert(
            state.into(),
            list.counts.get(state).copied().unwrap_or(0).into(),
        );
    }
    let shown = list.workers.len();
    let mut warnings: Vec<String> = list
        .next_cursor
        .iter()
        .map(|_| {
            format!(
                "Showing {shown} of {} workers, newest first; more are on later pages. Follow page.nextCursor with --cursor.",
                list.total
            )
        })
        .collect();
    // Spec 9: Tasks left `pending` behind a failed dependency are reported
    // here too, so a coordinator watching workers sees the stalled branch.
    let waiting = match &list.scope.run {
        Some(run) => waiting_on_failed_dependency(&engine, run)?,
        None => Vec::new(),
    };
    for entry in &waiting {
        warnings.push(format!(
            "Task {} waits on failed dependency {}; retry it or change the plan.",
            entry["taskId"].as_str().unwrap_or_default(),
            entry["failedDependency"].as_str().unwrap_or_default()
        ));
    }
    output::emit_ok(
        ctx.json,
        &json!({
            "scope": list.scope,
            "workers": list.workers.into_iter().map(list_row).collect::<Vec<_>>(),
            "counts": counts,
            "page": {
                "limit": list.limit,
                "total": list.total,
                "hasMore": list.next_cursor.is_some(),
                "nextCursor": list.next_cursor,
            },
            "waitingOnFailedDependency": waiting,
            "warnings": warnings,
        }),
    );
    Ok(())
}

/// The Run's `pending` Tasks whose dependency failed, each with the same
/// `nextAction` task-list gives it (`dispatch-show` on the failed dependency).
fn waiting_on_failed_dependency(engine: &Engine, run: &RunId) -> Result<Vec<Value>> {
    engine.store.read(|tx| {
        let mut waiting = Vec::new();
        for task in crate::engine::tasks::list_by_run(tx, run, Some("pending"))? {
            if let Some(failed) = crate::engine::tasks::failed_dependency(tx, &task.id)? {
                waiting.push(json!({
                    "taskId": task.id,
                    "failedDependency": failed,
                    "attention": "waiting_on_failed_dependency",
                    "nextAction": {
                        "kind": "dispatch-show",
                        "argv": ["pod", "dispatch-show", "--task", failed.as_str()],
                    },
                }));
            }
        }
        Ok(waiting)
    })
}

pub fn worker_stop(ctx: &Ctx, args: DispatchIdArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let dispatch = DispatchId::from_str(&args.dispatch)?;
    let (receipt, replayed) = mutation(
        ctx,
        &engine,
        "worker-stop",
        &json!({"dispatch": &dispatch}),
        || lifecycle::stop(&engine, &dispatch),
    )?;
    let ok = receipt.outcome != "stop_unknown";
    emit_worker_receipt(ctx, &receipt, replayed)?;
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

pub fn worker_abandon(ctx: &Ctx, args: DispatchIdArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let dispatch = DispatchId::from_str(&args.dispatch)?;
    let (receipt, replayed) = mutation(
        ctx,
        &engine,
        "worker-abandon",
        &json!({"dispatch": &dispatch}),
        || lifecycle::abandon(&engine, &dispatch),
    )?;
    emit_worker_receipt(ctx, &receipt, replayed)
}

pub fn worker_retain(ctx: &Ctx, args: WorkerRetainArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let dispatch = DispatchId::from_str(&args.dispatch)?;
    let reason = args.reason.unwrap_or_else(|| "coordinator retained".into());
    let (receipt, replayed) = mutation(
        ctx,
        &engine,
        "worker-retain",
        &json!({"dispatch": &dispatch, "reason": &reason}),
        || lifecycle::retain(&engine, &dispatch, &reason),
    )?;
    emit_worker_receipt(ctx, &receipt, replayed)
}

pub fn worker_release(ctx: &Ctx, args: DispatchIdArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let dispatch = DispatchId::from_str(&args.dispatch)?;
    let (receipt, replayed) = mutation(
        ctx,
        &engine,
        "worker-release",
        &json!({"dispatch": &dispatch}),
        || lifecycle::release(&engine, &dispatch),
    )?;
    let ok = receipt.outcome != "release_unknown";
    emit_worker_receipt(ctx, &receipt, replayed)?;
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

pub fn worker_gc(ctx: &Ctx, args: WorkerGcArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let run = args.run.as_deref().map(RunId::from_str).transpose()?;
    let (receipt, replayed) = mutation(
        ctx,
        &engine,
        "worker-gc",
        &json!({"run": &run, "include_retained": args.include_retained}),
        || lifecycle::gc(&engine, run.as_ref(), false, args.include_retained),
    )?;
    let ok = receipt
        .released
        .iter()
        .all(|worker| worker.outcome != "release_unknown");
    super::emit_mutation(ctx, &receipt, replayed)?;
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

pub fn dispatch(ctx: &Ctx, args: DispatchArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let task = TaskId::from_str(&args.task)?;
    let inject = !args.no_inject;
    let payload = json!({"task": &task, "to": &args.to, "inject": inject, "dryRun": args.dry_run, "returnPreamble": args.return_preamble});
    let (receipt, replayed) = if args.dry_run {
        (
            lifecycle::dispatch(&engine, &task, &args.to, inject, true, args.return_preamble)?,
            false,
        )
    } else {
        mutation(ctx, &engine, "dispatch", &payload, || {
            lifecycle::dispatch(
                &engine,
                &task,
                &args.to,
                inject,
                false,
                args.return_preamble,
            )
        })?
    };
    // `dispatched_unobserved` and a failed injection both exit 1.
    let observed = receipt.outcome.is_none();
    super::emit_mutation(ctx, &receipt, replayed)?;
    if !observed {
        std::process::exit(1);
    }
    Ok(())
}

pub fn dispatch_show(ctx: &Ctx, args: DispatchShowArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let task = TaskId::from_str(&args.task)?;
    let rows = engine.store.read(|tx| {
        if crate::engine::tasks::get(tx, &task)?.is_none() {
            return Err(PodError::new(
                ErrorCode::TaskNotFound,
                format!("Task {task} was not found"),
                &["Run pod task-list and retry with a valid Task ID."],
            ));
        }
        dispatches::list_by_task(tx, &task)
    })?;
    let dispatches = rows
        .into_iter()
        .map(|row| {
            let active = matches!(row.status.as_str(), "pending" | "dispatched");
            let mut value =
                serde_json::to_value(row).map_err(|error| PodError::store(error.to_string()))?;
            value["active"] = active.into();
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    // Orca shows one Dispatch: the active one, else the newest.
    let current = dispatches
        .iter()
        .rev()
        .find(|row| row["active"] == true)
        .or(dispatches.last())
        .map(|row| {
            let mut row = row.clone();
            row.as_object_mut().map(|fields| fields.remove("active"));
            row
        });
    output::emit_ok(
        ctx.json,
        &json!({"task_id": task, "dispatch": current, "dispatches": dispatches}),
    );
    Ok(())
}

fn parse_worktree(value: &str) -> Result<Worktree> {
    match value {
        "current" => Ok(Worktree::Current),
        "new-child" => Ok(Worktree::NewChild),
        "new-top-level" => Ok(Worktree::NewTopLevel),
        _ if value.starts_with("path:") => Ok(Worktree::Path(PathBuf::from(&value[5..]))),
        _ if value.starts_with("branch:") => Ok(Worktree::Branch(value[7..].to_owned())),
        _ => Err(PodError::usage("invalid --worktree value")),
    }
}

fn parse_setup(value: Option<&str>) -> Result<Setup> {
    match value.unwrap_or("run") {
        "run" => Ok(Setup::Run),
        "skip" => Ok(Setup::Skip),
        "inherit" => Ok(Setup::Inherit),
        _ => Err(PodError::usage("--setup must be run, skip, or inherit")),
    }
}

fn parse_deps(value: Option<&str>) -> Result<Vec<TaskId>> {
    value
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| PodError::usage(format!("--deps must be a JSON array: {error}")))
        .map(Option::unwrap_or_default)
}

fn mutation<T>(
    ctx: &Ctx,
    engine: &Engine,
    method: &str,
    payload: &serde_json::Value,
    f: impl FnOnce() -> Result<T>,
) -> Result<(T, bool)>
where
    T: Serialize + DeserializeOwned,
{
    let Some(request) = ctx.retry_request.as_deref() else {
        return f().map(|receipt| (receipt, false));
    };
    let request = RequestId::from_str(request)?;
    let caller = engine.caller.as_ref().ok_or_else(|| {
        PodError::new(
            ErrorCode::NotInHerdr,
            "mutation receipts require a caller inside herdr",
            &["Run the command from a pane inside herdr."],
        )
    })?;
    let fingerprint = caller.fingerprint();
    let hash = receipts::payload_hash(payload);
    let existing = engine.store.tx(|tx| {
        if let Some(row) = receipt_rows::get(tx, &fingerprint, &request)? {
            if row.method != method || row.payload_hash != hash {
                return Err(PodError::usage(format!(
                    "--retry-request belongs to method {}, not {method}, or its payload differs",
                    row.method
                )));
            }
            return Ok(row.receipt);
        }
        let now = engine.clock.now();
        receipt_rows::insert(
            tx,
            &receipt_rows::ReceiptRow {
                caller_fingerprint: fingerprint.clone(),
                request_id: request.clone(),
                method: method.into(),
                payload_hash: hash.clone(),
                state: "pending".into(),
                receipt: None,
                created_at: now,
                updated_at: now,
            },
        )?;
        Ok(None)
    })?;
    if let Some(receipt) = existing {
        return serde_json::from_value(receipt)
            .map(|receipt| (receipt, true))
            .map_err(|error| PodError::store(format!("invalid stored receipt: {error}")));
    }
    let value = f()?;
    let encoded = serde_json::to_value(&value)
        .map_err(|error| PodError::store(format!("could not serialize receipt: {error}")))?;
    engine.store.tx(|tx| {
        receipt_rows::update_state(tx, &fingerprint, &request, "completed")?;
        receipt_rows::update_receipt(tx, &fingerprint, &request, Some(&encoded))?;
        receipt_rows::update_updated_at(tx, &fingerprint, &request, engine.clock.now())?;
        receipt_rows::prune(tx, engine.clock.now())
    })?;
    Ok((value, false))
}
