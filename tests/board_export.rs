mod support;

use pod::board::{
    self,
    contract::{DispatchView, TaskView},
};
use pod::ids::RunId;
use serde_json::{Value, json};
use support::TestEnv;

const RUN: &str = "run_0123456789ab";
const AT: &str = "2026-10-12T12:00:00+00:00";

fn run_id() -> RunId {
    RUN.parse().unwrap()
}

fn seed_run(env: &TestEnv) {
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO runs (id,objective,coordinator_terminal_id,coordinator_handle,consumer_generation,home_cwd,depth,created_at,updated_at) VALUES (?1,'ship it','term_c','coord',1,'/tmp',0,?2,?2)",
                rusqlite::params![RUN, AT],
            )?;
            Ok(())
        })
        .unwrap();
}

fn task(env: &TestEnv, id: &str, parent: Option<&str>, status: &str, deps: &str) {
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO tasks (id,run_id,parent_id,title,spec,status,deps,created_by_terminal_id,created_at) \
                 VALUES (?1,?2,?3,?4,'spec',?5,?6,'term_c',?7)",
                rusqlite::params![id, RUN, parent, format!("title {id}"), status, deps, AT],
            )?;
            Ok(())
        })
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    env: &TestEnv,
    id: &str,
    task: &str,
    status: &str,
    retry_of: Option<&str>,
    reason: Option<&str>,
    created: &str,
) {
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO dispatches (id,run_id,task_id,status,retry_of_dispatch_id,failure_count,termination_reason,depth,assignee_handle,assignee_terminal_id,launch_effective,dispatched_at,created_at) \
                 VALUES (?1,?2,?3,?4,?5,0,?6,0,?7,?8,'{\"model\":\"opus\"}',?9,?9)",
                rusqlite::params![id, RUN, task, status, retry_of, reason, format!("h_{id}"), format!("term_{id}"), created],
            )?;
            Ok(())
        })
        .unwrap();
}

/// An accepted `worker_done` for `dispatch`, as T3 stores it in the Run mailbox.
fn worker_done(env: &TestEnv, id: &str, dispatch: &str, outcome: &str) {
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO messages (id,run_id,mailbox,from_handle,subject,type,priority,payload,created_at) \
                 VALUES (?1,?2,?3,'h','done','worker_done','normal',?4,?5)",
                rusqlite::params![
                    id,
                    RUN,
                    format!("run:{RUN}"),
                    json!({"dispatch_id": dispatch, "outcome": outcome}).to_string(),
                    AT
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

fn export(env: &TestEnv) -> Value {
    env.engine
        .store
        .read(|tx| board::export(tx, &run_id(), pod::Clock::now(env.engine.clock.as_ref())))
        .unwrap()
}

fn find<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap()
}

fn tid(n: u8) -> String {
    format!("task_00000000000{n}")
}
fn did(n: u8) -> String {
    format!("ctx_00000000000{n}")
}

#[test]
fn state_mapping_pending_ready_queued_dispatched_working_completed_done_failed_blocked() {
    let env = TestEnv::new();
    seed_run(&env);
    // dagr holds `working` to a working attempt (E150): a dispatched Task
    // whose Dispatch rows are gone is blocked on the coordinator, not working.
    for (i, (status, state)) in [
        ("pending", "queued"),
        ("ready", "queued"),
        ("dispatched", "blocked"),
        ("completed", "done"),
        ("failed", "failed"),
        ("blocked", "blocked"),
    ]
    .iter()
    .enumerate()
    {
        task(&env, &tid(i as u8), None, status, "[]");
        let v = export(&env);
        assert_eq!(find(&v, &tid(i as u8))["state"], *state, "status {status}");
    }
    assert_eq!(find(&export(&env), &tid(2))["unblock"], "coordinator");
    dispatch(&env, &did(2), &tid(2), "dispatched", None, None, AT);
    let v = export(&env);
    assert_eq!(find(&v, &tid(2))["state"], "working");
    assert!(find(&v, &tid(2)).get("unblock").is_none());
    dagr_check_strict(&v);
}

#[test]
fn attempt_cause_followup_or_sent_back_with_ref() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "dispatched", "[]");
    dispatch(
        &env,
        &did(1),
        &tid(1),
        "completed",
        None,
        Some("no_follow_on"),
        "2026-10-12T12:01:00+00:00",
    );
    dispatch(
        &env,
        &did(2),
        &tid(1),
        "failed",
        Some(&did(1)),
        Some("blocked_on"),
        "2026-10-12T12:02:00+00:00",
    );
    dispatch(
        &env,
        &did(3),
        &tid(1),
        "dispatched",
        Some(&did(2)),
        None,
        "2026-10-12T12:03:00+00:00",
    );
    // A Task started again without --retry-of still names its previous
    // attempt: dagr reserves `initial` for n = 1.
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET status='failed' WHERE id=?1",
                rusqlite::params![did(3)],
            )?;
            Ok(())
        })
        .unwrap();
    dispatch(
        &env,
        &did(4),
        &tid(1),
        "dispatched",
        None,
        None,
        "2026-10-12T12:04:00+00:00",
    );
    let v = export(&env);
    let a = find(&v, &tid(1))["attempts"].as_array().unwrap().clone();
    assert_eq!(a[0]["cause"], json!({"type": "initial"}));
    assert_eq!(a[1]["cause"], json!({"type": "followup", "ref": did(1)}));
    assert_eq!(a[2]["cause"], json!({"type": "sent_back", "ref": did(2)}));
    assert_eq!(a[3]["cause"], json!({"type": "sent_back", "ref": did(3)}));
    assert_eq!([a[0]["n"].clone(), a[2]["n"].clone()], [json!(1), json!(3)]);
    assert_eq!(a[0]["model"], "opus");
    dagr_check_strict(&v);
}

