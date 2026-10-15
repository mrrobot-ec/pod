use crate::error::{PodError, Result};

const COORDINATOR: &str = include_str!("../docs/guides/coordinator.md");
const WORKER: &str = include_str!("../docs/guides/worker.md");

/// Reference pages, named by file name without extension.
const REFERENCES: [(&str, &str); 9] = [
    ("commands", include_str!("../docs/reference/commands.md")),
    (
        "commands-messages",
        include_str!("../docs/reference/commands-messages.md"),
    ),
    (
        "commands-workers",
        include_str!("../docs/reference/commands-workers.md"),
    ),
    ("config", include_str!("../docs/reference/config.md")),
    ("errors", include_str!("../docs/reference/errors.md")),
    (
        "state-machines",
        include_str!("../docs/reference/state-machines.md"),
    ),
    (
        "transcript-formats",
        include_str!("../docs/reference/transcript-formats.md"),
    ),
    (
        "contract-capture",
        include_str!("../docs/reference/contract-capture.md"),
    ),
    (
        "herdr-adapter",
        include_str!("../docs/reference/herdr-adapter.md"),
    ),
];

/// Returns the guide for `role`, one named reference page, or (with `full`)
/// the guide followed by every reference page. `reference` wins over `full`.
pub fn get(role: &str, reference: Option<&str>, full: bool) -> Result<String> {
    let guide = match role {
        "coordinator" => COORDINATOR,
        "worker" => WORKER,
        other => {
            return Err(PodError::usage(format!(
                "unknown role `{other}`; expected `coordinator` or `worker`"
            )));
        }
    };
    if let Some(name) = reference {
        return REFERENCES
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, text)| (*text).to_owned())
            .ok_or_else(|| {
                let names: Vec<&str> = REFERENCES.iter().map(|(n, _)| *n).collect();
                PodError::usage(format!(
                    "unknown reference `{name}`; expected one of: {}",
                    names.join(", ")
                ))
            });
    }
    let mut out = guide.to_owned();
    if full {
        for (_, text) in REFERENCES {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(text);
        }
    }
    Ok(out)
}
