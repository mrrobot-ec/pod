//! Contract tests: Orca's recorded `--json` output against pod's, compared
//! for field names and JSON types only (spec section 17 item 2). Differences
//! from spec section 14 are listed in `allowed-differences.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// In the sample, absent from pod's output.
    Missing,
    /// In pod's output, absent from the sample.
    Extra,
    /// Present on both sides with different JSON types.
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub path: String,
    pub kind: DiffKind,
    pub detail: String,
}

/// One allowed difference. `path` is `$`, `$.field`, `$.array[].field`, or a
/// prefix ending in `.**` that covers a whole subtree. `kind` of `None`
/// allows every kind of difference at that path.
#[derive(Debug, Clone, Deserialize)]
pub struct AllowedEntry {
    pub command: String,
    pub path: String,
    #[serde(default)]
    pub kind: Option<DiffKind>,
    #[serde(default)]
    pub difference: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Allowed {
    #[serde(default, rename = "allow")]
    pub entries: Vec<AllowedEntry>,
}

impl Allowed {
    pub fn from_toml(text: &str) -> Self {
        toml::from_str(text).expect("parse allowed-differences.toml")
    }

    pub fn load() -> Self {
        Self::from_toml(&read_contract_file("allowed-differences.toml"))
    }

    /// Entries for one command (and `*` entries), ready to hand to `compare`.
    pub fn for_command(&self, command: &str) -> Allowed {
        Allowed {
            entries: self
                .entries
                .iter()
                .filter(|entry| entry.command == command || entry.command == "*")
                .cloned()
                .collect(),
        }
    }

