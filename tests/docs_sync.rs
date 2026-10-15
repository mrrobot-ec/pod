use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use pod::ErrorCode;

fn docs_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                // Plans, specs and review notes are history, not shipped docs.
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if !matches!(name.as_str(), "plans" | "specs" | "reviews") {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|ext| ext == "md") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // The README documents install and doctor, so it is a shipped doc too.
    let mut out = vec![root.join("README.md")];
    walk(&root.join("docs"), &mut out);
    out
}

fn docs_text() -> String {
    docs_files()
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn clap_flags() -> BTreeSet<String> {
    fn collect(cmd: &clap::Command, out: &mut BTreeSet<String>) {
        for arg in cmd.get_arguments() {
            if let Some(long) = arg.get_long() {
                out.insert(format!("--{long}"));
            }
        }
        for sub in cmd.get_subcommands() {
            collect(sub, out);
        }
    }
    let mut flags = BTreeSet::new();
    let mut cmd = pod::cli::clap_command();
    cmd.build();
    collect(&cmd, &mut flags);
    flags
}

/// Flags that belong to other tools (herdr, cargo, git, agent CLIs) or to the
/// install script, and are legitimately mentioned in docs.
const FOREIGN_FLAGS: &[&str] = &[
    "--help",
    "--version",
    "--ignored",
    "--test",
    "--session-id",
    "--skip-git-repo-check",
    "--dangerously-bypass-approvals-and-sandbox",
    "--dangerously-skip-permissions",
    "--no-daemon",
    // grok's, in config.md's example launch table.
    "--reasoning-effort",
    "--permission-mode",
    "--enabled",
    "--release",
];

fn tokens_starting_with_dashes(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'-');
        if boundary && bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2].is_ascii_lowercase()
        {
            let mut j = i + 2;
            while j < bytes.len()
                && (bytes[j].is_ascii_lowercase() || bytes[j].is_ascii_digit() || bytes[j] == b'-')
            {
                j += 1;
            }
            found.insert(text[i..j].trim_end_matches('-').to_owned());
            i = j;
        } else {
            i += 1;
        }
    }
    found
}

#[test]
fn docs_mention_no_removed_flag_or_code() {
    let text = docs_text();
    let flags = clap_flags();

    // Every flag that docs name is a real pod flag or a known foreign one.
    let unknown: Vec<_> = tokens_starting_with_dashes(&text)
        .into_iter()
        .filter(|flag| !flags.contains(flag) && !FOREIGN_FLAGS.contains(&flag.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "docs mention unknown flags: {unknown:?}"
    );

    // Every flag pod defines is documented.
    let documented = tokens_starting_with_dashes(&text);
    let missing: Vec<_> = flags.iter().filter(|f| !documented.contains(*f)).collect();
    assert!(missing.is_empty(), "flags missing from docs: {missing:?}");

    // Every error code is documented, and errors.md names no other code.
    let known: BTreeSet<_> = ErrorCode::ALL.iter().map(|c| code_name(*c)).collect();
    let errors_md =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/errors.md"))
            .unwrap();
    let rows: BTreeSet<_> = errors_md
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split('`').next())
        .map(str::to_owned)
        .collect();
    assert_eq!(rows, known, "errors.md rows must equal ErrorCode::ALL");

    // A removed code is not mentioned anywhere, quoted or not.
    assert!(
        !text.contains("not_implemented"),
        "docs still mention the removed not_implemented code"
    );

    // Quoted `"code": "x"` examples anywhere in docs use real codes.
    for part in text.split("\"code\"").skip(1) {
        let rest = part.trim_start_matches([':', ' ']);
        if let Some(rest) = rest.strip_prefix('"') {
            let name = rest.split('"').next().unwrap();
            assert!(
                known.contains(name) || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "docs show unknown error code {name}"
            );
        }
    }
}

#[test]
fn no_not_implemented_left() {
    fn walk(dir: &Path, hits: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                if text.contains("not_implemented") || text.contains("NotImplemented") {
                    hits.push(path.display().to_string());
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut hits,
    );
    assert!(hits.is_empty(), "not_implemented left in: {hits:?}");
}

/// Global constraints of the 0.2 plan: every file under `docs/` stays at or
/// under 400 lines except the design spec, and no doc, source file, or test
/// carries an em-dash (the vendored herdr schema is herdr's text).
#[test]
fn docs_stay_under_400_lines_and_nothing_carries_an_em_dash() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut docs = Vec::new();
    walk(&root.join("docs"), &mut docs);
    let long: Vec<_> = docs
        .iter()
        .filter(|path| !path.ends_with("docs/specs/2026-10-05-pod-design.md"))
        .filter_map(|path| {
            let lines = fs::read_to_string(path).ok()?.lines().count();
            (lines > 400).then(|| format!("{} ({lines})", path.display()))
        })
        .collect();
    assert!(long.is_empty(), "docs over 400 lines: {long:?}");

    let mut files = docs;
    for dir in ["src", "tests", "scripts", "skills"] {
        if root.join(dir).is_dir() {
            walk(&root.join(dir), &mut files);
        }
    }
    files.extend(["README.md", "CHANGELOG.md"].map(|name| root.join(name)));
    let dashed: Vec<_> = files
        .iter()
        .filter(|path| fs::read_to_string(path).is_ok_and(|text| text.contains('\u{2014}')))
        .map(|path| path.display().to_string())
        .collect();
    assert!(dashed.is_empty(), "em-dashes in: {dashed:?}");
}
