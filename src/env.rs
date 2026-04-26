use rlua::{Lua, Value};

/// Register `env.*` Lua API functions so scripts can read, set, and unset
/// environment variables at runtime.
///
/// Available functions:
///   env.get("KEY")     -> string | nil   (returns nil when not found)
///   env.set("KEY", v)  -> sets KEY to v  (overwrites existing)
///   env.unset("KEY")   -> removes KEY
///
/// The env table also supports table-like access:
///   env.USER           -> string | nil
///   for k,v in pairs(env) do ... end
pub fn register_lua_apis(lua: &Lua) -> std::result::Result<(), String> {
    let t = lua.create_table().map_err(|e| format!("{e}"))?;

    // env.get("KEY") -> string | nil
    {
        let f = lua
            .create_function(|_, key: String| -> rlua::Result<Option<String>> {
                Ok(std::env::var(key).ok())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("get", f).map_err(|e| format!("{e}"))?;
    }

    // env.set("KEY", "value")
    {
        let f = lua
            .create_function(|_, (key, val): (String, String)| {
                unsafe { std::env::set_var(&key, &val); }
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("set", f).map_err(|e| format!("{e}"))?;
    }

    // env.unset("KEY")
    {
        let f = lua
            .create_function(|_, key: String| {
                unsafe { std::env::remove_var(&key); }
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("unset", f).map_err(|e| format!("{e}"))?;
    }

    // Set up metatable for table-like access:
    //   __index: env.KEY -> value (or nil)
    //   __pairs: for k,v in pairs(env) do ... end
    {
        let mt = lua.create_table().map_err(|e| format!("{e}"))?;

        let index = lua
            .create_function(|lua, (_t, key): (rlua::Table, String)| -> rlua::Result<Value> {
                match std::env::var(&key) {
                    Ok(val) => Ok(Value::String(lua.create_string(&val)?)),
                    Err(_) => Ok(Value::Nil),
                }
            })
            .map_err(|e| format!("{e}"))?;
        mt.set("__index", index).map_err(|e| format!("{e}"))?;

        // Pairs iterator: snapshot env vars into a table at call time,
        // then delegate to the global `next` function.
        let pairs_fn = lua
            .create_function(|lua, _: ()| -> rlua::Result<(rlua::Function, rlua::Table, Value)> {
                let vars = lua.create_table()?;
                for (k, v) in std::env::vars() {
                    vars.set(k, v)?;
                }
                let next_fn = lua.globals().get::<_, rlua::Function>("next")?;
                Ok((next_fn, vars, Value::Nil))
            })
            .map_err(|e| format!("{e}"))?;
        mt.set("__pairs", pairs_fn).map_err(|e| format!("{e}"))?;

        t.set_metatable(Some(mt));
    }

    lua.globals().set("env", t)
        .map_err(|e| format!("{e}"))?;

    Ok(())
}
