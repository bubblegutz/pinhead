//! Lua serialization API — JSON, YAML, TOML, CSV.
//!
//! Registered in the Lua VM as top-level global tables:
//!
//! ```lua
//! json.enc(value)            -> string
//! json.enc_pretty(value)     -> string
//! json.dec(string)           -> value
//! json.q(text, path)         -> value
//! yaml.enc(value)            -> string
//! yaml.dec(string)           -> value
//! yaml.q(text, path)         -> value
//! toml.enc(value)            -> string
//! toml.dec(string)           -> value
//! toml.q(text, path)         -> value
//! csv.enc(rows)              -> string
//! csv.dec(string)            -> rows (array of tables)
//! csv.q(text, filter)        -> rows (filtered by column=value)
//! ```

use serde_json::Value as Json;

// ── Lua ↔ serde_json::Value conversion ──────────────────────────────────────

fn lua_to_json(lua: &rlua::Lua, val: rlua::Value) -> Result<Json, String> {
    match val {
        rlua::Value::Nil => Ok(Json::Null),
        rlua::Value::Boolean(b) => Ok(Json::Bool(b)),
        rlua::Value::Integer(i) => Ok(Json::Number(serde_json::Number::from(i))),
        rlua::Value::Number(n) => {
            if !n.is_finite() {
                return Err("cannot encode non-finite number".into());
            }
            serde_json::Number::from_f64(n)
                .map(Json::Number)
                .ok_or_else(|| format!("cannot encode number {n}"))
        }
        rlua::Value::String(s) => Ok(Json::String(
            s.to_str().map_err(|e| e.to_string())?.to_string(),
        )),
        rlua::Value::Table(t) => {
            let len: i64 = t.raw_len().try_into().unwrap_or(0);
            if len > 0 {
                let mut arr = Vec::with_capacity(len as usize);
                for i in 1..=len {
                    let v: rlua::Value = t.raw_get(i).map_err(|e| e.to_string())?;
                    arr.push(lua_to_json(lua, v)?);
                }
                Ok(Json::Array(arr))
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.clone().pairs::<rlua::Value, rlua::Value>() {
                    let (k, v) = pair.map_err(|e| e.to_string())?;
                    let key = match k {
                        rlua::Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
                        rlua::Value::Integer(i) => i.to_string(),
                        rlua::Value::Number(n) => n.to_string(),
                        _ => return Err("object keys must be strings or numbers".into()),
                    };
                    map.insert(key, lua_to_json(lua, v)?);
                }
                Ok(Json::Object(map))
            }
        }
        _ => Err("cannot encode this Lua type".into()),
    }
}

fn json_to_lua<'lua>(lua: &'lua rlua::Lua, val: &Json) -> Result<rlua::Value<'lua>, String> {
    match val {
        Json::Null => Ok(rlua::Value::Nil),
        Json::Bool(b) => Ok(rlua::Value::Boolean(*b)),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(rlua::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(rlua::Value::Number(f))
            } else {
                Err(format!("cannot decode number {n}"))
            }
        }
        Json::String(s) => {
            let ls = lua.create_string(s).map_err(|e| e.to_string())?;
            Ok(rlua::Value::String(ls))
        }
        Json::Array(arr) => {
            let t = lua.create_table().map_err(|e| e.to_string())?;
            for (i, v) in arr.iter().enumerate() {
                t.set((i + 1) as i64, json_to_lua(lua, v)?)
                    .map_err(|e| e.to_string())?;
            }
            Ok(rlua::Value::Table(t))
        }
        Json::Object(map) => {
            let t = lua.create_table().map_err(|e| e.to_string())?;
            for (k, v) in map {
                t.set(k.as_str(), json_to_lua(lua, v)?)
                    .map_err(|e| e.to_string())?;
            }
            Ok(rlua::Value::Table(t))
        }
    }
}

// ── JSON ────────────────────────────────────────────────────────────────────

pub fn json_encode(lua: &rlua::Lua, val: rlua::Value) -> Result<String, String> {
    serde_json::to_string(&lua_to_json(lua, val)?).map_err(|e| e.to_string())
}

pub fn json_encode_pretty(lua: &rlua::Lua, val: rlua::Value) -> Result<String, String> {
    serde_json::to_string_pretty(&lua_to_json(lua, val)?).map_err(|e| e.to_string())
}

pub fn json_decode<'lua>(lua: &'lua rlua::Lua, text: String) -> Result<rlua::Value<'lua>, String> {
    let j: Json = serde_json::from_str(&text).map_err(|e| format!("JSON error: {e}"))?;
    json_to_lua(lua, &j)
}

