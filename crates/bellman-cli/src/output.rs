//! Human and JSON output helpers (stable envelope for AI consumers).

use serde::Serialize;
use serde_json::{json, Value};

use crate::commands::{CliError, CommandPayload};

/// Print a successful command result.
pub fn emit_success(as_json: bool, payload: &CommandPayload) {
    if as_json {
        let mut body = payload.to_json();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("ok".into(), json!(true));
        }
        println!("{}", pretty(&body));
    } else {
        println!("{}", payload.to_human());
    }
}

/// Print an error (JSON envelope or plain text on stderr).
pub fn emit_error(as_json: bool, command: &'static str, err: &CliError) {
    if as_json {
        emit_parse_error(command, err.code, &err.message);
    } else {
        eprintln!("error: {}", err.message);
    }
}

/// Emit the stable JSON error envelope on stdout (AI-primary parse failures).
///
/// Schema: `{ "ok": false, "command": …, "error": { "code", "message" } }`.
pub fn emit_parse_error(command: &str, code: &str, message: &str) {
    let body = json!({
        "ok": false,
        "command": command,
        "error": {
            "code": code,
            "message": message,
        }
    });
    println!("{}", pretty(&body));
}

fn pretty(v: &Value) -> String {
    // Compact single-line JSON is easier for agents to parse from mixed streams;
    // pretty-print only when BELLMAN_JSON_PRETTY=1.
    if std::env::var_os("BELLMAN_JSON_PRETTY").is_some() {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    } else {
        serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
    }
}

/// Serialize a timer (and related structs) with the public wire shape.
pub fn timer_json(timer: &bellman_core::Timer) -> Value {
    // Timer already derives Serialize with stable field names.
    serde_json::to_value(timer).unwrap_or(Value::Null)
}

/// Helper for ad-hoc serializable payloads.
#[allow(dead_code)]
pub fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}
