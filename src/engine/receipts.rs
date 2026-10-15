use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};

use crate::engine::Engine;
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::RequestId;
use crate::store::Tx;
use crate::store::rows::receipts::{self, ReceiptRow};

pub fn with_receipt<T: Serialize + DeserializeOwned>(
    engine: &Engine,
    request_id: Option<&RequestId>,
    method: &str,
    payload_hash: &str,
    f: impl FnOnce(&Tx<'_>) -> Result<T>,
) -> Result<T> {
    with_receipt_replay(engine, request_id, method, payload_hash, f).map(|(value, _)| value)
}

/// `with_receipt`, also reporting whether the value is a replayed receipt
/// (the receipt's `mutation.replayed`).
pub fn with_receipt_replay<T: Serialize + DeserializeOwned>(
    engine: &Engine,
    request_id: Option<&RequestId>,
    method: &str,
    payload_hash: &str,
    f: impl FnOnce(&Tx<'_>) -> Result<T>,
) -> Result<(T, bool)> {
    let Some(request_id) = request_id else {
        return engine.store.tx(f).map(|value| (value, false));
    };
    let caller = engine.caller.as_ref().ok_or_else(|| {
        PodError::new(
            ErrorCode::NotInHerdr,
            "mutation receipts require a caller inside herdr",
            &["Run the command from a pane inside herdr."],
        )
    })?;
    let fingerprint = caller.fingerprint();
    if let Some(row) = engine
        .store
        .read(|tx| receipts::get(tx, &fingerprint, request_id))?
    {
        validate_replay(&row, method, payload_hash)?;
        if let Some(receipt) = row.receipt {
            return decode(receipt).map(|value| (value, true));
        }
    } else {
        // Precommitted, so a mutation that fails leaves the receipt `pending`
        // for request-show and a later replay.
        let now = engine.clock.now();
        engine.store.tx(|tx| {
            if receipts::get(tx, &fingerprint, request_id)?.is_none() {
                receipts::insert(
                    tx,
                    &row(&fingerprint, request_id, method, payload_hash, None, now),
                )?;
            }
            Ok(())
        })?;
    }
    engine.store.tx(|tx| {
        // A concurrent replay of the same request may have committed between
        // the read above and this IMMEDIATE transaction; never mutate twice.
        if let Some(row) = receipts::get(tx, &fingerprint, request_id)? {
            validate_replay(&row, method, payload_hash)?;
            if let Some(receipt) = row.receipt {
                return decode(receipt).map(|value| (value, true));
            }
        }
        let value = f(tx)?;
        let receipt = serde_json::to_value(&value)
            .map_err(|error| PodError::store(format!("could not serialize receipt: {error}")))?;
        let now = engine.clock.now();
        if receipts::get(tx, &fingerprint, request_id)?.is_some() {
            receipts::update_state(tx, &fingerprint, request_id, "completed")?;
            receipts::update_receipt(tx, &fingerprint, request_id, Some(&receipt))?;
            receipts::update_updated_at(tx, &fingerprint, request_id, now)?;
        } else {
            // `reset --scope all` deletes receipts inside the mutation.
            receipts::insert(
                tx,
                &row(
                    &fingerprint,
                    request_id,
                    method,
                    payload_hash,
                    Some(receipt),
                    now,
                ),
            )?;
        }
        receipts::prune(tx, now)?;
        Ok((value, false))
    })
}

fn row(
    fingerprint: &str,
    request_id: &RequestId,
    method: &str,
    payload_hash: &str,
    receipt: Option<serde_json::Value>,
    now: DateTime<Utc>,
) -> ReceiptRow {
    ReceiptRow {
        caller_fingerprint: fingerprint.to_owned(),
        request_id: request_id.clone(),
        method: method.to_owned(),
        payload_hash: payload_hash.to_owned(),
        state: if receipt.is_some() {
            "completed"
        } else {
            "pending"
        }
        .to_owned(),
        receipt,
        created_at: now,
        updated_at: now,
    }
}

fn decode<T: DeserializeOwned>(receipt: serde_json::Value) -> Result<T> {
    serde_json::from_value(receipt)
        .map_err(|error| PodError::store(format!("invalid stored receipt: {error}")))
}

#[derive(Debug)]
pub struct ReceiptState {
    pub request_id: String,
    pub method: String,
    pub payload_hash: String,
    pub state: String,
    pub receipt: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub fn show(engine: &Engine, request_id: &RequestId) -> Result<Option<ReceiptState>> {
    let caller = engine.caller.as_ref().ok_or_else(|| {
        PodError::new(
            ErrorCode::NotInHerdr,
            "request-show requires the caller that issued the mutation",
            &["Run request-show from the herdr pane that issued the request."],
        )
    })?;
    engine.store.read(|tx| {
        receipts::get(tx, &caller.fingerprint(), request_id).map(|row| {
            row.map(|row| ReceiptState {
                request_id: request_id.to_string(),
                method: row.method,
                payload_hash: row.payload_hash,
                state: row.state,
                receipt: row.receipt,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        })
    })
}

fn validate_replay(row: &ReceiptRow, method: &str, payload_hash: &str) -> Result<()> {
    if row.method != method || row.payload_hash != payload_hash {
        return Err(PodError::usage(format!(
            "--retry-request belongs to method {}, not {method}, or its payload differs",
            row.method
        )));
    }
    Ok(())
}

/// The `payload_hash` a receipt is keyed by: SHA-256 of the canonical JSON.
pub fn payload_hash(value: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(value.to_string().as_bytes());
    hex::encode(digest.finalize())
}
