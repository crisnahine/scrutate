//! A JSON value checked against the subset of JSON Schema the MCP tool definitions use.

/// A value against the subset of JSON Schema the tool definitions use.
pub fn conforms(schema: &serde_json::Value, v: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        return if options.iter().any(|o| conforms(o, v).is_ok()) { Ok(()) } else { Err(format!("{v} fits none of {schema}")) };
    }
    let kind = match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    let allowed: Vec<&str> = match &schema["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![kind],
    };
    if !allowed.contains(&kind) && !(kind == "integer" && allowed.contains(&"number")) {
        return Err(format!("{v} is a {kind}, not one of {allowed:?}"));
    }
    for name in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if v.get(name).is_none() {
            return Err(format!("{v} has no {name}"));
        }
    }
    for (name, sub) in schema["properties"].as_object().into_iter().flatten() {
        if let Some(field) = v.get(name) {
            conforms(sub, field).map_err(|e| format!("{name}: {e}"))?;
        }
    }
    if let (Some(items), Some(list)) = (schema.get("items"), v.as_array()) {
        for item in list {
            conforms(items, item)?;
        }
    }
    Ok(())
}