    fn permits(&self, diff: &Diff) -> bool {
        self.entries.iter().any(|entry| {
            let path_matches = match entry.path.strip_suffix(".**") {
                Some(prefix) => {
                    diff.path == prefix
                        || diff.path.starts_with(&format!("{prefix}."))
                        || diff.path.starts_with(&format!("{prefix}["))
                }
                None => entry.path == diff.path,
            };
            path_matches && entry.kind.is_none_or(|kind| kind == diff.kind)
        })
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Compares field names and JSON types recursively and returns the
/// differences `allowed` does not permit.
///
/// A `null` on either side is compatible with any type: the sample records one
/// value, and a nullable field is not a type difference. Array elements are
/// compared to the sample's elements; an actual element is clean when it
/// matches any sample element, otherwise the closest one is reported. An empty
/// array on either side asserts nothing about its elements.
pub fn compare(sample: &Value, actual: &Value, allowed: &Allowed) -> Vec<Diff> {
    let mut diffs = Vec::new();
    walk(sample, actual, "$", allowed, &mut diffs);
    diffs
}

fn walk(sample: &Value, actual: &Value, path: &str, allowed: &Allowed, out: &mut Vec<Diff>) {
    let mut found = Vec::new();
    match (sample, actual) {
        (Value::Null, _) | (_, Value::Null) => {}
        (Value::Object(want), Value::Object(got)) => {
            for (key, want_value) in want {
                let child = format!("{path}.{key}");
                match got.get(key) {
                    Some(got_value) => walk(want_value, got_value, &child, allowed, &mut found),
                    None => found.push(Diff {
                        path: child,
                        kind: DiffKind::Missing,
                        detail: format!("field of type {} is missing", type_name(want_value)),
                    }),
                }
            }
            for (key, got_value) in got {
                if !want.contains_key(key) {
                    found.push(Diff {
                        path: format!("{path}.{key}"),
                        kind: DiffKind::Extra,
                        detail: format!(
                            "field of type {} is not in the sample",
                            type_name(got_value)
                        ),
                    });
                }
            }
        }
        (Value::Array(want), Value::Array(got)) => {
            let element_path = format!("{path}[]");
            if !want.is_empty() {
                for element in got {
                    let mut best: Option<Vec<Diff>> = None;
                    for candidate in want {
                        let mut attempt = Vec::new();
                        walk(candidate, element, &element_path, allowed, &mut attempt);
                        if best.as_ref().is_none_or(|b| attempt.len() < b.len()) {
                            best = Some(attempt);
                        }
                    }
                    for diff in best.unwrap_or_default() {
                        if !found.contains(&diff) {
                            found.push(diff);
                        }
                    }
                }
            }
        }
        (want, got) if type_name(want) == type_name(got) => {}
        (want, got) => found.push(Diff {
            path: path.to_owned(),
            kind: DiffKind::Type,
            detail: format!("sample is {}, pod is {}", type_name(want), type_name(got)),
        }),
    }
    out.extend(found.into_iter().filter(|diff| !allowed.permits(diff)));
}

fn contract_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contract")
}

fn read_contract_file(relative: &str) -> String {
    let path = contract_dir().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// One pod invocation run before the sample command, in the same state
/// directory. `bind` maps a placeholder name to a JSON pointer into the step's
/// output (pod's flat envelope minus `ok`); later argv may use `{name}`.
/// One `_setup` step: a pod command (`args`, run as terminal `as` when given,
/// else `CALLER`), or with `seed_worker` a stored worker instead of a command.
struct Step {
    args: Vec<String>,
    bind: BTreeMap<String, String>,
    as_terminal: Option<String>,
    seed_worker: Option<SeedWorker>,
}

/// `{"task": "{task}", "terminal": "...", "handle": "..."}`: stores a
/// `dispatched` Dispatch of the Task with a `ready`, `active` worker on that
/// terminal, standing in for `worker-start` (T7) so worker-side commands such
/// as `ask` can run. The agent PID is this test process, an ancestor of every
/// pod it runs. The step's `bind` reads `{"dispatch": {"id"}}`.
#[derive(Clone, Deserialize)]
struct SeedWorker {
    task: String,
    terminal: String,
    handle: String,
}

struct Sample {
    command: String,
    setup: Vec<Step>,
    args: Vec<String>,
    /// `_fake_herdr`: `POD_FAKE_*` variables for the sample command only, so
    /// the fake herdr reproduces the herdr behaviour Orca's capture saw (an
    /// agent that never became ready, a paste it could not confirm).
    fake_herdr: BTreeMap<String, String>,
    result: Value,
}

fn string_list(value: &Value, what: &str) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{what} is not an array"))
        .iter()
        .map(|arg| {
            arg.as_str()
                .unwrap_or_else(|| panic!("{what} holds a non-string"))
                .to_owned()
        })
        .collect()
}

/// Loads `samples/<name>.json`, strips the harness keys (`_provenance`,
/// `_setup`, `_args`, `_fake_herdr`) and Orca's envelope (`id`, `ok`, `_meta`), and keeps
/// `result`.
fn load_sample(name: &str) -> Sample {
    let file: Value = serde_json::from_str(&read_contract_file(&format!("samples/{name}.json")))
        .unwrap_or_else(|error| panic!("parse sample {name}: {error}"));
    let args = string_list(&file["_args"], &format!("sample {name} _args"));
    assert!(!args.is_empty(), "sample {name} has an empty _args");
    let setup =
        match &file["_setup"] {
            Value::Null => Vec::new(),
            steps => steps
                .as_array()
                .unwrap_or_else(|| panic!("sample {name} _setup is not an array"))
                .iter()
                .enumerate()
                .map(|(index, step)| Step {
                    args: match &step["args"] {
                        Value::Null => Vec::new(),
                        args => string_list(args, &format!("sample {name} _setup[{index}].args")),
                    },
                    as_terminal: step["as"].as_str().map(str::to_owned),
                    seed_worker: match &step["seed_worker"] {
                        Value::Null => None,
                        seed => Some(serde_json::from_value(seed.clone()).unwrap_or_else(
                            |error| panic!("sample {name} _setup[{index}].seed_worker: {error}"),
                        )),
                    },
                    bind: match &step["bind"] {
                        Value::Null => BTreeMap::new(),
                        bind => serde_json::from_value(bind.clone()).unwrap_or_else(|error| {
                            panic!("sample {name} _setup[{index}].bind: {error}")
                        }),
                    },
                })
                .collect(),
        };
    assert!(
        matches!(
            file["_provenance"].as_str(),
            Some("captured" | "hand-authored")
        ),
        "sample {name} needs _provenance captured or hand-authored"
    );
    assert!(
        file["result"].is_object(),
        "sample {name} has no result object"
    );
    let fake_herdr: BTreeMap<String, String> = match &file["_fake_herdr"] {
        Value::Null => BTreeMap::new(),
        fake => serde_json::from_value(fake.clone())
            .unwrap_or_else(|error| panic!("sample {name} _fake_herdr: {error}")),
    };
    assert!(
        fake_herdr.keys().all(|key| key.starts_with("POD_FAKE_")),
        "sample {name} _fake_herdr may only set POD_FAKE_* variables"
    );
    Sample {
        command: args[0].clone(),
        setup,
        args,
        fake_herdr,
        result: file["result"].clone(),
    }
}

/// `{name}` placeholders in `arg`: a brace, lowercase letters or `_`, a brace.
/// JSON never matches, since its object keys are quoted.
fn placeholders(arg: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close)
                if close > 0
                    && after[..close]
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_') =>
            {
                found.push(&after[..close]);
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    found
}

fn substitute(args: &[String], bindings: &BTreeMap<String, String>) -> Vec<String> {
    args.iter()
        .map(|arg| {
            let mut out = arg.clone();
            for name in placeholders(arg) {
                let value = bindings
                    .get(name)
                    .unwrap_or_else(|| panic!("placeholder {{{name}}} in {arg:?} is not bound"));
                out = out.replace(&format!("{{{name}}}"), value);
            }
            out
        })
        .collect()
}

/// The coordinator terminal every sample runs as, through a fake herdr pane.
const CALLER: &str = "term_contract";

/// An isolated pod environment: one temporary directory for state, config,
/// and `HOME`, shared by a sample's setup steps and its command.
struct PodEnv {
    tmp: tempfile::TempDir,
}

impl PodEnv {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().expect("temporary directory"),
        }
    }

