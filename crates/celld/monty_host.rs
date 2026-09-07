//! Direct Monty access to celld storage. No JavaScript or V8 value conversion.
//! The native worker supplies the scope after acquiring the actor's input
//! gate; Python never supplies or changes it. Drop rolls back abandoned turns.
use crate::storage;
use serde_json::{json, Value};

pub(crate) struct Host {
    pub scope: Option<String>,
    transactions: usize,
}
impl Host {
    pub fn new(scope: Option<String>) -> Self {
        Self {
            scope,
            transactions: 0,
        }
    }
    pub fn in_transaction(&self) -> bool {
        self.transactions > 0
    }
    pub fn handles(op: &str) -> bool {
        (op.starts_with("storage.") && op != "storage.sync") || matches!(op, "now" | "uuid" | "log")
    }
    /// Returns the value and any committed alarm requiring the native wake gate.
    pub fn call(&mut self, op: &str, args: &Value) -> Result<(Value, Option<i64>), String> {
        let args = args.as_array().ok_or("invalid host arguments")?;
        let text = |i: usize| {
            args.get(i)
                .and_then(Value::as_str)
                .ok_or("expected string argument")
        };
        let mut alarm = None;
        let value = match op {
            "now" => json!(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64),
            "uuid" => json!(new_uuid()?),
            "log" => {
                tracing::info!(target: "monty", "{}", text(0)?);
                Value::Null
            }
            _ => {
                let scope = self
                    .scope
                    .as_deref()
                    .ok_or("storage requires a durable object")?;
                match op {
                    "storage.get" => {
                        let value =
                            storage::get_stored(scope, text(0)?).map_err(|e| e.to_string())?;
                        json!({"found":value.is_some(),"value":value.map(decode).transpose()?})
                    }
                    "storage.put" => {
                        storage::put_json(scope, text(0)?, args.get(1).ok_or("missing value")?)
                            .map_err(|e| e.to_string())?;
                        Value::Null
                    }
                    "storage.delete" => {
                        json!(storage::delete(scope, text(0)?).map_err(|e| e.to_string())?)
                    }
                    "storage.delete_all" => {
                        storage::delete_all_with_alarm(scope, false).map_err(|e| e.to_string())?;
                        Value::Null
                    }
                    "storage.list" => {
                        let limit = args
                            .get(1)
                            .and_then(Value::as_u64)
                            .filter(|n| *n <= 1000)
                            .ok_or("list limit must be 0-1000")?;
                        let reverse = args
                            .get(2)
                            .and_then(Value::as_bool)
                            .ok_or("reverse must be bool")?;
                        Value::Object(
                            storage::list_stored_with_options(
                                scope,
                                None,
                                None,
                                None,
                                Some(text(0)?),
                                Some(limit as usize),
                                reverse,
                            )
                            .map_err(|e| e.to_string())?
                            .into_iter()
                            .map(|(k, v)| Ok((k, decode(v)?)))
                            .collect::<Result<_, String>>()?,
                        )
                    }
                    "storage.sql" => {
                        let bindings = args
                            .get(1)
                            .and_then(Value::as_array)
                            .ok_or("SQL bindings must be an array")?;
                        let (columns, rows, _) = storage::sql_exec(scope, text(0)?, bindings)?;
                        json!(rows
                            .into_iter()
                            .map(|row| columns
                                .iter()
                                .cloned()
                                .zip(row)
                                .collect::<serde_json::Map<_, _>>())
                            .collect::<Vec<_>>())
                    }
                    "storage.get_alarm" => json!(storage::get_alarm(scope)),
                    "storage.set_alarm" => {
                        let when = args.first().ok_or("missing alarm time")?;
                        let at = if let Some(seconds) = when.as_f64() {
                            let timestamp =
                                chrono::Utc::now().timestamp_millis() as f64 + seconds * 1000.0;
                            if !timestamp.is_finite()
                                || timestamp < 0.0
                                || timestamp >= i64::MAX as f64
                            {
                                return Err("invalid alarm duration".into());
                            }
                            timestamp as i64
                        } else {
                            chrono::DateTime::parse_from_rfc3339(
                                when.as_str().ok_or("invalid alarm time")?,
                            )
                            .map_err(|_| "alarm datetime must include a timezone")?
                            .timestamp_millis()
                        };
                        alarm = storage::set_alarm(scope, at).map_err(|e| e.to_string())?;
                        Value::Null
                    }
                    "storage.delete_alarm" => {
                        storage::delete_alarm(scope).map_err(|e| e.to_string())?;
                        Value::Null
                    }
                    "storage.transaction_begin" => {
                        if self.transactions >= 16 {
                            return Err("transaction nesting limit exceeded".into());
                        }
                        storage::transaction_control(
                            scope,
                            "start",
                            self.transactions > 0,
                            &format!("cells_tx_{}", self.transactions),
                        )?;
                        self.transactions += 1;
                        Value::Null
                    }
                    "storage.transaction_commit" | "storage.transaction_rollback" => {
                        let depth = self
                            .transactions
                            .checked_sub(1)
                            .ok_or("no open transaction")?;
                        alarm = storage::transaction_control(
                            scope,
                            if op.ends_with("commit") {
                                "commit"
                            } else {
                                "rollback"
                            },
                            depth > 0,
                            &format!("cells_tx_{depth}"),
                        )?;
                        self.transactions = depth;
                        Value::Null
                    }
                    _ => return Err("unknown native capability".into()),
                }
            }
        };
        let reply = if matches!(op, "now" | "storage.get_alarm") && !value.is_null() {
            celld_monty::value::timestamp_reply(value.as_i64().ok_or("invalid timestamp")?)?
        } else {
            json!({"result":value})
        };
        Ok((reply, alarm))
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Some(scope) = &self.scope {
            while self.transactions > 0 {
                self.transactions -= 1;
                let _ = storage::transaction_control(
                    scope,
                    "rollback",
                    self.transactions > 0,
                    &format!("cells_tx_{}", self.transactions),
                );
            }
        }
    }
}
fn decode(value: storage::StoredValue) -> Result<Value, String> {
    match value {
        storage::StoredValue::LegacyJson(json) => {
            serde_json::from_str(&json).map_err(|e| e.to_string())
        }
        storage::StoredValue::V8(_) => Err(
            "Monty storage expects JSON; this key contains a JavaScript structured clone".into(),
        ),
    }
}

fn new_uuid() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
