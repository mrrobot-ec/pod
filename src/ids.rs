use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{PodError, Result};

pub fn new_id(prefix: &str) -> String {
    let compact = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}{}", &compact[..12])
}

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(new_id($prefix))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = PodError;

            fn from_str(value: &str) -> Result<Self> {
                let suffix = value.strip_prefix($prefix).ok_or_else(|| {
                    PodError::usage(format!("expected an ID beginning with {}", $prefix))
                })?;
                if suffix.len() == 12
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    Ok(Self(value.to_owned()))
                } else {
                    Err(PodError::usage(format!("invalid {} ID", $prefix)))
                }
            }
        }
    };
}

id_type!(RunId, "run_");
id_type!(TaskId, "task_");
id_type!(DispatchId, "ctx_");
id_type!(MessageId, "msg_");
id_type!(DeliveryId, "dlv_");
id_type!(GateId, "gate_");
id_type!(WakeId, "wake_");
id_type!(RequestId, "req_");