    /// Runs `pod <args> --json` with the fake herdr, as the caller pane of
    /// terminal `CALLER` (`HERDR_ENV`, `HERDR_PANE_ID`, `POD_FAKE_PANES`). pod's success envelope
    /// is flat (`{"ok": true, ...fields}`), so `ok` is dropped to line up with
    /// Orca's `result`. Receipts for failed or unknown starts are printed on
    /// stdout with exit 1, so only an empty stdout is a hard failure.
    fn run(&self, args: &[String], fake_herdr: &BTreeMap<String, String>) -> (Option<i32>, Value) {
        self.run_with(CALLER, args, fake_herdr)
    }

    /// The store the pod subprocesses use (same env resolution).
    fn store(&self) -> pod::Store {
        let home = self.tmp.path().display().to_string();
        let state = self.tmp.path().join("state").display().to_string();
        let env = |name: &str| match name {
            "HOME" => Some(home.clone()),
            "POD_STATE_DIR" => Some(state.clone()),
            "HERDR_SOCKET_PATH" => Some("/tmp/herdr/contract.sock".to_owned()),
            _ => None,
        };
        pod::Store::open(&pod::paths::resolve(&env).expect("paths")).expect("store")
    }

    fn seed_worker(&self, seed: &SeedWorker) -> pod::ids::DispatchId {
        use pod::store::rows::{dispatches, tasks, workers};
        let task: pod::ids::TaskId = seed.task.parse().expect("seed_worker task ID");
        let now = chrono::Utc::now();
        self.store()
            .tx(|tx| {
                let row = tasks::get(tx, &task)?.expect("seeded task exists");
                tasks::update_status(tx, &task, "dispatched")?;
                let id = pod::ids::DispatchId::new();
                let seeded = id.clone();
                dispatches::insert(
                    tx,
                    &dispatches::DispatchRow {
                        id: id.clone(),
                        run_id: row.run_id,
                        task_id: task.clone(),
                        status: "dispatched".into(),
                        retry_of_dispatch_id: None,
                        failure_count: 0,
                        last_failure: None,
                        termination_reason: None,
                        termination_target: None,
                        depth: 1,
                        assignee_handle: Some(seed.handle.clone()),
                        assignee_terminal_id: Some(seed.terminal.clone()),
                        assignee_pid: Some(std::process::id()),
                        agent_kind: Some("codex".into()),
                        launch_requested: None,
                        launch_effective: None,
                        dispatched_at: Some(now),
                        last_heartbeat_at: None,
                        last_phase: None,
                        completed_at: None,
                        created_at: now,
                    },
                )?;
                workers::insert(
                    tx,
                    &workers::WorkerRow {
                        dispatch_id: id,
                        state: "ready".into(),
                        stage: None,
                        placement: None,
                        worktree_path: None,
                        workspace_id: None,
                        tab_id: None,
                        pane_id: Some(crate::support::caller_pane(&seed.terminal)),
                        terminal_id: Some(seed.terminal.clone()),
                        owns_pane: true,
                        user_owned: false,
                        user_owned_reason: None,
                        setup_state: None,
                        setup_pane_id: None,
                        preamble_input: None,
                        effects: None,
                        residual_resources: None,
                        terminal_state: "active".into(),
                        retained_reason: None,
                        session_id: None,
                        transcript_path: None,
                        transcript_proof: None,
                        last_error: None,
                        created_at: now,
                        updated_at: now,
                    },
                )?;
                Ok(seeded)
            })
            .expect("seed worker")
    }

