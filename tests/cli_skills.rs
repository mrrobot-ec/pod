use assert_cmd::Command;
use serde_json::Value;

const REFERENCES: [&str; 9] = [
    "commands-messages",
    "commands-workers",
    "commands",
    "config",
    "errors",
    "state-machines",
    "transcript-formats",
    "contract-capture",
    "herdr-adapter",
];

fn pod(args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("pod").unwrap().args(args).assert()
}

fn stdout_of(args: &[&str]) -> String {
    let out = pod(args).success().get_output().stdout.clone();
    String::from_utf8(out).unwrap()
}

fn doc(path: &str) -> String {
    std::fs::read_to_string(format!("{}/docs/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn skills_get_coordinator_and_worker_match_docs_guides() {
    for role in ["coordinator", "worker"] {
        let expected = doc(&format!("guides/{role}.md"));
        assert_eq!(stdout_of(&["skills", "get", role]), expected);
        assert_eq!(pod::skills::get(role, None, false).unwrap(), expected);
    }
    let json: Value =
        serde_json::from_str(&stdout_of(&["skills", "get", "worker", "--json"])).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["content"], doc("guides/worker.md"));
}

#[test]
fn skills_get_reference_and_full() {
    // Every page under docs/reference is servable; a new page needs an entry
    // in src/skills.rs.
    let mut on_disk: Vec<String> =
        std::fs::read_dir(format!("{}/docs/reference", env!("CARGO_MANIFEST_DIR")))
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                name.strip_suffix(".md").map(str::to_owned)
            })
            .collect();
    on_disk.sort();
    let mut listed = REFERENCES.to_vec();
    listed.sort();
    assert_eq!(on_disk, listed);

    for name in REFERENCES {
        let expected = doc(&format!("reference/{name}.md"));
        assert_eq!(
            stdout_of(&["skills", "get", "coordinator", "--reference", name]),
            expected
        );
    }
    for role in ["coordinator", "worker"] {
        let full = stdout_of(&["skills", "get", role, "--full"]);
        assert!(full.starts_with(&doc(&format!("guides/{role}.md"))));
        for name in REFERENCES {
            assert!(
                full.contains(&doc(&format!("reference/{name}.md"))),
                "{name}"
            );
        }
    }
    // --reference wins over --full.
    assert_eq!(
        stdout_of(&["skills", "get", "worker", "--reference", "errors", "--full"]),
        doc("reference/errors.md")
    );

    pod(&["skills", "get", "worker", "--reference", "nope", "--json"])
        .code(2)
        .stderr(predicates::str::contains("\"code\":\"usage\""));
    pod(&["skills", "get", "janitor"]).code(2);
    assert!(pod::skills::get("janitor", None, false).is_err());
}
