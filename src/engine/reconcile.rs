use serde::{Deserialize, Serialize};

use crate::engine::{Engine, lifecycle, observe, wake};
use crate::error::Result;
use crate::herdr::{AgentStatus, HerdrEvent};
use crate::ids::DispatchId;
use crate::store::rows::{dispatches, runs, workers};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub inspected: usize,
    pub present: usize,
    pub missing: usize,
    /// Active Dispatches with no recorded pane (or a pane with no detected
    /// agent): nothing proves presence or absence, so presence stays null.
    #[serde(default)]
    pub unproven: usize,
}

/// Reconcile durable active Dispatches against herdr without inventing a task
/// ending. A missing pane proves absence, not failure.
pub fn startup(engine: &Engine) -> Result<ReconcileReport> {
    let active = engine.store.read(|tx| {
        Ok(dispatches::list(tx, 100_000)?
            .into_iter()
            .filter(|row| matches!(row.status.as_str(), "pending" | "dispatched"))
            .collect::<Vec<_>>())
    })?;
    let mut report = ReconcileReport::default();
    if active.is_empty() {
        return Ok(report);
    }
    let agents = engine.herdr.agent_list()?;
    for dispatch in active {
        report.inspected += 1;
        let pane = engine
            .store
            .read(|tx| workers::get(tx, &dispatch.id))?
            .and_then(|worker| worker.pane_id);
        // No recorded pane is no evidence either way (a start still in
        // progress), so it is not reported absent.
        let Some(pane_id) = pane else {
            report.unproven += 1;
            continue;
        };
        let agent = agents.iter().find(|agent| agent.pane_id == pane_id);
        let (status, needs_input, presence) = if engine.herdr.pane_get(&pane_id)?.is_none() {
            report.missing += 1;
            engine
                .store
                .tx(|tx| lifecycle::pane_gone(tx, engine.clock.now(), &dispatch.id))?;
            (AgentStatus::Unknown, Vec::new(), Some("absent"))
        } else if let Some(agent) = agent {
            report.present += 1;
            (
                agent.status.clone(),
                agent.needs_input.clone(),
                Some("present"),
            )
        } else {
            // The pane exists but herdr detects no agent in it: presence is
            // not proven either way.
            report.unproven += 1;
            (AgentStatus::Unknown, Vec::new(), None)
        };
        engine.store.tx(|tx| {
            observe::record_herdr_state(
                tx,
                engine.clock.now(),
                &dispatch.id,
                status,
                &needs_input,
                presence,
            )
        })?;
    }
    Ok(report)
}

/// A herdr event, in the `pod hook event` process herdr runs for it: records
/// the observation and makes the wake attempt an idle agent is eligible for.
pub fn event(engine: &Engine, event: HerdrEvent) -> Result<()> {
    observe_event(engine, event, true)
}

/// A herdr event seen by a waiting `check --wait` or `ask`: records the
/// observation and the screen snapshot only. herdr runs the hook for the
/// same event, and that process makes the wake attempt (it may wait out
/// the quiet window; a waiter must not).
pub fn waiter_event(engine: &Engine, event: HerdrEvent) -> Result<()> {
    observe_event(engine, event, false)
}

fn observe_event(engine: &Engine, event: HerdrEvent, wake_now: bool) -> Result<()> {
    let pane_id = match &event {
        HerdrEvent::PaneExited { pane_id }
        | HerdrEvent::PaneClosed { pane_id }
        | HerdrEvent::AgentStatusChanged { pane_id, .. } => pane_id.clone(),
    };
    let Some(dispatch) = dispatch_for_pane(engine, &pane_id)? else {
        return coordinator_event(engine, &pane_id, &event, wake_now);
    };
    let (status, needs_input, presence, idle) = match event {
        HerdrEvent::PaneExited { .. } => (AgentStatus::Unknown, Vec::new(), "exited", false),
        // A closed pane is confirmed gone (7.6 `absent`).
        HerdrEvent::PaneClosed { .. } => (AgentStatus::Unknown, Vec::new(), "absent", false),
        HerdrEvent::AgentStatusChanged {
            status,
            needs_input,
            ..
        } => {
            let idle = is_idle(&status);
            (status, needs_input, "present", idle)
        }
    };
    let closed = presence == "absent";
    engine.store.tx(|tx| {
        // herdr reports no user input on a pane (protocol 22), so a close pod
        // did not make is the takeover it can see (spec 7.4).
        if closed {
            lifecycle::pane_gone(tx, engine.clock.now(), &dispatch)?;
        }
        observe::record_herdr_state(
            tx,
            engine.clock.now(),
            &dispatch,
            status,
            &needs_input,
            Some(presence),
        )
    })?;
    if idle {
        wake::note_screen(engine, &pane_id);
        if wake_now {
            let mailbox = format!("dispatch:{dispatch}");
            wake::on_trigger(engine, &mailbox, wake::Trigger::IdleHook)?;
        }
    }
    Ok(())
}

/// herdr reports an agent that finished its turn as `done` until someone
/// looks at the pane; it is at its prompt exactly like `idle`.
fn is_idle(status: &AgentStatus) -> bool {
    matches!(status, AgentStatus::Idle | AgentStatus::Done)
}

/// An idle coordinator pane is the Run mailbox's recipient going idle, which
/// is an eligible wake-ladder trigger for that mailbox (8.2).
fn coordinator_event(
    engine: &Engine,
    pane_id: &str,
    event: &HerdrEvent,
    wake_now: bool,
) -> Result<()> {
    let HerdrEvent::AgentStatusChanged { status, .. } = event else {
        return Ok(());
    };
    if !is_idle(status) {
        return Ok(());
    }
    let Some(pane) = engine.herdr.pane_get(pane_id)? else {
        return Ok(());
    };
    let Some(run) = engine
        .store
        .read(|tx| runs::bound_to_terminal(tx, &pane.terminal_id))?
    else {
        return Ok(());
    };
    wake::note_screen(engine, pane_id);
    if !wake_now {
        return Ok(());
    }
    wake::on_trigger(engine, &format!("run:{}", run.id), wake::Trigger::IdleHook)?;
    Ok(())
}

/// The Dispatch observed through `pane_id`: the active one when the pane has
/// hosted several, else the most recent.
fn dispatch_for_pane(engine: &Engine, pane_id: &str) -> Result<Option<DispatchId>> {
    engine.store.read(|tx| {
        let candidates = workers::list(tx, 100_000)?
            .into_iter()
            .filter(|worker| worker.pane_id.as_deref() == Some(pane_id))
            .map(|worker| worker.dispatch_id)
            .collect::<Vec<_>>();
        for id in &candidates {
            if dispatches::get(tx, id)?
                .is_some_and(|row| matches!(row.status.as_str(), "pending" | "dispatched"))
            {
                return Ok(Some(id.clone()));
            }
        }
        Ok(candidates.into_iter().next())
    })
}