    /// Runs `pod <args> --json` as the caller pane of `terminal`.
    fn run_as(&self, terminal: &str, args: &[String]) -> (Option<i32>, Value) {
        self.run_with(terminal, args, &BTreeMap::new())
    }

    fn run_with(
        &self,
        terminal: &str,
        args: &[String],
        fake_herdr: &BTreeMap<String, String>,
    ) -> (Option<i32>, Value) {
        let output = Command::cargo_bin("pod")
            .expect("pod binary")
            .env_clear()
            .envs(fake_herdr)
            .env("POD_FAKE_HERDR", "1")
            .env("POD_BRIEF_PROOF_MS", "0")
            .env("HOME", self.tmp.path())
            .env("POD_STATE_DIR", self.tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/contract.sock")
            .envs(crate::support::fake_caller_env(terminal))
            .args(args)
            .arg("--json")
            .timeout(std::time::Duration::from_secs(30))
            .output()
            .expect("run pod");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.trim().is_empty(),
            "pod {args:?} printed nothing on stdout (exit {:?}); stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        let mut value: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|error| panic!("pod {args:?} stdout is not JSON: {error}: {stdout}"));
        if let Some(object) = value.as_object_mut() {
            object.remove("ok");
        }
        (output.status.code(), value)
    }
}

fn check_sample(name: &str) {
    let sample = load_sample(name);
    let allowed = Allowed::load().for_command(&sample.command);
    let env = PodEnv::new();
    let mut bindings = BTreeMap::new();
    for (index, step) in sample.setup.iter().enumerate() {
        let output = if let Some(seed) = &step.seed_worker {
            let task = substitute(std::slice::from_ref(&seed.task), &bindings).remove(0);
            let id = env.seed_worker(&SeedWorker {
                task,
                ..seed.clone()
            });
            serde_json::json!({"dispatch": {"id": id.as_str()}})
        } else {
            let args = substitute(&step.args, &bindings);
            let (code, output) = env.run_as(step.as_terminal.as_deref().unwrap_or(CALLER), &args);
            assert_eq!(
                code,
                Some(0),
                "sample {name} setup step {index} pod {args:?} failed: {output}"
            );
            output
        };
        for (key, pointer) in &step.bind {
            let value = output
                .pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("sample {name} setup step {index}: no string at {pointer} in {output}")
                });
            bindings.insert(key.clone(), value.to_owned());
        }
    }
    let args = substitute(&sample.args, &bindings);
    let (_, actual) = env.run(&args, &sample.fake_herdr);
    let diffs = compare(&sample.result, &actual, &allowed);
    assert!(
        diffs.is_empty(),
        "pod {args:?} differs from the Orca sample {name}:\n{}",
        diffs
            .iter()
            .map(|diff| format!("  {} {:?}: {}", diff.path, diff.kind, diff.detail))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn compare_reports_missing_field_and_type_mismatch() {
    let sample = json!({"a": 1, "b": {"c": "x"}, "d": [{"e": true}], "n": null});
    let actual = json!({"a": "1", "b": {}, "d": [{"e": true}], "n": "later"});
    let diffs = compare(&sample, &actual, &Allowed::default());
    let found: Vec<_> = diffs.iter().map(|d| (d.path.as_str(), d.kind)).collect();
    assert!(found.contains(&("$.a", DiffKind::Type)), "{found:?}");
    assert!(found.contains(&("$.b.c", DiffKind::Missing)), "{found:?}");
    assert_eq!(
        found.len(),
        2,
        "null and matching array elements are clean: {found:?}"
    );

    let nested = compare(
        &json!({"rows": [{"id": "x", "n": 1}]}),
        &json!({"rows": [{"id": "x", "n": 1}, {"id": 7, "extra": true}]}),
        &Allowed::default(),
    );
    let paths: Vec<_> = nested.iter().map(|d| (d.path.as_str(), d.kind)).collect();
    assert!(
        paths.contains(&("$.rows[].id", DiffKind::Type)),
        "{paths:?}"
    );
    assert!(
        paths.contains(&("$.rows[].n", DiffKind::Missing)),
        "{paths:?}"
    );
    assert!(
        paths.contains(&("$.rows[].extra", DiffKind::Extra)),
        "{paths:?}"
    );
}

#[test]
fn allowed_difference_suppresses_diff() {
    let sample = json!({"task": {"id": "t", "status": "x"}, "gone": 1});
    let actual = json!({"task": {"id": 5, "status": "x", "attention": "a"}, "extra": {"k": 1}});
    let allowed = Allowed::from_toml(
        r#"
        [[allow]]
        command = "demo"
        path = "$.task.attention"
        kind = "extra"
        [[allow]]
        command = "demo"
        path = "$.extra.**"
        [[allow]]
        command = "other"
        path = "$.gone"
        "#,
    );
    let for_demo = allowed.for_command("demo");
    let found: Vec<_> = compare(&sample, &actual, &for_demo)
        .into_iter()
        .map(|d| (d.path, d.kind))
        .collect();
    // `$.extra` itself is also covered by `$.extra.**`; `$.gone` belongs to
    // another command; the type mismatch has no entry (an entry with a
    // different kind must not suppress it).
    assert_eq!(
        found,
        vec![
            ("$.gone".to_owned(), DiffKind::Missing),
            ("$.task.id".to_owned(), DiffKind::Type)
        ]
    );
    let wrong_kind = Allowed::from_toml(
        "[[allow]]\ncommand = \"demo\"\npath = \"$.task.id\"\nkind = \"missing\"\n",
    );
    assert!(
        compare(&sample, &actual, &wrong_kind)
            .iter()
            .any(|d| d.path == "$.task.id")
    );
}

fn sample_names() -> BTreeSet<String> {
    fs::read_dir(contract_dir().join("samples"))
        .expect("samples dir")
        .map(|entry| {
            let file = entry
                .expect("sample entry")
                .file_name()
                .into_string()
                .expect("utf-8 name");
            file.strip_suffix(".json")
                .unwrap_or_else(|| panic!("{file} in samples/ is not a .json sample"))
                .to_owned()
        })
        .collect()
}

/// `(test name, sample name)` for every `sample_test!` line below.
fn wired_tests() -> Vec<(String, String)> {
    include_str!("mod.rs")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("sample_test!("))
        .filter(|rest| !rest.starts_with('$'))
        .map(|rest| {
            let (test, rest) = rest
                .split_once(',')
                .expect("sample_test!(test, \"name\" ...)");
            let name = rest.split('"').nth(1).expect("quoted sample name");
            (test.trim().to_owned(), name.to_owned())
        })
        .collect()
}

