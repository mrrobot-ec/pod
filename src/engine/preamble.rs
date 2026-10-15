use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{DispatchId, TaskId};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Specialist {
    pub dispatch_id: DispatchId,
    pub handle: String,
    pub task_title: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Decision {
    pub question: String,
    pub resolution: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreambleContext {
    pub pod: String,
    pub coordinator_handle: String,
    pub task_id: TaskId,
    pub dispatch_id: DispatchId,
    pub worker_handle: String,
    pub heartbeat_minutes: u64,
    pub specialists: Vec<Specialist>,
    pub decision: Option<Decision>,
    pub sub_dispatch: Option<String>,
    pub spec: String,
    /// Where a copy of this brief is stored (`lifecycle::brief_path`).
    pub brief_path: PathBuf,
}

pub fn render_preamble(ctx: &PreambleContext) -> String {
    let pod = &ctx.pod;
    let mut text = format!(
        "Please carry out this task from my pod coordinator by following the brief below. A copy is stored at {}; read it again after a context reset or compaction.\n\n\
You are working inside herdr as a dispatched worker.\n\
Your coordinator's handle is: {}\n\
Your task ID is: {}\n\
Your dispatch ID is: {}\n\
Your worker handle is: {}\n\n\
The coordinator cannot see this terminal. Reach it only with the commands below;\n\
a question or result left only in this terminal never gets to it.\n\
Do not post to Slack, GitHub, or other channels during the run.\n\n\
=== COMMANDS ===\n\
# Report the outcome. Required, exactly once. --body is three sentences: what you\n\
# did, what you found, what is left. Use --outcome failed when the work is not\n\
# complete; never encode failure only in prose and never silently exit. A failed\n\
# outcome must say why: --reason no_follow_on|blocked_on|denied|canceled|escalated|superseded|handed_off,\n\
# and blocked_on, handed_off, superseded also need --reason-target <task or dispatch id>.\n\
{pod} send --from {} --type worker_done --subject \"<short status>\" --body \"<three sentences>\" --task-id {} --dispatch-id {} --outcome succeeded [--reason <reason> --reason-target <id>]\n\n\
# Heartbeat every {} minutes while working. Skip while blocked in `ask` or `check --wait`.\n\
{pod} send --from {} --type heartbeat --subject \"alive\" --task-id {} --dispatch-id {} --phase \"<investigating|implementing|reviewing|waiting>\"\n\n\
# Ask the coordinator and wait for the answer. Use this instead of any local\n\
# question dialog. On timeout, resume with the returned message ID. To ask a\n\
# worker listed under SPECIALISTS instead, add --to dispatch:<its dispatch id>;\n\
# the coordinator is copied.\n\
{pod} ask --from {} --question \"<question>\" --options \"<a,b>\" --timeout-ms 600000 [--to dispatch:<id>]\n\n\
# Answer a specialist question received through check. Either copy's ID works.\n\
{pod} reply --id <message_id> --body \"<answer>\"\n\n\
# Escalate a blocker before completion. Evidence is a file path, a message ID,\n\
# or a worker-read cursor; recommendation is what you would do by default.\n\
{pod} send --from {} --type escalation --subject \"Blocked: <reason>\" --body \"<details>\" --evidence \"<ref>\" --recommendation \"<default>\" --task-id {} --dispatch-id {}\n\n\
# Read coordinator follow-ups. Nothing interrupts you: run this before starting a\n\
# new file, after a test run, and once more immediately before worker_done.\n\
{pod} check --terminal {} --json\n\n\
=== AFTER worker_done ===\n\
Stop and return to an idle prompt. Do not start new work, poll, or keep calling\n\
check. Do not exit. A direct instruction from the user is new work and must not\nreuse these IDs. The coordinator may re-engage this terminal with a fresh brief.\n",
        ctx.brief_path.display(),
        ctx.coordinator_handle,
        ctx.task_id,
        ctx.dispatch_id,
        ctx.worker_handle,
        ctx.worker_handle,
        ctx.task_id,
        ctx.dispatch_id,
        ctx.heartbeat_minutes,
        ctx.worker_handle,
        ctx.task_id,
        ctx.dispatch_id,
        ctx.worker_handle,
        ctx.worker_handle,
        ctx.task_id,
        ctx.dispatch_id,
        ctx.worker_handle,
    );
    if !ctx.specialists.is_empty() {
        text.push_str("\n=== SPECIALISTS ===\n");
        for specialist in &ctx.specialists {
            text.push_str(&format!(
                "{} {}: {}\n",
                specialist.dispatch_id, specialist.handle, specialist.task_title
            ));
        }
    }
    if let Some(decision) = &ctx.decision {
        text.push_str(&format!(
            "\n=== DECISION ===\nQuestion: {}\nResolution: {}\n",
            decision.question, decision.resolution
        ));
    }
    if let Some(sub_dispatch) = &ctx.sub_dispatch
        && !sub_dispatch.is_empty()
    {
        text.push_str("\n=== SUB-DISPATCH ===\n");
        text.push_str(sub_dispatch);
        text.push('\n');
    }
    text.push_str("\n=== TASK ===\n");
    text.push_str(&ctx.spec);
    if !ctx.spec.ends_with('\n') {
        text.push('\n');
    }
    text
}