// ── YAML ────────────────────────────────────────────────────────────────────

pub fn yaml_encode(lua: &rlua::Lua, val: rlua::Value) -> Result<String, String> {
    serde_yaml::to_string(&lua_to_json(lua, val)?).map_err(|e| format!("YAML error: {e}"))
}

pub fn yaml_decode<'lua>(lua: &'lua rlua::Lua, text: String) -> Result<rlua::Value<'lua>, String> {
    let j: Json = serde_yaml::from_str(&text).map_err(|e| format!("YAML error: {e}"))?;
    json_to_lua(lua, &j)
}

// ── TOML ────────────────────────────────────────────────────────────────────

pub fn toml_encode(lua: &rlua::Lua, val: rlua::Value) -> Result<String, String> {
    let j = lua_to_json(lua, val)?;
    // TOML only allows objects at the top level.
    let j_obj = match &j {
        Json::Object(_) => j,
        _ => {
            let mut m = serde_json::Map::new();
            m.insert("value".into(), j);
            Json::Object(m)
        }
    };
    toml::to_string(&j_obj).map_err(|e| format!("TOML error: {e}"))
}

pub fn toml_decode<'lua>(lua: &'lua rlua::Lua, text: String) -> Result<rlua::Value<'lua>, String> {
    let j: Json = toml::from_str(&text).map_err(|e| format!("TOML error: {e}"))?;
    json_to_lua(lua, &j)
}

// ── CSV ─────────────────────────────────────────────────────────────────────
//
// Encode: input is array of tables (each row = one table with string keys).
// Decode: output is array of tables, keys from header row.

pub fn csv_encode<'lua>(lua: &'lua rlua::Lua, val: rlua::Value) -> Result<String, String> {
    let j = lua_to_json(lua, val)?;
    let rows = match &j {
        Json::Array(arr) => arr,
        _ => return Err("CSV encode expects an array of objects".into()),
    };

    // Collect column headers from all rows.
    let mut columns: Vec<&str> = Vec::new();
    for row in rows {
        if let Json::Object(map) = row {
            for key in map.keys() {
                if !columns.contains(&key.as_str()) {
                    columns.push(key.as_str());
                }
            }
        }
    }
    columns.sort();

    let mut wtr = csv::Writer::from_writer(Vec::new());
    // Header row.
    wtr.write_record(&columns).map_err(|e| format!("CSV error: {e}"))?;

    for row in rows {
        match row {
            Json::Object(map) => {
                let rec: Vec<String> = columns
                    .iter()
                    .map(|col| {
                        map.get(*col)
                            .map(|v| match v {
                                Json::Null => String::new(),
                                Json::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                wtr.write_record(&rec)
                    .map_err(|e| format!("CSV error: {e}"))?;
            }
            _ => return Err("CSV encode expects an array of objects".into()),
        }
    }

    wtr.flush().map_err(|e| format!("CSV error: {e}"))?;
    let data = wtr.into_inner().map_err(|e| format!("CSV error: {e}"))?;
    Ok(String::from_utf8_lossy(&data).to_string())
}

pub fn csv_decode<'lua>(lua: &'lua rlua::Lua, text: String) -> Result<rlua::Value<'lua>, String> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(text.as_bytes());

    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| format!("CSV error: {e}"))?
        .iter()
        .map(|h| h.to_string())
        .collect();

    let result_t = lua.create_table().map_err(|e| e.to_string())?;
    let mut idx: i64 = 1;

    for row in rdr.records() {
        let rec = row.map_err(|e| format!("CSV error: {e}"))?;
        let row_t = lua.create_table().map_err(|e| e.to_string())?;
        for (i, field) in rec.iter().enumerate() {
            let key = headers.get(i).map(|h| h.as_str()).unwrap_or("");
            // Try numeric parse.
            if let Ok(n) = field.parse::<i64>() {
                row_t.set(key, n).map_err(|e| e.to_string())?;
            } else if let Ok(n) = field.parse::<f64>() {
                row_t.set(key, n).map_err(|e| e.to_string())?;
            } else if field.is_empty() {
                row_t.set(key, rlua::Value::Nil).map_err(|e| e.to_string())?;
            } else {
                row_t.set(key, field).map_err(|e| e.to_string())?;
            }
        }
        result_t.set(idx, row_t).map_err(|e| e.to_string())?;
        idx += 1;
    }

    Ok(rlua::Value::Table(result_t))
}

// ── Path query ──────────────────────────────────────────────────────────────
//
// Supports dot-separated paths: "key", "key.sub", "arr.0.field", "0".
// Array indices are numeric segments.

fn json_query_value<'a>(val: &'a Json, path: &str) -> Result<&'a Json, String> {
    if path.is_empty() {
        return Ok(val);
    }
    let mut current = val;
    for segment in path.split('.') {
        if segment.is_empty() {
            continue;
        }
        match current {
            Json::Object(map) => {
                current = map
                    .get(segment)
                    .ok_or_else(|| format!("key '{segment}' not found"))?;
            }
            Json::Array(arr) => {
                let idx: usize = segment
                    .parse()
                    .map_err(|_| format!("array index '{segment}' is not a valid integer"))?;
                current = arr
                    .get(idx)
                    .ok_or_else(|| format!("index {idx} out of bounds (len {})", arr.len()))?;
            }
            other => {
                return Err(format!(
                    "cannot index into {} with '{segment}'",
                    type_name(other)
                ));
            }
        }
    }
    Ok(current)
}

