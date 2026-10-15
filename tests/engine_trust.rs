//! Folder trust for `--yolo` workers (spec section 4): the agent's own
//! config gets the entry a person's "trust" answer would have written.

use std::path::PathBuf;

use pod::engine::trust;

#[test]
fn codex_trust_is_appended_once_and_keeps_existing_content() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        "model = \"gpt-5.6-sol\"\n\n[projects.\"/already\"]\ntrust_level = \"trusted\"\n",
    )
    .unwrap();
    let folders = [PathBuf::from("/already"), PathBuf::from("/new/work tree")];
    let added = trust::ensure("codex", &config, &folders).unwrap();
    assert_eq!(added, [PathBuf::from("/new/work tree")]);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.starts_with("model = \"gpt-5.6-sol\""), "{text}");
    assert!(
        text.contains("[projects.\"/new/work tree\"]\ntrust_level = \"trusted\"\n"),
        "{text}"
    );
    let table: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        table["projects"]["/new/work tree"]["trust_level"].as_str(),
        Some("trusted")
    );
    assert_eq!(
        table["projects"]["/already"]["trust_level"].as_str(),
        Some("trusted")
    );
    // Idempotent.
    assert!(
        trust::ensure("codex", &config, &folders)
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
}

#[test]
fn codex_trust_creates_a_missing_config_as_0600_and_keeps_an_existing_mode() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join(".codex").join("config.toml");
    let added = trust::ensure("codex", &config, &[PathBuf::from("/repo")]).unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
    trust::ensure("codex", &config, &[PathBuf::from("/other")]).unwrap();
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn codex_trust_leaves_an_unparseable_config_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(&config, "this = [is not toml\n").unwrap();
    let error = trust::ensure("codex", &config, &[PathBuf::from("/repo")]).unwrap_err();
    assert!(error.message.contains("folder trust"), "{}", error.message);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "this = [is not toml\n"
    );
}

#[test]
fn claude_trust_sets_has_trust_dialog_accepted_and_keeps_other_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join(".claude.json");
    std::fs::write(
        &config,
        "{\"theme\":\"dark\",\"projects\":{\"/old\":{\"hasTrustDialogAccepted\":true,\"allowedTools\":[]}}}",
    )
    .unwrap();
    let folders = [PathBuf::from("/old"), PathBuf::from("/new")];
    let added = trust::ensure("claude", &config, &folders).unwrap();
    assert_eq!(added, [PathBuf::from("/new")]);
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(value["theme"], "dark");
    assert_eq!(
        value["projects"]["/old"]["allowedTools"],
        serde_json::json!([])
    );
    assert_eq!(value["projects"]["/new"]["hasTrustDialogAccepted"], true);
    assert!(
        trust::ensure("claude", &config, &folders)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn other_kinds_are_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("whatever");
    assert!(
        trust::ensure("opencode", &config, &[PathBuf::from("/x")])
            .unwrap()
            .is_empty()
    );
    assert!(!config.exists());
}
