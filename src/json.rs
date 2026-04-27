//! Lua <-> JSON serialization for handler scripts.
//!
//! Provides `json.encode()` and `json.decode()` in the Lua VM so that
//! handler scripts can serialize/deserialize JSON payloads (e.g. from
//! REST API responses).

/// Convert an mlua `Value` to a `serde_json::Value` for encoding.
fn lua_to_json(lua: &mlua::Lua, val: mlua::Value) -> Result<serde_json::Value, String> {
    match val {
        mlua::Value::Nil => Ok(serde_json::Value::Null),
        mlua::Value::Boolean(b) => Ok(serde_json::Value::Bool(b)),
        mlua::Value::Integer(i) => Ok(serde_json::Value::Number(
            serde_json::Number::from(i),
        )),
        mlua::Value::Number(n) => {
            // mlua::Number is f64. Only serialize if it's finite.
            if !n.is_finite() {
                return Err("cannot encode non-finite number".into());
            }
            serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| format!("cannot encode number {n}"))
        }
        mlua::Value::String(s) => Ok(serde_json::Value::String(
            s.to_str().map_err(|e| e.to_string())?.to_string(),
        )),
        mlua::Value::Table(t) => {
            // Check if it's an array (consecutive integer keys 1..n).
            let len: i64 = t
                .raw_len()
                .try_into()
                .unwrap_or(0);
            if len > 0 {
                let mut arr = Vec::with_capacity(len as usize);
                for i in 1..=len {
                    let v: mlua::Value = t
                        .raw_get(i)
                        .map_err(|e| e.to_string())?;
                    arr.push(lua_to_json(lua, v)?);
                }
                Ok(serde_json::Value::Array(arr))
            } else {
                // Object: iterate all key-value pairs with string keys.
                let mut map = serde_json::Map::new();
                for pair in t.clone().pairs::<mlua::Value, mlua::Value>() {
                    let (k, v) = pair.map_err(|e| e.to_string())?;
                    let key = match k {
                        mlua::Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
                        mlua::Value::Integer(i) => i.to_string(),
                        mlua::Value::Number(n) => n.to_string(),
                        _ => return Err("JSON object keys must be strings or numbers".into()),
                    };
                    map.insert(key, lua_to_json(lua, v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
        }
        _ => Err("cannot encode this Lua type to JSON".into()),
    }
}

/// Convert a `serde_json::Value` to an mlua `Value` for decoding.
fn json_to_lua<'lua>(
    lua: &'lua mlua::Lua,
    val: &serde_json::Value,
) -> Result<mlua::Value<'lua>, String> {
    match val {
        serde_json::Value::Null => Ok(mlua::Value::Nil),
        serde_json::Value::Bool(b) => Ok(mlua::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(mlua::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(mlua::Value::Number(f))
            } else {
                Err(format!("cannot decode JSON number {n}"))
            }
        }
        serde_json::Value::String(s) => {
            let ls = lua
                .create_string(s)
                .map_err(|e| e.to_string())?;
            Ok(mlua::Value::String(ls))
        }
        serde_json::Value::Array(arr) => {
            let t = lua.create_table().map_err(|e| e.to_string())?;
            for (i, v) in arr.iter().enumerate() {
                let idx = (i + 1) as i64;
                let lv = json_to_lua(lua, v)?;
                t.set(idx, lv).map_err(|e| e.to_string())?;
            }
            Ok(mlua::Value::Table(t))
        }
        serde_json::Value::Object(map) => {
            let t = lua.create_table().map_err(|e| e.to_string())?;
            for (k, v) in map {
                let lv = json_to_lua(lua, v)?;
                t.set(k.as_str(), lv).map_err(|e| e.to_string())?;
            }
            Ok(mlua::Value::Table(t))
        }
    }
}

/// Encode a Lua value to a JSON string.
pub fn encode(lua: &mlua::Lua, val: mlua::Value) -> Result<String, String> {
    let json = lua_to_json(lua, val)?;
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

/// Encode a Lua value to a pretty-printed JSON string.
pub fn encode_pretty(lua: &mlua::Lua, val: mlua::Value) -> Result<String, String> {
    let json = lua_to_json(lua, val)?;
    serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
}

/// Decode a JSON string into a Lua value.
pub fn decode<'lua>(lua: &'lua mlua::Lua, text: String) -> Result<mlua::Value<'lua>, String> {
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("JSON decode error: {e}"))?;
    json_to_lua(lua, &json)
}