#[test]
fn samples_are_well_formed_and_each_has_a_test() {
    let names = sample_names();
    for name in &names {
        load_sample(name);
    }
    let wired = wired_tests();
    let mut tested = BTreeSet::new();
    for (test, name) in &wired {
        assert_eq!(
            *test,
            format!("sample_{}", name.replace(['.', '-'], "_")),
            "test for sample {name} is misnamed"
        );
        assert!(names.contains(name), "{test} names a missing sample {name}");
        assert!(tested.insert(name.clone()), "sample {name} has two tests");
    }
    assert_eq!(
        tested, names,
        "every sample needs exactly one sample_test! line"
    );
    assert!(
        names.len() >= 30,
        "expected the captured set, found {}",
        names.len()
    );
}

/// A stand-in ID per placeholder name, in pod's ID format, for parsing argv
/// without running setup.
fn dummy_binding(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => "run_000000000000",
        "task" => "task_000000000000",
        "dispatch" => "ctx_000000000000",
        "message" => "msg_000000000000",
        "delivery" => "dlv_000000000000",
        "gate" => "gate_000000000000",
        "request" => "req_000000000000",
        _ => return None,
    })
}

/// Every setup step and sample command parses as a pod command line, and
/// every placeholder is bound by an earlier step. Runs without the commands
/// being implemented, so a sample cannot carry an Orca-only flag or ID.
#[test]
fn sample_argv_parse_as_pod_commands() {
    use clap::Parser;
    for name in sample_names() {
        let sample = load_sample(&name);
        let mut bindings = BTreeMap::new();
        let steps = sample.setup.iter().map(|step| (&step.args, &step.bind));
        let empty = BTreeMap::new();
        for (args, bind) in steps.chain([(&sample.args, &empty)]) {
            let argv = substitute(args, &bindings);
            if argv.is_empty() {
                // a seed_worker step: nothing to parse, but it may bind.
            } else if let Err(error) = pod::cli::Cli::try_parse_from(
                ["pod"]
                    .into_iter()
                    .chain(argv.iter().map(String::as_str))
                    .chain(["--json"]),
            ) {
                panic!("sample {name}: pod {argv:?} does not parse: {error}");
            }
            for (key, pointer) in bind {
                assert!(
                    pointer.starts_with('/'),
                    "sample {name}: bind {key} needs a JSON pointer"
                );
                let value = dummy_binding(key)
                    .unwrap_or_else(|| panic!("sample {name}: unknown placeholder name {key}"));
                bindings.insert(key.clone(), value.to_owned());
            }
        }
        assert_eq!(
            sample.command, sample.args[0],
            "sample {name}: the command is the first argument"
        );
    }
}