/// A worker's latest message is the task's `note` and counts as output for
/// the attempt's freshness.
#[test]
fn latest_worker_message_is_the_task_note_and_output_time() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "dispatched", "[]");
    dispatch(&env, &did(1), &tid(1), "dispatched", None, None, AT);
    let later = "2026-10-12T12:30:00+00:00";
    env.engine
        .store
        .tx(|tx| {
            for (id, kind, subject, at) in [
                ("msg_000000000001", "heartbeat", "alive", AT),
                ("msg_000000000002", "escalation", "Blocked: flaky test", later),
            ] {
                tx.0.execute(
                    "INSERT INTO messages (id,run_id,mailbox,from_handle,subject,type,priority,payload,created_at) \
                     VALUES (?1,?2,?3,'h',?4,?5,'normal',?6,?7)",
                    rusqlite::params![
                        id,
                        RUN,
                        format!("run:{RUN}"),
                        subject,
                        kind,
                        format!("{{\"dispatch_id\":\"{}\"}}", did(1)),
                        at
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let v = export(&env);
    let card = find(&v, &tid(1));
    assert_eq!(card["note"], "escalation: Blocked: flaky test");
    assert_eq!(card["attempts"][0]["liveness"]["last_output_at"], later);
    dagr_check_strict(&v);
}

/// dagr's model column is 12 cells and expects a `model·effort` chip.
#[test]
fn model_chip_is_family_version_and_effort() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "dispatched", "[]");
    dispatch(&env, &did(1), &tid(1), "dispatched", None, None, AT);
    for (launch, chip) in [
        (
            r#"{"model":"gpt-5.6-terra","effort":"high"}"#,
            "terra5.6·high",
        ),
        (
            r#"{"model":"gpt-6.1-sol","effort":"xhigh"}"#,
            "sol6.1·xhigh",
        ),
        (
            r#"{"model":"claude-opus-5-5","effort":"medium"}"#,
            "opus5.5·medium",
        ),
        (r#"{"model":"claude-sonnet-5-5"}"#, "sonnet5.5"),
        (
            r#"{"model":"something-else","effort":null}"#,
            "something-else",
        ),
    ] {
        env.engine
            .store
            .tx(|tx| {
                tx.0.execute(
                    "UPDATE dispatches SET launch_effective=?1 WHERE id=?2",
                    rusqlite::params![launch, did(1)],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            find(&export(&env), &tid(1))["attempts"][0]["model"],
            chip,
            "{launch}"
        );
    }
}

#[test]
fn exited_without_ending_is_lost_recorded_ending_keeps_outcome() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "dispatched", "[]");
    task(&env, &tid(2), None, "completed", "[]");
    dispatch(&env, &did(1), &tid(1), "dispatched", None, None, AT);
    dispatch(
        &env,
        &did(2),
        &tid(2),
        "completed",
        None,
        Some("no_follow_on"),
        AT,
    );
    worker_done(&env, "msg_000000000001", &did(2), "succeeded");
    env.engine
        .store
        .tx(|tx| {
            for d in [did(1), did(2)] {
                tx.0.execute(
                    "INSERT INTO observations VALUES (?1,'exited','unknown','exited','[]','herdr_state',NULL,NULL,NULL,?2)",
                    rusqlite::params![d, AT],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let v = export(&env);
    let lost = &find(&v, &tid(1))["attempts"][0];
    assert_eq!(lost["state"], "lost");
    assert!(lost.get("liveness").is_none(), "a lost attempt is not live");
    assert!(lost.get("locator").is_none());
    assert_eq!(lost["ended_at"], AT);
    assert!(lost.get("outcome").is_none());
    // A working Task whose attempt was lost is blocked on the coordinator.
    assert_eq!(find(&v, &tid(1))["state"], "blocked");
    assert_eq!(find(&v, &tid(1))["unblock"], "coordinator");
    let done = &find(&v, &tid(2))["attempts"][0];
    assert_eq!(done["state"], "done");
    assert_eq!(
        done["outcome"],
        json!({"result": "done", "evidence": "reported", "reason": "no_follow_on", "receipt": "worker_done accepted"})
    );
}

#[test]
fn fenced_or_gated_ending_is_recorded_not_reported() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "ready", "[]");
    task(&env, &tid(2), None, "blocked", "[]");
    task(&env, &tid(3), None, "ready", "[]");
    // worker-stop fences with `canceled` (D8); a gate completes with no reason (D6).
    dispatch(&env, &did(1), &tid(1), "failed", None, Some("canceled"), AT);
    dispatch(&env, &did(2), &tid(2), "completed", None, None, AT);
    // A rejected worker_done is not a report either.
    dispatch(&env, &did(3), &tid(3), "failed", None, Some("canceled"), AT);
    worker_done(&env, "msg_000000000003", &did(3), "succeeded");
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE messages SET rejected_reason='inactive_dispatch' WHERE id='msg_000000000003'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let v = export(&env);
    assert_eq!(
        find(&v, &tid(1))["attempts"][0]["outcome"],
        json!({"result": "failed", "evidence": "heuristic", "reason": "canceled", "receipt": "ending recorded by pod"})
    );
    assert_eq!(
        find(&v, &tid(2))["attempts"][0]["outcome"],
        json!({"result": "done", "evidence": "heuristic", "receipt": "ending recorded by pod"})
    );
    assert_eq!(
        find(&v, &tid(3))["attempts"][0]["outcome"]["evidence"],
        "heuristic"
    );
}

#[test]
fn missing_run_is_usage_error() {
    let env = TestEnv::new();
    let error = env
        .engine
        .store
        .read(|tx| board::export(tx, &run_id(), pod::Clock::now(env.engine.clock.as_ref())))
        .unwrap_err();
    assert_eq!(error.code, pod::error::ErrorCode::Usage);
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn pending_ask_blocks_task_with_unblock_coordinator_or_specialist() {
    let env = TestEnv::new();
    seed_run(&env);
    for n in 1..=3 {
        task(&env, &tid(n), None, "dispatched", "[]");
        dispatch(&env, &did(n), &tid(n), "dispatched", None, None, AT);
    }
    env.engine
        .store
        .tx(|tx| {
            let msg = |id: &str, mailbox: &str, thread: &str| {
                tx.0.execute(
                    "INSERT INTO messages (id,run_id,mailbox,from_handle,subject,type,priority,thread_id,created_at) \
                     VALUES (?1,?2,?3,'h','q','question','normal',?4,?5)",
                    rusqlite::params![id, RUN, mailbox, thread, AT],
                )
            };
            // Task 1 asks the coordinator; task 2 asks specialist dispatch 3 (copy in the Run mailbox).
            msg("msg_000000000001", &format!("run:{RUN}"), "thr_1")?;
            msg("msg_000000000002", &format!("dispatch:{}", did(3)), "thr_2")?;
            msg("msg_000000000003", &format!("run:{RUN}"), "thr_2")?;
            for (m, d) in [("msg_000000000001", did(1)), ("msg_000000000003", did(2))] {
                tx.0.execute(
                    "INSERT INTO questions (message_id,run_id,dispatch_id,asker_handle,status,created_at) \
                     VALUES (?1,?2,?3,'h','pending',?4)",
                    rusqlite::params![m, RUN, d, AT],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let v = export(&env);
    assert_eq!(find(&v, &tid(1))["state"], "blocked");
    assert_eq!(find(&v, &tid(1))["unblock"], "coordinator");
    assert_eq!(find(&v, &tid(2))["state"], "blocked");
    assert_eq!(find(&v, &tid(2))["unblock"], format!("h_{}", did(3)));
    assert_eq!(find(&v, &tid(3))["state"], "working");
    assert!(find(&v, &tid(3)).get("unblock").is_none());
}

#[test]
fn user_gate_with_task_exports_inputs_and_an_untasked_gate_is_its_own_card() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "blocked", "[]");
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO gates (id,run_id,task_id,audience,question,options,evidence,recommendation,status,created_at) \
                 VALUES ('gate_000000000001',?1,?2,'user','Ship?','[\"yes\",\"no\"]','doc.md','yes','pending',?3)",
                rusqlite::params![RUN, tid(1), AT],
            )?;
            tx.0.execute(
                "INSERT INTO gates (id,run_id,task_id,audience,question,options,evidence,recommendation,status,created_at) \
                 VALUES ('gate_000000000002',?1,NULL,'user','Orphan?','[\"a\"]','e','a','pending',?2)",
                rusqlite::params![RUN, AT],
            )?;
            Ok(())
        })
        .unwrap();
    let v = export(&env);
    let t = find(&v, &tid(1));
    assert_eq!(t["kind"], "question");
    assert_eq!(t["state"], "blocked");
    assert_eq!(t["unblock"], "user");
    assert_eq!(t["owner"], "user");
    assert_eq!(t["criteria"], "Ship?");
    assert_eq!(
        t["note"],
        "options: yes | no · recommended: yes · evidence: doc.md"
    );
    assert!(t.get("inputs").is_none(), "dagr inputs are task ids");
    assert_eq!(v["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(find(&v, "gate:gate_000000000002")["criteria"], "Orphan?");
}

#[test]
fn an_untasked_user_gate_is_a_question_card_and_a_decision_sidebar_status() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "dispatched", "[]");
    dispatch(&env, &did(1), &tid(1), "dispatched", None, None, AT);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO workers (dispatch_id,state,workspace_id,terminal_state,created_at,updated_at) \
                 VALUES (?1,'ready','ws_one','active',?2,?2)",
                rusqlite::params![did(1), AT],
            )?;
            Ok(())
        })
        .unwrap();
    let question = "Ship with the old limiter or wait for the new one?";
    let gate = env
        .engine
        .store
        .tx(|tx| {
            pod::engine::gates::create(
                tx,
                pod::Clock::now(env.engine.clock.as_ref()),
                &run_id(),
                "user",
                None,
                question,
                &["ship".into(), "wait".into()],
                Some("tests pass on both"),
                Some("ship"),
                true,
            )
        })
        .unwrap();
    let v = export(&env);
    let card = find(&v, &format!("gate:{}", gate.id));
    assert_eq!(
        (
            card["kind"].as_str(),
            card["state"].as_str(),
            card["unblock"].as_str(),
            card["owner"].as_str()
        ),
        (
            Some("question"),
            Some("blocked"),
            Some("user"),
            Some("user")
        )
    );
    assert_eq!(card["criteria"], question);
    assert_eq!(
        card["note"],
        "options: ship | wait · recommended: ship · evidence: tests pass on both"
    );
    assert!(card.get("project").is_none());
    dagr_check_strict(&v);
    let statuses = env
        .engine
        .store
        .read(|tx| board::sidebar_statuses(tx, &run_id()))
        .unwrap();
    assert_eq!(statuses.len(), 1);
    assert!(
        statuses.iter().all(|(_, status)| status
            .as_deref()
            .is_some_and(|s| s.starts_with("DECISION · Ship with the old limiter"))),
        "{statuses:?}"
    );
    // Spec A5.2: exactly the first 40 characters of the question.
    assert_eq!(
        statuses[0].1.as_deref().unwrap(),
        format!(
            "DECISION · {} · title task_000000000001 · working",
            question.chars().take(40).collect::<String>()
        ),
        "{statuses:?}"
    );
    env.engine
        .store
        .tx(|tx| {
            pod::engine::gates::resolve(
                tx,
                pod::Clock::now(env.engine.clock.as_ref()),
                &gate.id,
                "ship",
            )
        })
        .unwrap();
    let v = export(&env);
    assert!(
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["id"] != format!("gate:{}", gate.id))
    );
    let statuses = env
        .engine
        .store
        .read(|tx| board::sidebar_statuses(tx, &run_id()))
        .unwrap();
    assert!(
        statuses
            .iter()
            .all(|(_, status)| !status.as_deref().unwrap_or("").starts_with("DECISION"))
    );
}

#[test]
fn heartbeat_phase_is_progress_note() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "dispatched", "[]");
    dispatch(&env, &did(1), &tid(1), "dispatched", None, None, AT);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET last_phase='implementing', last_heartbeat_at=?1 WHERE id=?2",
                rusqlite::params![AT, did(1)],
            )?;
            Ok(())
        })
        .unwrap();
    let v = export(&env);
    assert_eq!(
        find(&v, &tid(1))["attempts"][0]["progress"],
        json!({"note": "implementing"})
    );
    assert_eq!(
        find(&v, &tid(1))["attempts"][0]["liveness"],
        json!({"prompt_acknowledged": false, "last_output_at": AT, "queued_input": 0})
    );
    let status = board::sidebar_status(
        &TaskView {
            id: tid(1),
            display_name: Some("T12".into()),
            title: None,
        },
        &DispatchView {
            status: "dispatched".into(),
            last_phase: Some("implementing".into()),
        },
    );
    assert_eq!(status, "T12 · implementing");
}

