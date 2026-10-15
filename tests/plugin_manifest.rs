use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest() -> toml::Table {
    let text = std::fs::read_to_string(root().join("herdr-plugin.toml")).unwrap();
    text.parse().unwrap()
}

fn argv(entry: &toml::Value) -> Vec<&str> {
    entry["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect()
}

#[test]
fn plugin_manifest_parses_and_lists_three_events_and_three_actions() {
    assert!(!root().join("plugin.toml").exists());
    let m = manifest();
    for key in ["id", "name", "version", "min_herdr_version"] {
        assert!(m[key].is_str(), "missing top-level {key}");
    }
    assert_eq!(m["min_herdr_version"].as_str(), Some("0.9.3"));
    let platforms: Vec<_> = m["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(platforms, ["macos", "linux"]);

    let build = m["build"].as_array().unwrap();
    assert_eq!(build.len(), 1);
    assert!(argv(&build[0]).contains(&"scripts/install.sh"));
    assert!(root().join("scripts/install.sh").is_file());

    let startup = m["startup"].as_array().unwrap();
    assert_eq!(startup.len(), 1);
    assert_eq!(argv(&startup[0]), ["pod", "hook", "startup"]);

    let events = m["events"].as_array().unwrap();
    let on: Vec<_> = events.iter().map(|e| e["on"].as_str().unwrap()).collect();
    assert_eq!(
        on,
        ["pane.exited", "pane.closed", "pane.agent_status_changed"]
    );
    for event in events {
        assert_eq!(argv(event), ["pod", "hook", "event"]);
    }

    let actions = m["actions"].as_array().unwrap();
    let ids: Vec<_> = actions.iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["open-board", "worker-list", "doctor"]);
    for action in actions {
        assert!(action["title"].is_str());
        assert!(!argv(action).is_empty());
    }

    // worker-list opens a popup pane (spec 3.5).
    let panes = m["panes"].as_array().unwrap();
    let pane = panes
        .iter()
        .find(|p| p["id"].as_str() == Some("worker-list"))
        .expect("worker-list pane");
    assert_eq!(pane["placement"].as_str(), Some("popup"));
    assert!(argv(pane).join(" ").contains("pod worker-list"));
}

#[test]
fn install_script_shellcheck_clean() {
    let script = root().join("scripts/install.sh");
    // A missing program is NotFound, or exit 127 where the spawn goes
    // through a shell or an emulator (qemu-user reports it that way).
    let Ok(output) =
        Command::new("shellcheck")
            .arg(&script)
            .output()
            .and_then(|output| match output.status.code() {
                Some(127) => Err(std::io::ErrorKind::NotFound.into()),
                _ => Ok(output),
            })
    else {
        eprintln!("shellcheck not installed; skipping");
        return;
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "needs a herdr binary and mutates its plugin registry; set POD_E2E=1"]
fn herdr_plugin_link_dry_run() {
    if std::env::var("POD_E2E").as_deref() != Ok("1") {
        eprintln!("POD_E2E!=1; skipping");
        return;
    }
    // herdr keeps one plugin registry per user under XDG_CONFIG_HOME. Point it
    // at a temp dir and drop inherited HERDR_* vars so the test never touches
    // the real registry or a live server.
    let home = tempfile::tempdir().unwrap();
    let herdr = |args: &[&str]| {
        let mut command = Command::new("herdr");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HERDR_") {
                command.env_remove(key);
            }
        }
        command
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_STATE_HOME", home.path().join("state"))
            .args(args)
            .output()
            .expect("herdr on PATH")
    };
    let repo = root();
    let link = herdr(&["plugin", "link", repo.to_str().unwrap(), "--disabled"]);
    let list = herdr(&["plugin", "list"]);
    // unlink needs a running server; uninstall does not, so fall back to it.
    let unlinked = herdr(&["plugin", "unlink", "pod"]);
    if !unlinked.status.success() {
        herdr(&["plugin", "uninstall", "pod"]);
    }
    assert!(
        link.status.success(),
        "link: {}",
        String::from_utf8_lossy(&link.stdout)
    );
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(listed.contains("pod (pod)"), "{listed}");
}
