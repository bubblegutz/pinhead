//! SQLite-backed database API for Lua — document store and relational SQL.
//!
//! ## Architecture (CSP)
//!
//! Each open database spawns two background coroutines:
//!
//! - **1 writer task** — owns a read-write `rusqlite::Connection`.  All
//!   mutations go through a dedicated `mpsc` channel → FIFO serialized.
//! - **1 reader task** — owns a read-only `rusqlite::Connection`.  All
//!   queries go through a separate `mpsc` channel.
//!
//! Both channels carry typed requests with `oneshot` reply channels.
//!
//! ## Lua API
//!
//! ### `doc.*` — document store (SQLite + JSON1)
//!
//! ```lua
//! local h = doc.open("data.db")
//! doc.set(h, "alice", {name = "Alice", age = 30})
//! local alice = doc.get(h, "alice")
//! doc.delete(h, "alice")
//! local results = doc.find(h, "$.age", 30)
//! local all = doc.all(h)
//! local n = doc.count(h)
//! doc.close(h)
//! ```
//!
//! ### `sql.*` — raw relational SQL
//!
//! ```lua
//! local h = sql.open("data.db")
//! sql.exec(h, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
//! sql.exec(h, "INSERT INTO t (name) VALUES (?1)", "Alice")
//! local rows = sql.query(h, "SELECT * FROM t")
//! local row = sql.row(h, "SELECT * FROM t WHERE id = ?1", 1)
//! sql.close(h)
//! ```

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A handle to an open database.  Passed to `doc.*` / `sql.*` functions.
#[derive(Clone, Debug)]
pub struct DbHandle {
    pub(crate) id: u64,
    pub(crate) write_tx: mpsc::Sender<WriteRequest>,
    pub(crate) read_tx: mpsc::Sender<ReadRequest>,
}

/// SQL parameter value.
#[derive(Debug, Clone)]
pub enum Param {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

/// A database row: column name → value.
pub type Row = HashMap<String, Value>;

/// A value returned from a query.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

// ---------------------------------------------------------------------------
// Internal request types
// ---------------------------------------------------------------------------

pub(crate) enum WriteRequest {
    Exec {
        sql: String,
        params: Vec<Param>,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    Close {
        reply: mpsc::Sender<()>,
    },
}

pub(crate) enum ReadRequest {
    Query {
        sql: String,
        params: Vec<Param>,
        reply: mpsc::Sender<Result<Vec<Row>, String>>,
    },
}

// ---------------------------------------------------------------------------
// Handle registry
// ---------------------------------------------------------------------------

/// Manages all open database handles, shared across Lua closures.
pub struct DbRegistry {
    handles: HashMap<u64, DbHandle>,
    next_id: u64,
}

impl DbRegistry {
    pub fn new() -> Self {
        Self {
            handles: HashMap::new(),
            next_id: 1,
        }
    }

    /// Open a database file and return a handle.  Spawns writer + reader tasks.
    /// `init_sql` is executed synchronously on the writer connection before
    /// the reader is spawned (avoids a file-does-not-exist race).
    pub fn open(
        &mut self,
        path: &str,
        init_sql: Option<&str>,
    ) -> Result<DbHandle, String> {
        let id = self.next_id;
        self.next_id += 1;

        // Open writer connection synchronously so the database file exists
        // before the reader task tries to open it read-only.
        let writer_conn = rusqlite::Connection::open(path)
            .map_err(|e| format!("open writer connection: {e}"))?;
        let _ = writer_conn.execute_batch("PRAGMA journal_mode=WAL;");
        if let Some(sql) = init_sql {
            writer_conn
                .execute_batch(sql)
                .map_err(|e| format!("init SQL: {e}"))?;
        }

        // Open reader connection now too (file exists).
        let reader_conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open reader connection: {e}"))?;

        let (write_tx, write_rx) = mpsc::channel::<WriteRequest>();
        let (read_tx, read_rx) = mpsc::channel::<ReadRequest>();

        // Spawn the single writer task on blocking pool (serialized writes).
        tokio::task::spawn_blocking(move || {
            run_writer_conn(writer_conn, write_rx);
        });

        // Spawn the single reader task on blocking pool.
        tokio::task::spawn_blocking(move || {
            run_reader_conn(reader_conn, read_rx);
        });

        let handle = DbHandle {
            id,
            write_tx,
            read_tx,
        };
        self.handles.insert(id, handle.clone());
        Ok(handle)
    }

    pub fn remove(&mut self, id: u64) {
        self.handles.remove(&id);
    }