/// Every JSON path of `value`, with `[]` for array elements. An array's
/// element path is recorded even when it is empty.
fn sample_paths(value: &Value, path: &str, out: &mut BTreeSet<String>) {
    out.insert(path.to_owned());
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                sample_paths(child, &format!("{path}.{key}"), out);
            }
        }
        Value::Array(items) => {
            let element = format!("{path}[]");
            out.insert(element.clone());
            for item in items {
                sample_paths(item, &element, out);
            }
        }
        _ => {}
    }
}

fn parent_path(path: &str) -> &str {
    let cut = path.rfind('.').unwrap_or(0);
    &path[..cut.max(1)]
}

#[test]
fn allowed_differences_name_their_section_14_row() {
    let allowed = Allowed::load();
    assert!(!allowed.entries.is_empty());
    let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in sample_names() {
        let sample = load_sample(&name);
        sample_paths(
            &sample.result,
            "$",
            paths.entry(sample.command.clone()).or_default(),
        );
    }
    for entry in &allowed.entries {
        assert!(
            !entry.difference.trim().is_empty() && entry.path.starts_with('$'),
            "allowed entry for {} {} needs a `difference` and a `$` path",
            entry.command,
            entry.path
        );
        let known = paths.get(&entry.command).unwrap_or_else(|| {
            panic!("allowed entry names {}, which has no sample", entry.command)
        });
        let path = entry.path.strip_suffix(".**").unwrap_or(&entry.path);
        let anchored = match entry.kind {
            Some(DiffKind::Extra) => known.contains(parent_path(path)) && !known.contains(path),
            _ => known.contains(path),
        };
        assert!(
            anchored,
            "allowed entry {} {} {:?} does not fit the {} samples: an `extra` path \
             needs its parent in a sample and must not itself be there; any other \
             path must be in a sample",
            entry.command, entry.path, entry.kind, entry.command
        );
    }
}

