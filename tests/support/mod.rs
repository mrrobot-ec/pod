// Shared by every integration test crate; not every crate uses every item.
#![allow(dead_code)]

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pod::{Caller, Config, Engine, FakeClock, FakeHerdr, Store};

pub struct TestEnv {
    pub engine: Engine,
    pub herdr: Arc<FakeHerdr>,
    pub clock: Arc<FakeClock>,
    pub tmp: tempfile::TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().expect("create temporary test directory");
        let home = tmp.path().display().to_string();
        let state = tmp.path().join("state").display().to_string();
        let config_path = tmp.path().join("config.toml").display().to_string();
        let env = |name: &str| match name {
            "HOME" => Some(home.clone()),
            "POD_STATE_DIR" => Some(state.clone()),
            "POD_CONFIG" => Some(config_path.clone()),
            "HERDR_SOCKET_PATH" => Some("/tmp/herdr/test.sock".into()),
            _ => None,
        };
        let paths = pod::paths::resolve(&env).expect("resolve test paths");
        let store = Store::open(&paths).expect("open test store");
        let herdr = Arc::new(FakeHerdr::new());
        let clock = Arc::new(FakeClock(std::sync::Mutex::new(
            Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0)
                .single()
                .expect("valid test time"),
        )));
        let mut engine = Engine::new(
            store,
            Box::new((*herdr).clone()),
            Box::new(SharedClock(clock.clone())),
            Config::default(),
            paths,
            None,
        );
        engine.input_settle = std::time::Duration::ZERO;
        engine.tick_interval = std::time::Duration::from_millis(200);
        engine.ready_wait = std::time::Duration::from_millis(100);
        Self {
            engine,
            herdr,
            clock,
            tmp,
        }
    }

    /// A second engine on the same store, fake herdr, and clock: another
    /// `pod` process, such as the tab bar tick beside a waiting coordinator.
    pub fn second_engine(&self) -> Engine {
        let store = Store::open(&self.engine.paths).expect("open test store");
        let mut engine = Engine::new(
            store,
            Box::new((*self.herdr).clone()),
            Box::new(SharedClock(self.clock.clone())),
            Config::default(),
            self.engine.paths.clone(),
            None,
        );
        engine.input_settle = self.engine.input_settle;
        engine.tick_interval = self.engine.tick_interval;
        engine.ready_wait = self.engine.ready_wait;
        engine
    }

    pub fn as_caller(&mut self, terminal_id: &str, pid: u32) -> &mut Self {
        let pane_id = caller_pane(terminal_id);
        self.herdr.add_pane(&pane_id, terminal_id, "ws_fake");
        self.engine.caller = Some(Caller {
            terminal_id: terminal_id.into(),
            pane_id: Some(pane_id),
            pid,
            session: self.engine.paths.session.clone(),
        });
        self
    }

    /// A `pod` subprocess on this environment's store under the fake herdr.
    /// With `caller`, it runs as that terminal through the real identity path
    /// (`HERDR_ENV`, `HERDR_PANE_ID`, and a pane seeded by `POD_FAKE_PANES`);
    /// without, it runs outside herdr.
    pub fn pod(&self, caller: Option<&str>) -> assert_cmd::Command {
        let mut command = assert_cmd::Command::cargo_bin("pod").expect("pod binary");
        command
            .env_clear()
            .env("HOME", self.tmp.path())
            .env("POD_STATE_DIR", self.tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/test.sock")
            .env("POD_FAKE_HERDR", "1");
        if let Some(terminal_id) = caller {
            command.envs(fake_caller_env(terminal_id));
        }
        command
    }
}

/// The fake pane a caller terminal runs in.
/// The brief a worker was told to read: the file a typed pointer names, or
/// the text itself when it is not a pointer.
pub fn brief_of(typed: &str) -> String {
    typed
        .split_once("stored at ")
        .and_then(|(_, rest)| rest.split_once(". Read that file"))
        .and_then(|(path, _)| std::fs::read_to_string(path).ok())
        .unwrap_or_else(|| typed.to_owned())
}

pub fn caller_pane(terminal_id: &str) -> String {
    format!("pane-{terminal_id}")
}

/// Environment that makes a `pod` subprocess under `POD_FAKE_HERDR=1` resolve
/// `terminal_id` as its caller: a seeded fake pane plus the herdr variables
/// `identity::resolve` reads.
pub fn fake_caller_env(terminal_id: &str) -> Vec<(&'static str, String)> {
    let pane = caller_pane(terminal_id);
    vec![
        ("HERDR_ENV", "1".to_owned()),
        ("HERDR_PANE_ID", pane.clone()),
        ("POD_FAKE_PANES", format!("{pane}={terminal_id}")),
    ]
}

/// Adds an idle fake agent (`name=kind:pane:pid`) to a `pod` subprocess's
/// fake herdr, for commands that look agents up.
pub fn with_fake_agent(
    command: &mut assert_cmd::Command,
    name: &str,
    kind: &str,
    pane: &str,
    pid: u32,
) {
    command.env("POD_FAKE_AGENTS", format!("{name}={kind}:{pane}:{pid}"));
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

struct SharedClock(Arc<FakeClock>);

impl pod::Clock for SharedClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        pod::Clock::now(self.0.as_ref())
    }
}