#[test]
fn export_validates_against_v3_schema() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(0), None, "dispatched", "[]");
    task(
        &env,
        &tid(1),
        Some(&tid(0)),
        "pending",
        &format!("[\"{}\"]", tid(0)),
    );
    task(&env, &tid(2), None, "blocked", "[]");
    dispatch(&env, &did(1), &tid(0), "dispatched", None, None, AT);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO events (run_id,at,type,task_id,dispatch_id,actor,detail) VALUES (?1,?2,'task_created',?3,NULL,'coord','{\"x\":1}')",
                rusqlite::params![RUN, AT, tid(0)],
            )?;
            tx.0.execute(
                "INSERT INTO gates (id,run_id,task_id,audience,question,options,evidence,recommendation,status,created_at) \
                 VALUES ('gate_000000000001',?1,?2,'user','Ship?','[\"yes\"]','e','yes','pending',?3)",
                rusqlite::params![RUN, tid(2), AT],
            )?;
            Ok(())
        })
        .unwrap();
    let v = export(&env);
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string("tests/board/dagr-v3.schema.json").unwrap())
            .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}\n{v:#}");
    assert_eq!(
        v["projects"],
        json!([{"id": tid(0), "title": format!("title {}", tid(0))}])
    );
    assert_eq!(v["dagr"], 3);
    assert_eq!(v["events"][0]["type"], "note");
    assert_eq!(v["events"][0]["detail"], "task_created: {\"x\":1}");
    dagr_check_strict(&v);
}

