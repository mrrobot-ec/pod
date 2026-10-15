pub mod env;
pub mod gates;
pub mod messages;
pub mod recovery;
pub mod runs;
pub mod tasks;
pub mod workers;

use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{Ctx, output};
use crate::error::{PodError, Result};

/// Prints a mutating command's receipt with the `mutation` block every
/// mutating receipt carries, as Orca's do: `requestId` is the
/// `--retry-request` ID (null without one) and `replayed` says whether the
/// stored receipt was returned instead of mutating again.
pub(crate) fn emit_mutation<T: Serialize>(ctx: &Ctx, receipt: &T, replayed: bool) -> Result<()> {
    let mut value = serde_json::to_value(receipt)
        .map_err(|error| PodError::store(format!("could not serialize receipt: {error}")))?;
    if let Value::Object(fields) = &mut value {
        fields.insert(
            "mutation".into(),
            json!({"requestId": ctx.retry_request, "replayed": replayed}),
        );
    }
    output::emit_ok(ctx.json, &value);
    Ok(())
}
