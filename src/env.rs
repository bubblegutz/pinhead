use rlua::Lua;

/// Register `env.*` Lua API functions so scripts can read, set, and unset
/// environment variables at runtime.
///
/// Available functions:
///   env.get("KEY")     -> string | nil   (returns nil when not found)
///   env.set("KEY", v)  -> sets KEY to v  (overwrites existing)
///   env.unset("KEY")   -> removes KEY
pub fn register_lua_apis(lua: &Lua) -> Result<(), String> {
    let t = lua.create_table().map_err(|e| format!("{e}"))?;

    // env.get("KEY") -> string | nil
    {
        let f = lua
            .create_function(|_, key: String| -> Result<Option<String>, rlua::Error> {
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

    lua.globals().set("env", t)
        .map_err(|e| format!("{e}"))?;

    Ok(())
}