/// A `done` or `failed` Task whose latest attempt disagrees (settled by
/// `task-update` or a gate) gets a coordinator attempt, so dagr's
/// projection rule (E150) holds and the settlement is a visible claim.
#[test]
fn coordinator_settlement_without_a_worker_attempt_is_an_asserted_attempt() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "completed", "[]");
    task(&env, &tid(2), None, "failed", "[]");
    dispatch(
        &env,
        &did(2),
        &tid(2),
        "completed",
        None,
        Some("no_follow_on"),
        AT,
    );
    let v = export(&env);
    let settled = &find(&v, &tid(1))["attempts"][0];
    assert_eq!(settled["id"], format!("{}·a1", tid(1)));
    assert_eq!(settled["state"], "done");
    assert_eq!(settled["cause"], json!({"type": "initial"}));
    assert_eq!(settled["outcome"]["evidence"], "asserted");
    let failed = find(&v, &tid(2))["attempts"].as_array().unwrap().clone();
    assert_eq!(failed.len(), 2);
    assert_eq!(failed[1]["state"], "failed");
    assert_eq!(
        failed[1]["cause"],
        json!({"type": "followup", "ref": did(2)})
    );
    assert_eq!(find(&v, &tid(2))["state"], "failed");
    dagr_check_strict(&v);
}

