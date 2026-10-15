use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::error::PodError;

pub fn emit_ok<T: Serialize>(json_output: bool, value: &T) {
    let value = serde_json::to_value(value)
        .unwrap_or_else(|error| json!({"serializationError": error.to_string()}));
    let mut envelope = Map::new();
    envelope.insert("ok".into(), Value::Bool(true));
    match value {
        Value::Object(fields) => envelope.extend(fields),
        other => {
            envelope.insert("value".into(), other);
        }
    }
    if json_output {
        println!("{}", Value::Object(envelope));
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(envelope))
                .expect("serialize success output")
        );
    }
}

pub fn emit_err(json_output: bool, error: &PodError) -> i32 {
    if json_output {
        let mut data = error.data.clone();
        data.insert("nextSteps".into(), json!(error.next_steps));
        eprintln!(
            "{}",
            json!({
                "error": {
                    "code": error.code,
                    "message": error.message,
                    "data": data,
                }
            })
        );
    } else {
        eprintln!("error: {}", error.message);
        for step in &error.next_steps {
            eprintln!("next: {step}");
        }
    }
    error.exit_code()
}