    pub fn get(&self, id: u64) -> Option<&DbHandle> {
        self.handles.get(&id)
    }
}

// ---------------------------------------------------------------------------
// Writer task
// --------------------------------------------------------------------------

fn run_writer_conn(
    conn: rusqlite::Connection,
    rx: mpsc::Receiver<WriteRequest>,
) {
    loop {
        match rx.recv() {
            Ok(WriteRequest::Exec { sql, params, reply }) => {
                let result = exec_sql(&conn, &sql, &params);
                let _ = reply.send(result);
            }
            Ok(WriteRequest::Close { reply }) => {
                let _ = reply.send(());
                break;
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Reader task
// ---------------------------------------------------------------------------

fn run_reader_conn(
    conn: rusqlite::Connection,
    rx: mpsc::Receiver<ReadRequest>,
) {
    loop {
        match rx.recv() {
            Ok(ReadRequest::Query { sql, params, reply }) => {
                let result = query_sql(&conn, &sql, &params);
                let _ = reply.send(result);
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// SQL helpers
// ---------------------------------------------------------------------------

fn exec_sql(conn: &rusqlite::Connection, sql: &str, params: &[Param]) -> Result<u64, String> {
    let p: Vec<Box<dyn rusqlite::types::ToSql>> =
        params.iter().map(param_to_box).collect();
    let refs: Vec<&dyn rusqlite::types::ToSql> = p.iter().map(|p| p.as_ref()).collect();
    let count = conn
        .execute(sql, refs.as_slice())
        .map_err(|e| format!("SQL exec error: {e}"))?;
    Ok(count as u64)
}

fn query_sql(conn: &rusqlite::Connection, sql: &str, params: &[Param]) -> Result<Vec<Row>, String> {
    let p: Vec<Box<dyn rusqlite::types::ToSql>> =
        params.iter().map(param_to_box).collect();
    let refs: Vec<&dyn rusqlite::types::ToSql> = p.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("SQL prepare error: {e}"))?;

    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let rows = stmt
        .query_map(refs.as_slice(), |row| {
            let mut map = HashMap::new();
            for (i, name) in col_names.iter().enumerate() {
                map.insert(name.clone(), read_cell(row, i));
            }
            Ok(map)
        })
        .map_err(|e| format!("SQL query error: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("SQL row error: {e}"))?;

    Ok(rows)
}

fn param_to_box(p: &Param) -> Box<dyn rusqlite::types::ToSql> {
    match p {
        Param::Null => Box::new(rusqlite::types::Null),
        Param::Integer(i) => Box::new(*i),
        Param::Real(f) => Box::new(*f),
        Param::Text(s) => Box::new(s.clone()),
    }
}

fn read_cell(row: &rusqlite::Row, i: usize) -> Value {
    use rusqlite::types::Type;
    match row.get_ref(i) {
        Ok(r) => match r.data_type() {
            Type::Null => Value::Null,
            Type::Integer => Value::Integer(r.as_i64().unwrap_or(0)),
            Type::Real => Value::Real(r.as_f64().unwrap_or(0.0)),
            Type::Text => Value::Text(r.as_str().unwrap_or("").to_string()),
            Type::Blob => {
                let n = r.as_bytes().map(|b| b.len()).unwrap_or(0);
                Value::Text(format!("<blob {n} bytes>"))
            }
        },
        Err(_) => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Blocking helpers (for synchronous Lua closures)
// ---------------------------------------------------------------------------

pub(crate) fn send_exec_writer(
    tx: &mpsc::Sender<WriteRequest>,
    sql: &str,
    params: Vec<Param>,
) -> Result<u64, String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    tx.send(WriteRequest::Exec {
        sql: sql.to_string(),
        params,
        reply: reply_tx,
    })
    .map_err(|_| "writer task gone".to_string())?;
    reply_rx
        .recv()
        .map_err(|_| "writer reply lost".to_string())?
}

pub(crate) fn send_query_reader(
    tx: &mpsc::Sender<ReadRequest>,
    sql: &str,
    params: Vec<Param>,
) -> Result<Vec<Row>, String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    tx.send(ReadRequest::Query {
        sql: sql.to_string(),
        params,
        reply: reply_tx,
    })
    .map_err(|_| "reader task gone".to_string())?;
    reply_rx
        .recv()
        .map_err(|_| "reader reply lost".to_string())?
}

pub(crate) fn send_row_reader(
    tx: &mpsc::Sender<ReadRequest>,
    sql: &str,
    params: Vec<Param>,
) -> Result<Option<Row>, String> {
    let mut rows = send_query_reader(tx, sql, params)?;
    Ok(rows.drain(..).next())
}

pub(crate) fn send_close_writer(tx: &mpsc::Sender<WriteRequest>) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    tx.send(WriteRequest::Close { reply: reply_tx })
        .map_err(|_| "writer task gone".to_string())?;
    reply_rx
        .recv()
        .map_err(|_| "writer reply lost".to_string())
}

// ---------------------------------------------------------------------------
// Lua API registration
// ---------------------------------------------------------------------------

/// Register `doc.*` and `sql.*` Lua tables.  Called from `HandlerRuntime::new`.
pub fn register_lua_apis(
    lua: &mlua::Lua,
) -> Result<(Arc<Mutex<DbRegistry>>, Arc<Mutex<DbRegistry>>), String> {
    let doc_reg = Arc::new(Mutex::new(DbRegistry::new()));
    let sql_reg = Arc::new(Mutex::new(DbRegistry::new()));

    register_doc_api(lua, doc_reg.clone())?;
    register_sql_api(lua, sql_reg.clone())?;

    Ok((doc_reg, sql_reg))
}

fn get_handle(
    reg: &Mutex<DbRegistry>,
    id: i64,
) -> Result<DbHandle, mlua::Error> {
    let r = reg.lock().map_err(|_| {
        mlua::Error::RuntimeError("database registry lock poisoned".into())
    })?;
    r.get(id as u64)
        .cloned()
        .ok_or_else(|| mlua::Error::RuntimeError("invalid database handle".into()))
}

/// Map a Result<T, String> to Result<T, mlua::Error>.
fn map_err<T>(r: Result<T, String>) -> Result<T, mlua::Error> {
    r.map_err(|e| mlua::Error::RuntimeError(e.into()))
}

fn register_doc_api(lua: &mlua::Lua, reg: Arc<Mutex<DbRegistry>>) -> Result<(), String> {
    let t = lua.create_table().map_err(|e| format!("{e}"))?;

    // doc.open(path) → handle_id
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |_, path: String| {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64, mlua::Error> {
                    let mut r = reg.lock().map_err(|_| {
                        mlua::Error::RuntimeError("registry lock poisoned".into())
                    })?;
                    let h = r
                        .open(&path, Some("CREATE TABLE IF NOT EXISTS docs (key TEXT PRIMARY KEY, value TEXT)"))
                        .map_err(|e| mlua::Error::RuntimeError(e.into()))?;
                    Ok(h.id as i64)
                }));
                match r {
                    Ok(v) => v,
                    Err(_) => Ok(0),
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("open", fn_).map_err(|e| format!("{e}"))?;
    }

    // doc.close(handle_id)
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |_, handle_id: i64| {
                let h = get_handle(&reg, handle_id)?;
                let _ = send_close_writer(&h.write_tx);
                reg.lock()
                    .map_err(|_| mlua::Error::RuntimeError("registry lock poisoned".into()))?
                    .remove(handle_id as u64);
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("close", fn_).map_err(|e| format!("{e}"))?;
    }

    // doc.set(handle_id, key, value)
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |lua, (handle_id, key, value): (i64, String, mlua::Value)| {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), mlua::Error> {
                    let h = get_handle(&reg, handle_id)?;
                    let json = map_err(crate::serialize::json_encode(lua, value))?;
                    map_err(send_exec_writer(
                        &h.write_tx,
                        "INSERT INTO docs (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        vec![Param::Text(key), Param::Text(json)],
                    ))?;
                    Ok(())
                }));
                let _ = r;
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("set", fn_).map_err(|e| format!("{e}"))?;
    }

    // doc.get(handle_id, key) → value or nil
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |lua, (handle_id, key): (i64, String)| {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<mlua::Value, mlua::Error> {
                    let h = get_handle(&reg, handle_id)?;
                    let row = map_err(send_row_reader(
                        &h.read_tx,
                        "SELECT value FROM docs WHERE key = ?1",
                        vec![Param::Text(key)],
                    ))?;
                    match row.and_then(|r| r.get("value").cloned()) {
                        Some(Value::Text(s)) => {
                            let val = map_err(crate::serialize::json_decode(lua, s))?;
                            Ok(val)
                        }
                        _ => Ok(mlua::Value::Nil),
                    }
                }));
                match r {
                    Ok(v) => v,
                    Err(_) => Ok(mlua::Value::Nil),
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("get", fn_).map_err(|e| format!("{e}"))?;
    }

    // doc.delete(handle_id, key)
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |_, (handle_id, key): (i64, String)| {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), mlua::Error> {
                    let h = get_handle(&reg, handle_id)?;
                    map_err(send_exec_writer(
                        &h.write_tx,
                        "DELETE FROM docs WHERE key = ?1",
                        vec![Param::Text(key)],
                    ))?;
                    Ok(())
                }));
                let _ = r;
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("delete", fn_).map_err(|e| format!("{e}"))?;
    }

    // doc.find(handle_id, json_path, value) → array of {key, value}
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |lua, (handle_id, json_path, value): (i64, String, String)| {
                let h = get_handle(&reg, handle_id)?;
                let rows = map_err(send_query_reader(
                    &h.read_tx,
                    "SELECT key, value FROM docs WHERE json_extract(value, ?1) = ?2",
                    vec![Param::Text(json_path), Param::Text(value)],
                ))?;
                let result = map_err(kv_rows_to_lua(lua, &rows))?;
                Ok(result)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("find", fn_).map_err(|e| format!("{e}"))?;
    }

    // doc.all(handle_id) → array of {key, value}
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |lua, handle_id: i64| {
                let h = get_handle(&reg, handle_id)?;
                let rows = map_err(send_query_reader(
                    &h.read_tx,
                    "SELECT key, value FROM docs ORDER BY key",
                    vec![],
                ))?;
                let result = map_err(kv_rows_to_lua(lua, &rows))?;
                Ok(result)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("all", fn_).map_err(|e| format!("{e}"))?;
    }

    // doc.count(handle_id) → integer
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |_, handle_id: i64| {
                let h = get_handle(&reg, handle_id)?;
                let row = map_err(send_row_reader(
                    &h.read_tx,
                    "SELECT COUNT(*) AS cnt FROM docs",
                    vec![],
                ))?;
                let n = row
                    .and_then(|r| r.get("cnt").cloned())
                    .map(|v| match v {
                        Value::Integer(i) => i,
                        _ => 0,
                    })
                    .unwrap_or(0);
                Ok(n)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("count", fn_).map_err(|e| format!("{e}"))?;
    }

    lua.globals().set("doc", t).map_err(|e| format!("{e}"))?;
    Ok(())
}

fn register_sql_api(lua: &mlua::Lua, reg: Arc<Mutex<DbRegistry>>) -> Result<(), String> {
    let t = lua.create_table().map_err(|e| format!("{e}"))?;

    // sql.open(path) → handle_id
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |_, path: String| {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64, mlua::Error> {
                    let mut r = reg.lock().map_err(|_| {
                        mlua::Error::RuntimeError("registry lock poisoned".into())
                    })?;
                    let h = r
                        .open(&path, None)
                        .map_err(|e| mlua::Error::RuntimeError(e.into()))?;
                    Ok(h.id as i64)
                }));
                match r {
                    Ok(v) => v,
                    Err(_) => Ok(0),
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("open", fn_).map_err(|e| format!("{e}"))?;
    }

