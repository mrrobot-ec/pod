use std::path::PathBuf;

use crate::engine::Engine;
use crate::error::{PodError, Result};
use crate::ids::DispatchId;
use crate::store::rows::workers::WorkerRow;
use crate::store::rows::{dispatches, runs, workers};
use crate::transcript::{Reader, reader_for};

/// A session's reader and the transcript file it found.
pub type Located = (Box<dyn Reader>, PathBuf);

struct Proof {
    session_id: String,
    session_path: Option<PathBuf>,
    /// The agent kind herdr detected, used when the Dispatch has none.
    kind: Option<String>,
}

/// The transcript of a worker's session and the reader for it, when the
/// session is proven and a reader finds the file. `None` says only that there
/// is nothing to read; `worker-read` needs the reason too, see
/// [`locate_with_reason`]. The working directory is the worker's worktree,
/// else the Run's `home_cwd`.
pub fn locate_for_worker(
    engine: &Engine,
    dispatch: &DispatchId,
    kind: Option<&str>,
    released: bool,
    warnings: &mut Vec<String>,
) -> Result<Option<Located>> {
    Ok(locate_with_reason(engine, dispatch, kind, released, warnings)?.ok())
}

/// Proof of session, in the order of spec section 11. herdr is asked only
/// while the worker is unreleased. The inner `Err` carries the
/// `fallbackReason`.
pub fn locate_with_reason(
    engine: &Engine,
    dispatch: &DispatchId,
    kind: Option<&str>,
    released: bool,
    warnings: &mut Vec<String>,
) -> Result<std::result::Result<Located, &'static str>> {
    let (worker, cwd) = engine.store.read(|tx| {
        let row = dispatches::get(tx, dispatch)?
            .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} was not found")))?;
        let worker = workers::get(tx, dispatch)?
            .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} has no worker to read")))?;
        let home = runs::get(tx, &row.run_id)?.map(|run| run.home_cwd);
        let cwd = worker.worktree_path.clone().or(home).unwrap_or_default();
        Ok((worker, PathBuf::from(cwd)))
    })?;
    let live = if released {
        None
    } else {
        herdr_proof(engine, &worker, warnings)
    };
    let Some(proof) = live.or_else(|| stored_proof(&worker)) else {
        return Ok(Err("session_unproven"));
    };
    let Some(reader) = kind.or(proof.kind.as_deref()).and_then(reader_for) else {
        return Ok(Err("reader_unavailable"));
    };
    let located = reader
        .locate(&proof.session_id, proof.session_path.as_deref(), &cwd)
        .ok()
        .flatten();
    Ok(match located {
        Some(path) => Ok((reader, path)),
        None => Err("reader_unavailable"),
    })
}

/// The proof pod recorded: a session it assigned at launch, or herdr's
/// report kept at release. A session ID with no proof value is not proof.
fn stored_proof(worker: &WorkerRow) -> Option<Proof> {
    if !matches!(
        worker.transcript_proof.as_deref(),
        Some("herdr" | "pod_assigned")
    ) || (worker.session_id.is_none() && worker.transcript_path.is_none())
    {
        return None;
    }
    Some(Proof {
        session_id: worker.session_id.clone().unwrap_or_default(),
        session_path: worker.transcript_path.as_ref().map(PathBuf::from),
        kind: None,
    })
}

fn herdr_proof(engine: &Engine, worker: &WorkerRow, warnings: &mut Vec<String>) -> Option<Proof> {
    let pane = worker.pane_id.as_deref()?;
    let agents = match engine.herdr.agent_list() {
        Ok(agents) => agents,
        Err(error) => {
            warnings.push(format!(
                "herdr session proof unavailable: {}",
                error.message
            ));
            return None;
        }
    };
    let agent = agents.into_iter().find(|agent| agent.pane_id == pane)?;
    if agent.session_id.is_none() && agent.session_path.is_none() {
        return None;
    }
    Some(Proof {
        session_id: agent.session_id.unwrap_or_default(),
        session_path: agent.session_path,
        kind: Some(agent.kind),
    })
}