fn type_name(val: &Json) -> &'static str {
    match val {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

/// Decode a format, run a path query, return the result as a Lua value.
fn decode_and_query<'lua>(
    lua: &'lua rlua::Lua,
    text: String,
    path: String,
    parser: fn(&str) -> Result<Json, String>,
    format: &str,
) -> Result<rlua::Value<'lua>, String> {
    let j = parser(&text).map_err(|e| format!("{format} error: {e}"))?;
    let queried = json_query_value(&j, &path)?;
    json_to_lua(lua, queried)
}

// ── Parser helpers ──────────────────────────────────────────────────────────

fn parse_json(text: &str) -> Result<Json, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn parse_yaml(text: &str) -> Result<Json, String> {
    serde_yaml::from_str(text).map_err(|e| e.to_string())
}

fn parse_toml(text: &str) -> Result<Json, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

// ── Public query functions ──────────────────────────────────────────────────

pub fn json_query<'lua>(
    lua: &'lua rlua::Lua,
    text: String,
    path: String,
) -> Result<rlua::Value<'lua>, String> {
    decode_and_query(lua, text, path, parse_json, "JSON")
}

pub fn yaml_query<'lua>(
    lua: &'lua rlua::Lua,
    text: String,
    path: String,
) -> Result<rlua::Value<'lua>, String> {
    decode_and_query(lua, text, path, parse_yaml, "YAML")
}

pub fn toml_query<'lua>(
    lua: &'lua rlua::Lua,
    text: String,
    path: String,
) -> Result<rlua::Value<'lua>, String> {
    decode_and_query(lua, text, path, parse_toml, "TOML")
}

/// CSV query: filter rows by `column=value`.
/// Returns an array of matching rows (as Lua tables).
pub fn csv_query<'lua>(
    lua: &'lua rlua::Lua,
    text: String,
    filter: String,
) -> Result<rlua::Value<'lua>, String> {
    let (col, expected) = filter
        .split_once('=')
        .ok_or_else(|| "CSV query syntax: column=value".to_string())?;
    let col = col.trim();
    let expected = expected.trim();

    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(text.as_bytes());

    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| format!("CSV error: {e}"))?
        .iter()
        .map(|h| h.to_string())
        .collect();

    // Find column index.
    let col_idx = headers
        .iter()
        .position(|h| h == col)
        .ok_or_else(|| format!("column '{col}' not found in CSV"))?;

    let result_t = lua.create_table().map_err(|e| e.to_string())?;
    let mut idx: i64 = 1;

    for row in rdr.records() {
        let rec = row.map_err(|e| format!("CSV error: {e}"))?;
        let val = rec.get(col_idx).unwrap_or("");
        if val == expected {
            let row_t = lua.create_table().map_err(|e| e.to_string())?;
            for (i, field) in rec.iter().enumerate() {
                let key = headers.get(i).map(|h| h.as_str()).unwrap_or("");
                if let Ok(n) = field.parse::<i64>() {
                    row_t.set(key, n).map_err(|e| e.to_string())?;
                } else if let Ok(n) = field.parse::<f64>() {
                    row_t.set(key, n).map_err(|e| e.to_string())?;
                } else if field.is_empty() {
                    row_t.set(key, rlua::Value::Nil).map_err(|e| e.to_string())?;
                } else {
                    row_t.set(key, field).map_err(|e| e.to_string())?;
                }
            }
            result_t.set(idx, row_t).map_err(|e| e.to_string())?;
            idx += 1;
        }
    }

    Ok(rlua::Value::Table(result_t))
}