    // sql.close(handle_id)
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(move |_, handle_id: i64| {
                let h = get_handle(&reg, handle_id)?;
                let _ = send_close_writer(&h.write_tx);
                reg.lock()
                    .map_err(|_| mlua::Error::RuntimeError("registry lock poisoned".into()))?
                    .remove(handle_id as u64);
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("close", fn_).map_err(|e| format!("{e}"))?;
    }

    // sql.exec(handle_id, sql, params) → rows_affected
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(
                move |_, (handle_id, sql, params_val): (i64, String, mlua::Value)| {
                    let h = get_handle(&reg, handle_id)?;
                    let params = lua_value_to_params(params_val);
                    let n = map_err(send_exec_writer(&h.write_tx, &sql, params))?;
                    Ok(n as i64)
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("exec", fn_).map_err(|e| format!("{e}"))?;
    }

    // sql.query(handle_id, sql, params) → array of row tables
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(
                move |lua, (handle_id, sql, params_val): (i64, String, mlua::Value)| {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<mlua::Value, mlua::Error> {
                        let h = get_handle(&reg, handle_id)?;
                        let params = lua_value_to_params(params_val);
                        let rows = map_err(send_query_reader(&h.read_tx, &sql, params))?;
                        let result = map_err(sql_rows_to_lua(lua, &rows))?;
                        Ok(result)
                    }));
                    match r {
                        Ok(v) => v,
                        Err(_) => Ok(mlua::Value::Nil),
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("query", fn_).map_err(|e| format!("{e}"))?;
    }

    // sql.row(handle_id, sql, params) → single row table or nil
    {
        let reg = reg.clone();
        let fn_ = lua
            .create_function(
                move |lua, (handle_id, sql, params_val): (i64, String, mlua::Value)| {
                    let h = get_handle(&reg, handle_id)?;
                    let params = lua_value_to_params(params_val);
                    let row = match map_err(send_row_reader(&h.read_tx, &sql, params))? {
                        Some(r) => r,
                        None => return Ok(mlua::Value::Nil),
                    };
                    let t = map_err(lua.create_table().map_err(|e| format!("{e}")))?;
                    for (k, v) in &row {
                        set_lua_value(lua, &t, k, v)?;
                    }
                    Ok(mlua::Value::Table(t))
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("row", fn_).map_err(|e| format!("{e}"))?;
    }

    lua.globals().set("sql", t).map_err(|e| format!("{e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Lua value helpers
// ---------------------------------------------------------------------------

fn set_lua_value(
    lua: &mlua::Lua,
    t: &mlua::Table,
    key: &str,
    val: &Value,
) -> Result<(), mlua::Error> {
    match val {
        Value::Null => t.set(key, mlua::Value::Nil),
        Value::Integer(i) => t.set(key, *i),
        Value::Real(f) => t.set(key, *f),
        Value::Text(s) => {
            let ls = lua
                .create_string(s.as_bytes())
                .map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
            t.set(key, ls)
        }
    }
}

fn lua_value_to_params(val: mlua::Value) -> Vec<Param> {
    match val {
        mlua::Value::Nil => vec![],
        mlua::Value::Integer(i) => vec![Param::Integer(i)],
        mlua::Value::Number(f) => vec![Param::Real(f)],
        mlua::Value::String(s) => {
            vec![Param::Text(s.to_str().unwrap_or("").to_string())]
        }
        mlua::Value::Table(t) => {
            let mut params = Vec::new();
            for i in 1..=t.len().unwrap_or(0) {
                let v: mlua::Value = t.get(i).unwrap_or(mlua::Value::Nil);
                params.push(lua_single_to_param(v));
            }
            params
        }
        mlua::Value::Boolean(b) => vec![Param::Integer(if b { 1 } else { 0 })],
        _ => vec![],
    }
}

fn lua_single_to_param(val: mlua::Value) -> Param {
    match val {
        mlua::Value::Nil => Param::Null,
        mlua::Value::Integer(i) => Param::Integer(i),
        mlua::Value::Number(f) => Param::Real(f),
        mlua::Value::String(s) => Param::Text(s.to_str().unwrap_or("").to_string()),
        mlua::Value::Boolean(b) => Param::Integer(if b { 1 } else { 0 }),
        _ => Param::Null,
    }
}

// ---------------------------------------------------------------------------
// Rows → Lua table conversion
// ---------------------------------------------------------------------------

/// Convert key/value rows (from doc.* queries) to a Lua array table.
fn kv_rows_to_lua<'lua>(
    lua: &'lua mlua::Lua,
    rows: &[Row],
) -> Result<mlua::Value<'lua>, String> {
    let t = lua.create_table().map_err(|e| format!("{e}"))?;
    for (i, row) in rows.iter().enumerate() {
        let entry = lua.create_table().map_err(|e| format!("{e}"))?;
        if let Some(Value::Text(k)) = row.get("key") {
            let ls = lua
                .create_string(k.as_bytes())
                .map_err(|e| format!("{e}"))?;
            entry.set("key", ls).map_err(|e| format!("{e}"))?;
        }
        if let Some(Value::Text(v)) = row.get("value") {
            let val = crate::serialize::json_decode(lua, v.clone())
                .unwrap_or(mlua::Value::Nil);
            entry.set("value", val).map_err(|e| format!("{e}"))?;
        }
        t.set(i + 1, entry).map_err(|e| format!("{e}"))?;
    }
    Ok(mlua::Value::Table(t))
}

/// Convert generic SQL rows to a Lua array table.
fn sql_rows_to_lua<'lua>(
    lua: &'lua mlua::Lua,
    rows: &[Row],
) -> Result<mlua::Value<'lua>, String> {
    let t = lua.create_table().map_err(|e| format!("{e}"))?;
    for (i, row) in rows.iter().enumerate() {
        let entry = lua.create_table().map_err(|e| format!("{e}"))?;
        for (k, v) in row {
            let val: mlua::Value = match v {
                Value::Null => mlua::Value::Nil,
                Value::Integer(n) => mlua::Value::Integer(*n),
                Value::Real(f) => mlua::Value::Number(*f),
                Value::Text(s) => lua
                    .create_string(s.as_bytes())
                    .map(mlua::Value::String)
                    .unwrap_or(mlua::Value::Nil),
            };
            entry.set(k.as_str(), val).map_err(|e| format!("{e}"))?;
        }
        t.set(i + 1, entry).map_err(|e| format!("{e}"))?;
    }
    Ok(mlua::Value::Table(t))
}
