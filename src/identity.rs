use sha2::{Digest, Sha256};

use crate::error::{ErrorCode, PodError, Result};
use crate::herdr::Herdr;
use crate::paths::session_from_env;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Caller {
    pub terminal_id: String,
    pub pane_id: Option<String>,
    pub pid: u32,
    pub session: String,
}

impl Caller {
    pub fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.terminal_id.as_bytes());
        digest.update(b"\n");
        digest.update(self.session.as_bytes());
        hex::encode(digest.finalize())
    }
}

/// The calling pane's identity (spec section 3.4). `None` outside herdr
/// (`HERDR_ENV` unset) and when `HERDR_PANE_ID` is unset, for example a hook
/// herdr runs outside any pane: herdr's `pane.current` without a caller pane
/// is the focused pane, which is not the caller, so it is never used here.
pub fn resolve(env: &dyn Fn(&str) -> Option<String>, herdr: &dyn Herdr) -> Result<Option<Caller>> {
    if env("HERDR_ENV").is_none() {
        return Ok(None);
    }
    let Some(pane_id) = env("HERDR_PANE_ID").filter(|pane| !pane.is_empty()) else {
        return Ok(None);
    };
    let pane = herdr.pane_get(&pane_id)?.ok_or_else(|| {
        PodError::new(
            ErrorCode::NotInHerdr,
            format!("herdr does not know pane {pane_id} from HERDR_PANE_ID"),
            &["Run the command from a live pane of this herdr session, or check HERDR_SOCKET_PATH."],
        )
    })?;
    Ok(Some(Caller {
        terminal_id: pane.terminal_id,
        pane_id: Some(pane.pane_id),
        pid: std::process::id(),
        session: session_from_env(env),
    }))
}
