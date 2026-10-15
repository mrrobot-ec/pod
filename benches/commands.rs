//! Spec section 16 targets, measured against the real `pod` binary under the
//! fake herdr (so herdr round trips are excluded). Each command is timed
//! over a handful of runs and the median is asserted against the target
//! before criterion samples it, so `cargo test --release` (which runs this
//! target once) fails when a target is missed; debug builds only measure.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use pod::Store;

const RUN: &str = "run_000000000001";
const NON_WAITING_TARGET: Duration = Duration::from_millis(50);
const SAMPLES: usize = 15;

fn seed(home: &Path) {
    let state = home.join("state");
    let config = home.join("config.toml");
    let env = |name: &str| match name {
        "HOME" => Some(home.display().to_string()),
        "POD_STATE_DIR" => Some(state.display().to_string()),
        "POD_CONFIG" => Some(config.display().to_string()),
        "HERDR_SOCKET_PATH" => Some("/tmp/herdr/bench.sock".to_owned()),
        _ => None,
    };
    std::fs::write(&config, "").expect("write bench config");
    let paths = pod::paths::resolve(&env).expect("resolve bench paths");
    let store = Store::open(&paths).expect("open bench store");
    let now = "2026-10-12T12:00:00+00:00";
    store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'bench', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)",
                (RUN, now),
            )?;
            for index in 0..100u32 {
                let task = format!("task_{index:012x}");
                let dispatch = format!("ctx_{index:012x}");
                let handle = format!("worker-{index}");
                let terminal = format!("term-{index}");
                tx.0.execute(
                    "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)",
                    (&task, RUN, now),
                )?;
                tx.0.execute(
                    "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, ?4, ?5, 1, 'codex', ?6)",
                    (&dispatch, RUN, &task, &handle, &terminal, now),
                )?;
                tx.0.execute(
                    "INSERT INTO workers (dispatch_id, state, worktree_path, pane_id, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', '/tmp/work', ?2, ?3, 'active', 1, ?4, ?4)",
                    (&dispatch, format!("pane-{index}"), &terminal, now),
                )?;
            }
            Ok(())
        })
        .expect("seed bench store");
}

fn pod(home: &Path, args: &[&str]) -> Duration {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_pod"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("POD_STATE_DIR", home.join("state"))
        .env("POD_CONFIG", home.join("config.toml"))
        .env("HERDR_SOCKET_PATH", "/tmp/herdr/bench.sock")
        .env("POD_FAKE_HERDR", "1")
        // The coordinator's pane, and worker 0 idle at its prompt, so `send`
        // reaches the nudge's quiet check (spec 8.2) on the timed path.
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "pane-coordinator")
        .env(
            "POD_FAKE_PANES",
            "pane-coordinator=term-coordinator,pane-0=term-0",
        )
        .env("POD_FAKE_AGENTS", "worker-0=codex:pane-0:1")
        .output()
        .expect("run pod");
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "pod {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    elapsed
}

fn median(home: &Path, args: &[&str]) -> Duration {
    let mut times: Vec<Duration> = (0..SAMPLES).map(|_| pod(home, args)).collect();
    times.sort();
    times[SAMPLES / 2]
}

fn bench_non_waiting_command_under_50ms(c: &mut Criterion) {
    let tmp = tempfile::tempdir().expect("bench dir");
    seed(tmp.path());
    let mailbox = format!("dispatch:{}", "ctx_000000000000");
    let commands: [(&str, Vec<&str>); 4] = [
        ("status", vec!["status", "--json"]),
        ("task-list", vec!["task-list", "--run", RUN, "--json"]),
        ("worker-list", vec!["worker-list", "--run", RUN, "--json"]),
        // A mutating command that commits, exports the board, and makes the
        // first wake attempt for an idle worker: it never waits out the
        // 3-second quiet window (commands-messages.md).
        (
            "send",
            vec!["send", "--to", &mailbox, "--subject", "bench", "--json"],
        ),
    ];
    let mut group = c.benchmark_group("non_waiting_command");
    for (name, args) in &commands {
        let measured = median(tmp.path(), args);
        // A debug build on a slow or emulated machine is not what spec 16
        // measures; the target is asserted for release builds
        // (`cargo test --release`, `cargo bench`).
        assert!(
            cfg!(debug_assertions) || measured < NON_WAITING_TARGET,
            "{name} took {measured:?}, target is under {NON_WAITING_TARGET:?} (spec section 16)"
        );
        group.bench_function(*name, |b| b.iter(|| pod(tmp.path(), args)));
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench_non_waiting_command_under_50ms
}
criterion_main!(benches);
