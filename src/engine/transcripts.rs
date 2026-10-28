//! Finding a worker's transcript, and proving the brief reached it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::error::Result;
use crate::ids::DispatchId;
use crate::store::rows::{
    dispatches, runs,
    workers::{self, WorkerRow},
};
use crate::transcript::{Reader, reader_for};

/// How often the brief's proof reads the transcript again.
const PROOF_POLL: Duration = Duration::from_millis(200);
const PROOF_PAGE: u32 = 50;

struct Proof {
    session_id: String,
    session_path: Option<PathBuf>,
    /// The agent kind herdr detected, used when the Dispatch has none.
    kind: Option<String>,
}

/// A transcript's reader and file.
pub type Located = (Box<dyn Reader>, PathBuf);

/// What the brief's proof found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BriefProof {
    /// A user turn contains the text.
    Seen,
    /// The transcript was located but showed no such turn in the window.
    Missing,
    /// No reader for the kind, or no transcript could be located.
    Unproven,
}

/// The transcript reader and file of a Dispatch's worker, resolved as
/// `worker-read` does: the worktree path, else the Run's `home_cwd`. `None`
/// when there is no session proof, no reader for the kind, or no file yet.
pub fn locate_for_worker(
    engine: &Engine,
    dispatch: &DispatchId,
    kind: Option<&str>,
    released: bool,
    warnings: &mut Vec<String>,
) -> Result<Option<Located>> {
    Ok(locate_with_reason(engine, dispatch, kind, released, warnings)?.ok())
}

/// `locate_for_worker`, keeping why nothing was found: the inner `Err` is
/// `worker-read`'s `fallbackReason`. Proof of session, in the order of spec
/// section 11; herdr is asked only while the worker is unreleased.
pub fn locate_with_reason(
    engine: &Engine,
    dispatch: &DispatchId,
    kind: Option<&str>,
    released: bool,
    warnings: &mut Vec<String>,
) -> Result<std::result::Result<Located, &'static str>> {
    let Some((worker, cwd)) = engine.store.read(|tx| {
        let Some(row) = dispatches::get(tx, dispatch)? else {
            return Ok(None);
        };
        let Some(worker) = workers::get(tx, dispatch)? else {
            return Ok(None);
        };
        let home = runs::get(tx, &row.run_id)?.map(|run| run.home_cwd);
        let cwd = worker.worktree_path.clone().or(home).unwrap_or_default();
        Ok(Some((worker, PathBuf::from(cwd))))
    })?
    else {
        return Ok(Err("session_unproven"));
    };
    Ok(locate(engine, kind, &cwd, &worker, released, warnings))
}

fn locate(
    engine: &Engine,
    kind: Option<&str>,
    cwd: &Path,
    worker: &WorkerRow,
    released: bool,
    warnings: &mut Vec<String>,
) -> std::result::Result<Located, &'static str> {
    let live = if released {
        None
    } else {
        herdr_proof(engine, worker, warnings)
    };
    let proof = live.or_else(|| stored_proof(worker));
    let proof = proof.ok_or("session_unproven")?;
    let reader = kind
        .or(proof.kind.as_deref())
        .and_then(reader_for)
        .ok_or("reader_unavailable")?;
    let path = reader
        .locate(&proof.session_id, proof.session_path.as_deref(), cwd)
        .ok()
        .flatten()
        .ok_or("reader_unavailable")?;
    Ok((reader, path))
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

/// Polls the Dispatch's transcript for up to `window` (one look at least)
/// for a user turn containing `needle`.
pub fn wait_for_user_text(
    engine: &Engine,
    dispatch: &DispatchId,
    kind: Option<&str>,
    needle: &str,
    window: Duration,
) -> Result<BriefProof> {
    let deadline = Instant::now() + window;
    let mut located = false;
    loop {
        // Warnings from a poll that finds nothing yet are not the receipt's.
        let mut ignored = Vec::new();
        if let Some((reader, path)) =
            locate_for_worker(engine, dispatch, kind, false, &mut ignored)?
        {
            located = true;
            if user_text_in(reader.as_ref(), &path, needle) {
                return Ok(BriefProof::Seen);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(if located {
                BriefProof::Missing
            } else {
                BriefProof::Unproven
            });
        }
        std::thread::sleep(PROOF_POLL.min(left));
    }
}

/// Whether any user row of the transcript contains `needle`, paging from
/// the start (a reused worker's transcript holds earlier turns).
fn user_text_in(reader: &dyn Reader, path: &Path, needle: &str) -> bool {
    let mut cursor: Option<String> = None;
    loop {
        let Ok(page) = reader.read(path, cursor.as_deref(), PROOF_PAGE) else {
            return false;
        };
        if page
            .rows
            .iter()
            .any(|row| row.role == "user" && row.text.contains(needle))
        {
            return true;
        }
        if page.content_complete || page.rows.is_empty() || page.cursor == cursor {
            return false;
        }
        cursor = page.cursor;
    }
}
