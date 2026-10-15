mod support;

use assert_cmd::Command;
use predicates::prelude::*;
use support::TestEnv;

const RUN: &str = "run_000000000001";
const TASK: &str = "task_000000000001";
const DISPATCH: &str = "ctx_000000000001";

fn seed(env: &TestEnv) {
    let now = env.clock.0.lock().unwrap().to_rfc3339();
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'test', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)",
                (RUN, &now),
            )?;
            tx.0.execute(
                "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)",
                (TASK, RUN, &now),
            )?;
            tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, 'worker-a', 'term-worker', ?4, 'codex', ?5)",
                (DISPATCH, RUN, TASK, std::process::id(), &now),
            )?;
            tx.0.execute(
                "INSERT INTO workers (dispatch_id, state, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', 'term-worker', 'active', 1, ?2, ?2)",
                (DISPATCH, &now),
            )?;
            tx.0.execute(
                "INSERT INTO messages (id,run_id,mailbox,from_handle,subject,body,type,priority,read,created_at) VALUES ('msg_000000000001',?1,?2,'sender','full subject','full body','status','normal',0,?3)",
                (RUN, format!("dispatch:{DISPATCH}"), &now),
            )?;
            tx.0.execute(
                "INSERT INTO messages (id,run_id,mailbox,from_handle,subject,body,type,priority,read,created_at) VALUES ('msg_000000000002',?1,?2,'sender','run status','body','status','normal',0,?3)",
                (RUN, format!("run:{RUN}"), &now),
            )?;
            tx.0.execute(
                "INSERT INTO messages (id,run_id,mailbox,from_handle,subject,body,type,priority,read,created_at) VALUES ('msg_000000000003',?1,?2,'sender','run handoff','body','handoff','normal',0,?3)",
                (RUN, format!("run:{RUN}"), &now),
            )?;
            Ok(())
        })
        .unwrap();
}

fn command(env: &TestEnv) -> Command {
    let mut command = Command::cargo_bin("pod").unwrap();
    command
        .env("POD_STATE_DIR", env.tmp.path().join("state"))
        .env("POD_FAKE_HERDR", "1")
        .env("HERDR_SOCKET_PATH", "/tmp/herdr/test.sock")
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID");
    command
}

#[test]
fn send_typed_flags_and_payload_are_exclusive() {
    Command::cargo_bin("pod")
        .unwrap()
        .args([
            "send",
            "--subject",
            "s",
            "--payload",
            "{}",
            "--task-id",
            "task_000000000001",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn inbox_terminal_full() {
    let env = TestEnv::new();
    seed(&env);
    command(&env)
        .args(["inbox", "--terminal", "worker-a", "--full", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"body\":\"full body\""))
        .stdout(predicate::str::contains("\"mailbox\":\"dispatch:"));
}

#[test]
fn check_peek_types() {
    let env = TestEnv::new();
    seed(&env);
    command(&env)
        .args([
            "check", "--peek", "--types", "status", "--run", RUN, "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("run status"))
        .stdout(predicate::str::contains("run handoff").not());
}

#[test]
fn check_unknown_run_is_usage_error() {
    let env = TestEnv::new();
    seed(&env);
    for mode in ["--peek", "--all", "--unread"] {
        command(&env)
            .args(["check", mode, "--run", "run_ffffffffffff", "--json"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("\"code\":\"usage\""));
    }
}

#[test]
fn check_terminal_outside_herdr_is_refused() {
    let env = TestEnv::new();
    seed(&env);
    command(&env)
        .args(["check", "--terminal", "worker-a", "--json"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("\"code\":\"not_in_herdr\""));
}