/// `dagr check --strict` is the contract's own proof. It runs when a dagr
/// binary is on PATH (herdr-dagr 0.3.1 or newer); without one the derived
/// schema in tests/board is the stand-in.
fn dagr_check_strict(v: &Value) {
    let Some(dagr) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("dagr"))
            .find(|candidate| candidate.is_file())
    }) else {
        eprintln!("dagr not on PATH; skipped dagr check --strict");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("run.json");
    std::fs::write(&file, serde_json::to_vec(v).unwrap()).unwrap();
    let output = std::process::Command::new(dagr)
        .args(["check", "--strict", "--json"])
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.code() == Some(0) && stdout.trim() == "[]",
        "dagr check --strict: exit {:?}\n{stdout}\n{}\n{v:#}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timing test: run with --release")]
fn export_500_tasks_under_30ms() {
    let env = TestEnv::new();
    seed_run(&env);
    env.engine
        .store
        .tx(|tx| {
            for i in 0..500 {
                let t = format!("task_{i:012x}");
                let d = format!("ctx_{i:012x}");
                tx.0.execute(
                    "INSERT INTO tasks (id,run_id,title,spec,status,deps,created_by_terminal_id,created_at) VALUES (?1,?2,'t','s','dispatched','[]','c',?3)",
                    rusqlite::params![t, RUN, AT],
                )?;
                tx.0.execute(
                    "INSERT INTO dispatches (id,run_id,task_id,status,failure_count,depth,assignee_handle,assignee_terminal_id,last_phase,created_at) VALUES (?1,?2,?3,'dispatched',0,0,'h',?1,'p',?4)",
                    rusqlite::params![d, RUN, t, AT],
                )?;
                tx.0.execute(
                    "INSERT INTO observations VALUES (?1,'live','working','present','[]','herdr_state',NULL,NULL,NULL,?2)",
                    rusqlite::params![d, AT],
                )?;
                tx.0.execute(
                    "INSERT INTO events (run_id,at,type,task_id,actor) VALUES (?1,?2,'e',?3,'c')",
                    rusqlite::params![RUN, AT, t],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let now = pod::Clock::now(env.engine.clock.as_ref());
    let best = (0..5)
        .map(|_| {
            let start = std::time::Instant::now();
            env.engine
                .store
                .read(|tx| board::export(tx, &run_id(), now))
                .unwrap();
            start.elapsed()
        })
        .min()
        .unwrap();
    assert!(best.as_millis() < 30, "export took {best:?}");
}

#[test]
fn write_is_atomic_and_0600() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    seed_run(&env);
    let v = export(&env);
    let path = board::write(&env.engine.paths, &run_id(), &v).unwrap();
    assert_eq!(path, env.engine.paths.board_dir.join(RUN).join("run.json"));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
        v
    );
    // Rewrite replaces in place and leaves no temporary file behind.
    let v2 = json!({"version": 3, "again": true});
    board::write(&env.engine.paths, &run_id(), &v2).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
        v2
    );
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("run.json")]);
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn write_tightens_existing_run_dir_and_concurrent_writers_never_tear() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let dir = env.engine.paths.board_dir.join(RUN);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let paths = &env.engine.paths;
    let path = &dir.join("run.json");
    std::thread::scope(|scope| {
        for writer in 0..4 {
            scope.spawn(move || {
                for i in 0..50 {
                    let v = json!({"writer": writer, "i": i, "pad": "x".repeat(4096)});
                    board::write(paths, &run_id(), &v).unwrap();
                    // Every read parses: no reader ever sees a partial file.
                    let read: Value =
                        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                    assert_eq!(read["pad"].as_str().unwrap().len(), 4096);
                }
            });
        }
    });
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("run.json")]);
}

