//! Lua HTTP client — vendored libcurl via curl-sys (static-curl + static-ssl).
//! Fully static, no system dependencies beyond the kernel.

use curl::easy::{Easy, List};
use std::time::Duration;

pub fn do_request<'lua>(
    lua: &'lua mlua::Lua,
    method: &str,
    url: String,
    opts: Option<mlua::Table<'lua>>,
) -> Result<mlua::Value<'lua>, String> {
    let mut headers: Vec<String> = Vec::new();
    let mut body_data: Option<String> = None;
    let mut decode_format: Option<String> = None;

    if let Some(ref table) = opts {
        if let Ok(hdrs) = table.get::<_, mlua::Table>("headers") {
            for pair in hdrs.pairs::<mlua::String, mlua::String>() {
                let (k, v) = pair.map_err(|e| format!("header: {e}"))?;
                headers.push(format!("{}: {}", k.to_str().unwrap(), v.to_str().unwrap()));
            }
        }
        if let Ok(b) = table.get::<_, String>("body") { body_data = Some(b); }
        decode_format = table.get::<_, String>("decode").ok();
    }

    let mut h = Easy::new();
    h.url(&url).map_err(|e| format!("url: {e}"))?;
    h.timeout(Duration::from_secs(10)).map_err(|e| format!("timeout: {e}"))?;
    h.connect_timeout(Duration::from_secs(5)).map_err(|e| format!("connect: {e}"))?;
    h.follow_location(true).map_err(|e| format!("redirect: {e}"))?;
    if method != "GET" { h.custom_request(method).map_err(|e| format!("method: {e}"))?; }
    if !headers.is_empty() {
        let mut l = List::new();
        for hdr in &headers { l.append(hdr).map_err(|e| format!("header: {e}"))?; }
        h.http_headers(l).map_err(|e| format!("headers: {e}"))?;
    }
    if let Some(ref b) = body_data { h.post_fields_copy(b.as_bytes()).map_err(|e| format!("body: {e}"))?; }

    let mut body = Vec::new();
    let mut raw_hdrs = Vec::new();
    {
        let mut tx = h.transfer();
        tx.write_function(|d| { body.extend_from_slice(d); Ok(d.len()) }).map_err(|e| format!("write: {e}"))?;
        tx.header_function(|d| { raw_hdrs.extend_from_slice(d); true }).map_err(|e| format!("hdr: {e}"))?;
        tx.perform().map_err(|e| format!("perform: {e}"))?;
    }

    let status: u32 = h.response_code().map_err(|e| format!("status: {e}"))?;
    let body_str = String::from_utf8_lossy(&body).to_string();

    if method == "GET" && opts.is_none() {
        return Ok(mlua::Value::String(lua.create_string(&body_str).map_err(|e| format!("{e}"))?));
    }

    let t = lua.create_table().map_err(|e| format!("{e}"))?;
    t.set("status", status as i64).map_err(|e| format!("{e}"))?;
    t.set("ok", (200..300).contains(&status)).map_err(|e| format!("{e}"))?;
    t.set("body", if let Some(ref fmt) = decode_format {
        match fmt.as_str() {
            "json" => crate::serialize::json_decode(lua, body_str)?,
            "yaml" => crate::serialize::yaml_decode(lua, body_str)?,
            "toml" => crate::serialize::toml_decode(lua, body_str)?,
            "csv" => crate::serialize::csv_decode(lua, body_str)?,
            _ => return Err(format!("unknown decode: {fmt}")),
        }
    } else {
        mlua::Value::String(lua.create_string(&body_str).map_err(|e| format!("{e}"))?)
    }).map_err(|e| format!("{e}"))?;

    let ht = lua.create_table().map_err(|e| format!("{e}"))?;
    for line in String::from_utf8_lossy(&raw_hdrs).lines() {
        if let Some((n, v)) = line.split_once(':') {
            let n = n.trim().to_string();
            let v = v.trim().to_string();
            if !n.is_empty() { ht.set(n, v).map_err(|e| format!("{e}"))?; }
        }
    }
    t.set("headers", ht).map_err(|e| format!("{e}"))?;
    Ok(mlua::Value::Table(t))
}