/// One `#[test]` per sample. `ignore "T<n>"` marks a command whose task has
/// not landed; that task's reviewer deletes `, ignore "T<n>"` from the line.
macro_rules! sample_test {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            check_sample($name);
        }
    };
    ($test:ident, $name:literal, ignore $task:literal) => {
        #[test]
        #[ignore = $task]
        fn $test() {
            check_sample($name);
        }
    };
}

sample_test!(sample_run_create, "run-create");
sample_test!(sample_run_list, "run-list");
sample_test!(sample_run_show, "run-show");
sample_test!(sample_run_current, "run-current");
sample_test!(sample_task_create, "task-create");
sample_test!(sample_task_list, "task-list");
sample_test!(sample_task_list_brief, "task-list.brief");
sample_test!(sample_task_list_ready, "task-list.ready");
sample_test!(sample_gate_create, "gate-create");
sample_test!(sample_gate_resolve, "gate-resolve");
sample_test!(sample_gate_list, "gate-list");
sample_test!(sample_request_show, "request-show");
sample_test!(sample_send, "send");
sample_test!(sample_reply, "reply");
sample_test!(sample_inbox, "inbox");
sample_test!(sample_check_peek, "check.peek");
sample_test!(sample_check_wait_question, "check.wait-question");
#[rustfmt::skip]
sample_test!(sample_check_wait_heartbeats_and_question, "check.wait-heartbeats-and-question");
sample_test!(sample_check_wait_timed_out, "check.wait-timed-out");
sample_test!(sample_check_ack_wait_mixed, "check.ack-wait-mixed");
#[rustfmt::skip]
sample_test!(sample_check_ack_wait_worker_done, "check.ack-wait-worker-done");
sample_test!(sample_worker_start_ready, "worker-start.ready");
#[rustfmt::skip]
sample_test!(sample_worker_start_ready_worktree_path, "worker-start.ready-worktree-path");
#[rustfmt::skip]
sample_test!(sample_worker_start_failed_agent_readiness, "worker-start.failed-agent-readiness");
#[rustfmt::skip]
sample_test!(sample_worker_start_outcome_unknown, "worker-start.outcome-unknown");
sample_test!(sample_worker_show, "worker-show");
sample_test!(sample_worker_list, "worker-list");
sample_test!(sample_worker_release, "worker-release");
sample_test!(sample_dispatch_show, "dispatch-show");
sample_test!(sample_worker_read, "worker-read");