/// `pod board` with the same paths as `env`, and `PATH` limited to `bin`.
fn pod_board(env: &TestEnv, bin: &std::path::Path, args: &[&str]) -> std::process::Output {
    assert_cmd::Command::cargo_bin("pod")
        .unwrap()
        .arg("board")
        .args(args)
        .arg("--json")
        .env_clear()
        .env("HOME", env.tmp.path())
        .env("POD_STATE_DIR", env.tmp.path().join("state"))
        .env("HERDR_SOCKET_PATH", "/tmp/herdr/test.sock")
        .env("PATH", bin)
        .output()
        .unwrap()
}

#[test]
fn board_command_writes_run_json_and_reports_dagr() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "ready", "[]");
    let bin = env.tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();

    let output = pod_board(&env, &bin, &["--run", RUN]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let file = env.engine.paths.board_dir.join(RUN).join("run.json");
    assert_eq!(body["boardFile"], file.display().to_string());
    assert_eq!(body["opened"], false);
    assert_eq!(body["dagr"], "absent");
    assert!(body["install"].as_str().unwrap().contains("herdr-dagr"));
    let written: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(written["tasks"][0]["id"], tid(1));

    // pod never runs dagr itself; with herdr unreachable it cannot open a
    // tab for it either, so the board stays unopened with a warning.
    let dagr = bin.join("dagr");
    std::fs::write(&dagr, "#!/bin/sh\ntouch \"$0.ran\"\n").unwrap();
    std::fs::set_permissions(&dagr, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = pod_board(&env, &bin, &["--run", RUN]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["dagr"], "present");
    assert_eq!(body["opened"], false);
    assert!(body.get("install").is_none());
    assert!(
        body["warnings"][0]
            .as_str()
            .unwrap()
            .contains("dagr was not opened")
    );
    assert!(!bin.join("dagr.ran").exists());
}

#[test]
fn board_command_without_run_or_with_unknown_run_is_usage() {
    let env = TestEnv::new();
    let bin = env.tmp.path().join("bin");
    for args in [&[][..], &["--run", RUN][..], &["--run", "nope"][..]] {
        let output = pod_board(&env, &bin, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("\"code\":\"usage\""));
    }
}

/// Spec 12: `pod board` opens dagr in a new, focused tab of the caller's
/// workspace whose shell has `DAGR_RUN` set to the board file, typing the
/// `dagr` path through safe input; herdr's shell runs it, not pod.
#[test]
fn board_opens_dagr_in_a_tab_with_dagr_run() {
    let mut env = TestEnv::new();
    env.as_caller("term-coord", 42);
    let file = env.engine.paths.board_dir.join(RUN).join("run.json");
    let pane = pod::cli::commands::env::open_dagr(
        &env.engine,
        &file,
        std::path::Path::new("/opt/dagr bin/dagr"),
    )
    .unwrap();
    let calls = env.herdr.calls();
    assert!(calls.contains(&format!("tab_env:board:DAGR_RUN={}", file.display())));
    assert!(calls.contains(&"tab_focus:board".to_owned()));
    assert!(
        calls.contains(&format!("pane_send_keys:{pane}:Enter")),
        "{calls:?}"
    );
    assert!(
        env.herdr
            .submissions()
            .iter()
            .any(|(target, text)| *target == pane && text.contains("'/opt/dagr bin/dagr'"))
    );
}

#[test]
fn a_reported_cost_is_appended_to_the_task_note() {
    let env = TestEnv::new();
    seed_run(&env);
    task(&env, &tid(1), None, "dispatched", "[]");
    task(&env, &tid(2), None, "dispatched", "[]");
    dispatch(&env, &did(1), &tid(1), "dispatched", None, None, AT);
    dispatch(&env, &did(2), &tid(2), "dispatched", None, None, AT);
    // An earlier attempt in the same pane shares the session: counted once.
    dispatch(&env, &did(3), &tid(1), "failed", None, None, AT);
    env.engine
        .store
        .tx(|tx| {
            for (dispatch, session) in [
                (did(1), Some("sess-1")),
                (did(2), None),
                (did(3), Some("sess-1")),
            ] {
                tx.0.execute(
                    "INSERT INTO workers (dispatch_id,state,session_id,terminal_state,created_at,updated_at) \
                     VALUES (?1,'ready',?2,'active',?3,?3)",
                    rusqlite::params![dispatch, session, AT],
                )?;
            }
            tx.0.execute(
                "INSERT INTO messages (id,run_id,mailbox,from_handle,subject,type,priority,payload,created_at) \
                 VALUES ('msg_000000000001',?1,?2,'h','alive','heartbeat','normal',?3,?4)",
                rusqlite::params![
                    RUN,
                    format!("run:{RUN}"),
                    format!("{{\"dispatch_id\":\"{}\"}}", did(1)),
                    AT
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let usage = pod::usage::UsageStore::new(env.engine.paths.state_dir.join("usage"));
    let payload = r#"{"session_id":"sess-1","cost":{"total_cost_usd":3.1046},"rate_limits":{}}"#;
    let now = pod::Clock::now(env.engine.clock.as_ref());
    usage
        .save_session(&pod::usage::claude_session_from_statusline(payload, now).unwrap())
        .unwrap();
    let v = env
        .engine
        .store
        .read(|tx| board::export_with_costs(tx, &run_id(), now, &usage))
        .unwrap();
    assert_eq!(
        find(&v, &tid(1))["note"],
        "heartbeat: alive · $3.10 reported"
    );
    assert!(find(&v, &tid(2))["note"].is_null());
    dagr_check_strict(&v);
}
