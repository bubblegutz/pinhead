use mlua::{Lua, Value};

/// Register `env.*` Lua API functions so scripts can read, set, and unset
/// environment variables at runtime.
///
/// Available functions (dispatched via metatable __index):
///   env.get("KEY")     -> string | nil   (returns nil when not found)
///   env.set("KEY", v)  -> sets KEY to v  (overwrites existing)
///   env.unset("KEY")   -> removes KEY
///
/// The env table also supports table-like access (via __index):
///   env.USER           -> string | nil
///   for k,v in pairs(env) do ... end
pub fn register_lua_apis(lua: &Lua) -> std::result::Result<(), String> {
    // Create the env table — initially empty so pairs only yields env vars.
    let t = lua.create_table().map_err(|e| format!("{e}"))?;

    // Store get/set/unset as RegistryKeys so they outlive the &Lua ref.
    let get_key = {
        let f = lua
            .create_function(|_, key: String| -> mlua::Result<Option<String>> {
                Ok(std::env::var(key).ok())
            })
            .map_err(|e| format!("{e}"))?;
        lua.create_registry_value(f).map_err(|e| format!("{e}"))?
    };
    let set_key = {
        let f = lua
            .create_function(|_, (key, val): (String, String)| {
                unsafe { std::env::set_var(&key, &val); }
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        lua.create_registry_value(f).map_err(|e| format!("{e}"))?
    };
    let unset_key = {
        let f = lua
            .create_function(|_, key: String| {
                unsafe { std::env::remove_var(&key); }
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        lua.create_registry_value(f).map_err(|e| format!("{e}"))?
    };

    // Set up metatable:
    //   __index   — dispatches methods (get/set/unset) and env var reads
    //   __pairs   — snapshot env vars for for k,v in pairs(env) do ... end
    {
        let mt = lua.create_table().map_err(|e| format!("{e}"))?;

        // __index(t, key):
        //   - "get"/"set"/"unset" → return the function from registry
        //   - anything else       → return env var value (or nil)
        let index = lua
            .create_function(
                move |lua, (_t, key): (mlua::Table, String)| -> mlua::Result<Value> {
                    let f = match key.as_str() {
                        "get" => Some(lua.registry_value::<mlua::Function>(&get_key)?),
                        "set" => Some(lua.registry_value::<mlua::Function>(&set_key)?),
                        "unset" => Some(lua.registry_value::<mlua::Function>(&unset_key)?),
                        _ => None,
                    };
                    if let Some(func) = f {
                        return Ok(Value::Function(func));
                    }
                    // Fall back to env var.
                    match std::env::var(&key) {
                        Ok(val) => Ok(Value::String(lua.create_string(&val)?)),
                        Err(_) => Ok(Value::Nil),
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;
        mt.set("__index", index).map_err(|e| format!("{e}"))?;

        // __pairs(t): snapshot env vars into a new table, then delegate
        // to the global `next` function.
        let pairs_fn = lua
            .create_function(
                |lua, _t: mlua::Table| -> mlua::Result<(mlua::Function, mlua::Table, Value)> {
                    let vars = lua.create_table()?;
                    for (k, v) in std::env::vars() {
                        vars.set(k, v)?;
                    }
                    let next_fn = lua.globals().get::<_, mlua::Function>("next")?;
                    Ok((next_fn, vars, Value::Nil))
                },
            )
            .map_err(|e| format!("{e}"))?;
        mt.set("__pairs", pairs_fn).map_err(|e| format!("{e}"))?;

        t.set_metatable(Some(mt));
    }

    lua.globals().set("env", t)
        .map_err(|e| format!("{e}"))?;

    Ok(())
}
