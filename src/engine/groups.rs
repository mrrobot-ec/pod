use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{DispatchId, RunId};
use crate::store::Tx;
use crate::store::rows::{dispatches, observations, workers};

/// The live Dispatches of `run` that `group` names, oldest first, excluding
/// the sender's own terminal. A member is live when its Dispatch is
/// `dispatched` and its worker `ready` with `terminal_state = active`; `@idle`
/// also needs the latest observation `present` and `idle_at_prompt`.
pub fn resolve_group(
    tx: &Tx<'_>,
    run: &RunId,
    group: &str,
    sender_terminal: &str,
) -> Result<Vec<DispatchId>> {
    enum Filter<'a> {
        All,
        Idle,
        Worktree(&'a str),
        Kind(&'a str),
    }
    let filter = if group == "@all" {
        Filter::All
    } else if group == "@idle" {
        Filter::Idle
    } else if let Some(path) = group.strip_prefix("@worktree:") {
        Filter::Worktree(path)
    } else if let Some(kind) = group.strip_prefix('@') {
        Filter::Kind(kind)
    } else {
        return Err(invalid_group(group));
    };

    let mut members = Vec::new();
    for dispatch in dispatches::list_by_run(tx, run)? {
        if dispatch.status != "dispatched"
            || dispatch.assignee_terminal_id.as_deref() == Some(sender_terminal)
        {
            continue;
        }
        let Some(worker) = workers::get(tx, &dispatch.id)? else {
            continue;
        };
        if worker.state != "ready" || worker.terminal_state != "active" {
            continue;
        }
        let member = match filter {
            Filter::All => true,
            Filter::Idle => observations::get(tx, &dispatch.id)?.is_some_and(|observation| {
                observation.activity == "idle_at_prompt"
                    && observation.presence.as_deref() == Some("present")
            }),
            Filter::Worktree(path) => worker.worktree_path.as_deref() == Some(path),
            Filter::Kind(kind) => dispatch.agent_kind.as_deref() == Some(kind),
        };
        if member {
            members.push(dispatch.id);
        }
    }
    if members.is_empty() {
        return Err(invalid_group(group));
    }
    Ok(members)
}

fn invalid_group(group: &str) -> PodError {
    PodError::new(
        ErrorCode::InvalidRecipient,
        format!("group {group} resolved to no live recipients"),
        &["Address a live worker directly or choose a group with live workers."],
    )
}
