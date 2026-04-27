//! Lua HTTP client API — registered as the `req` global table.
//!
//! ```lua
//! req.get(url)                    -> body (string)
//! req.get(url, opts)              -> response table
//! req.post(url, opts)             -> response table
//! req.put(url, opts)              -> response table
//! req.delete(url, opts)           -> response table
//! req.patch(url, opts)            -> response table
//! req.head(url, opts)             -> response table
//! req.options(url, opts)          -> response table
//! ```
//!
//! `opts` supports:
//! - `headers` — table of string → string
//! - `query`   — table of string → string (query parameters)
//! - `body`    — raw string body
//! - `json`    — any Lua value (serialized as JSON, sets Content-Type)
//! - `form`    — table of string → string (form-encoded, sets Content-Type)
//! - `decode`  — string: `"json"`, `"yaml"`, `"toml"`, or `"csv"` (auto-decodes body)
//!
//! Response table: `{status = number, body = string|value, headers = {name = value, ...}}`
//! When `decode` is set, `body` contains the decoded Lua value instead of a raw string.

use std::time::Duration;

use ureq::http;

/// Execute an HTTP request and return a Lua response value.
///
/// When `opts` is `None` and method is GET, returns just the body string.
/// Otherwise returns a table with `status`, `body`, and `headers` fields.
pub fn do_request<'lua>(
    lua: &'lua mlua::Lua,
    method: &str,
    url: String,
    opts: Option<mlua::Table<'lua>>,
) -> Result<mlua::Value<'lua>, String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build(),
    );

    // ── Parse options ──────────────────────────────────────────────────────
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut query: Option<Vec<(String, String)>> = None;
    let mut body_type: BodyType = BodyType::None;
    let mut decode_format: Option<String> = None;

    if let Some(ref table) = opts {
        // Headers
        if let Ok(hdrs) = table.get::<_, mlua::Table>("headers") {
            for pair in hdrs.pairs::<mlua::String, mlua::String>() {
                let (k, v) = pair.map_err(|e| format!("header error: {e}"))?;
                headers.push((k.to_str().unwrap().to_string(), v.to_str().unwrap().to_string()));
            }
        }

        // Query params
        if let Ok(q) = table.get::<_, mlua::Table>("query") {
            let mut params = Vec::new();
            for pair in q.pairs::<mlua::Value, mlua::Value>() {
                let (k, v) = pair.map_err(|e| format!("query error: {e}"))?;
                params.push((
                    value_to_string(&k).unwrap_or_default(),
                    value_to_string(&v).unwrap_or_default(),
                ));
            }
            query = Some(params);
        }

        // Body: json > form > string (mutually exclusive)
        if table.get::<_, mlua::Value>("json").is_ok() {
            let json_val: mlua::Value = table.get("json").map_err(|e| format!("{e}"))?;
            let body_str =
                serde_json::to_string(&crate::serialize::lua_to_json(lua, json_val)?)
                    .map_err(|e| format!("JSON encode error: {e}"))?;
            body_type = BodyType::Json(body_str);
        } else if let Ok(form_t) = table.get::<_, mlua::Table>("form") {
            let mut pairs = Vec::new();
            for pair in form_t.pairs::<mlua::Value, mlua::Value>() {
                let (k, v) = pair.map_err(|e| format!("form error: {e}"))?;
                pairs.push((
                    value_to_string(&k).unwrap_or_default(),
                    value_to_string(&v).unwrap_or_default(),
                ));
            }
            body_type = BodyType::Form(pairs);
        } else if let Ok(body_str) = table.get::<_, String>("body") {
            body_type = BodyType::String(body_str);
        }

        // Decode option
        decode_format = table.get::<_, String>("decode").ok();
    }

    // ── Build URI with query params ────────────────────────────────────────
    let uri_str = if let Some(ref qp) = query {
        let mut uri = url;
        let mut sep = if uri.contains('?') { '&' } else { '?' };
        for (k, v) in qp {
            uri.push(sep);
            uri.push_str(&url_encode(k));
            uri.push('=');
            uri.push_str(&url_encode(v));
            sep = '&';
        }
        uri
    } else {
        url
    };

    let uri: http::Uri = uri_str.parse().map_err(|e: http::uri::InvalidUri| format!("bad URI: {e}"))?;

    // ── Build request and send ─────────────────────────────────────────────
    let mut req_builder = http::Request::builder()
        .method(method)
        .uri(uri);

    for (k, v) in &headers {
        req_builder = req_builder.header(k.as_str(), v.as_str());
    }

    let response = match body_type {
        BodyType::Json(body) => {
            let req = req_builder
                .header("Content-Type", "application/json")
                .body(body)
                .map_err(|e| format!("build request: {e}"))?;
            agent.run(req).map_err(map_ureq_error)?
        }
        BodyType::Form(pairs) => {
            let encoded = form_encode(&pairs);
            let req = req_builder
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(encoded)
                .map_err(|e| format!("build request: {e}"))?;
            agent.run(req).map_err(map_ureq_error)?
        }
        BodyType::String(body) => {
            let req = req_builder
                .body(body)
                .map_err(|e| format!("build request: {e}"))?;
            agent.run(req).map_err(map_ureq_error)?
        }
        BodyType::None => {
            let req = req_builder
                .body(())
                .map_err(|e| format!("build request: {e}"))?;
            agent.run(req).map_err(map_ureq_error)?
        }
    };

    // ── Extract response data ──────────────────────────────────────────────
    let status: i64 = response.status().as_u16().into();
    let ok = response.status().is_success();
    let resp_headers = response.headers().clone();
    let resp_body = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read body: {e}"))?;

    // Simple GET without opts — return body string directly
    if method == "GET" && opts.is_none() {
        let s = lua.create_string(&resp_body).map_err(|e| format!("{e}"))?;
        return Ok(mlua::Value::String(s));
    }

    // Build response table
    let result = lua.create_table().map_err(|e| format!("{e}"))?;
    result.set("status", status).map_err(|e| format!("{e}"))?;
    result.set("ok", ok).map_err(|e| format!("{e}"))?;

    let body_lua = if let Some(ref fmt) = decode_format {
        decode_response_body(lua, fmt, &resp_body)?
    } else {
        let s = lua.create_string(&resp_body).map_err(|e| format!("{e}"))?;
        mlua::Value::String(s)
    };
    result.set("body", body_lua).map_err(|e| format!("{e}"))?;

    let hdrs_t = lua.create_table().map_err(|e| format!("{e}"))?;
    for (name, value) in resp_headers.iter() {
        let name_str = name.as_str().to_string();
        if let Ok(val_str) = value.to_str() {
            hdrs_t
                .set(name_str, val_str.to_string())
                .map_err(|e| format!("{e}"))?;
        }
    }
    result.set("headers", hdrs_t).map_err(|e| format!("{e}"))?;

    Ok(mlua::Value::Table(result))
}

enum BodyType {
    None,
    String(String),
    Json(String),
    Form(Vec<(String, String)>),
}

fn decode_response_body<'lua>(
    lua: &'lua mlua::Lua,
    format: &str,
    body: &str,
) -> Result<mlua::Value<'lua>, String> {
    match format {
        "json" => crate::serialize::json_decode(lua, body.to_string()),
        "yaml" => crate::serialize::yaml_decode(lua, body.to_string()),
        "toml" => crate::serialize::toml_decode(lua, body.to_string()),
        "csv" => crate::serialize::csv_decode(lua, body.to_string()),
        other => Err(format!("unknown decode format: {other}")),
    }
}

fn map_ureq_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        other => format!("HTTP error: {other}"),
    }
}

fn value_to_string(val: &mlua::Value) -> Option<String> {
    match val {
        mlua::Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
        mlua::Value::Integer(i) => Some(i.to_string()),
        mlua::Value::Number(n) => Some(n.to_string()),
        mlua::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn url_encode(s: &str) -> String {
    // Minimal percent-encoding for query parameters
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn form_encode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}
