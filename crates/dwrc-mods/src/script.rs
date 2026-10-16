//! Lua scripting for mods.
//!
//! Each mod gets its own isolated Lua 5.4 state with a `dwrc` API table:
//!
//! ```lua
//! dwrc.game              -- "dw1" | "dw2" | "dw3"
//! dwrc.options           -- this mod's option values
//! dwrc.data              -- persistent table, saved per mod
//! dwrc.log(msg)
//! dwrc.on_frame(function(ctx) ... end)
//! dwrc.on_exec(0x80012345, function(ctx) ... return false end)
//! dwrc.on_input(function(buttons) return buttons end) -- before each frame
//! dwrc.buttons.Cross    -- bit masks for each controller button
//! dwrc.save()            -- persist dwrc.data now
//! ```
//!
//! Callbacks receive a `ctx` giving access to the console while it is
//! paused at that point: `read8/16/32`, `read_s8/s16/s32`, `write8/16/32`,
//! `read_bytes`, `write_bytes`, `reg`, `set_reg`, `pc`, `set_pc`, `ret`,
//! `frame`, `buttons`, `pressed`. An exec hook that changes the PC (for
//! example with `ctx:ret(value)` to replace a whole function) must return
//! `true`.
//!
//! Scripts are sandboxed: only the `string`, `table`, `math`, `utf8` and
//! `coroutine` libraries are available, plus `os.time`, `os.clock` and `os.difftime`;
//! there is no `io`, no `os.execute`, no `debug`, no binary chunk loading,
//! and `require` only loads `.lua` files from the mod's own folder. A mod
//! installed from the internet can therefore touch the emulated console
//! and its own saved data, but not the player's computer.

use anyhow::{anyhow, Result};
use dwrc_psx::{Bus, Button, Cpu, HookAction};
use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, UserData, UserDataMethods, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const REG_NAMES: [&str; 32] = dwrc_mips::REG_NAMES;

#[derive(Default)]
struct HostState {
    input_cbs: Vec<RegistryKey>,
    frame_cbs: Vec<RegistryKey>,
    exec_hooks: Vec<(u32, Arc<RegistryKey>)>,
}

pub struct ScriptHost {
    id: String,
    lua: Lua,
    state: Arc<Mutex<HostState>>,
    data_dir: PathBuf,
    installed: Vec<u32>,
}

/// The `ctx` object handed to callbacks.
struct Ctx<'a> {
    cpu: &'a mut Cpu,
    bus: &'a mut Bus,
    frame: u64,
}

fn reg_index(v: &Value) -> mlua::Result<usize> {
    match v {
        Value::Integer(i) if (0..32).contains(i) => Ok(*i as usize),
        Value::String(s) => {
            let s = s.to_str()?;
            REG_NAMES
                .iter()
                .position(|n| *n == &*s)
                .ok_or_else(|| mlua::Error::runtime(format!("unknown register '{}'", &*s)))
        }
        _ => Err(mlua::Error::runtime("register must be 0..31 or a name like 'a0'")),
    }
}

fn button_from_name(s: &str) -> Option<Button> {
    Button::ALL.into_iter().find(|b| format!("{b:?}").eq_ignore_ascii_case(s))
}

impl UserData for Ctx<'_> {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("frame", |_, c, ()| Ok(c.frame));
        m.add_method("read8", |_, c, a: u32| Ok(c.bus.peek8(a)));
        m.add_method("read16", |_, c, a: u32| Ok(c.bus.peek16(a)));
        m.add_method("read32", |_, c, a: u32| Ok(c.bus.peek32(a)));
        m.add_method("read_s8", |_, c, a: u32| Ok(c.bus.peek8(a) as i8));
        m.add_method("read_s16", |_, c, a: u32| Ok(c.bus.peek16(a) as i16));
        m.add_method("read_s32", |_, c, a: u32| Ok(c.bus.peek32(a) as i32));
        m.add_method_mut("write8", |_, c, (a, v): (u32, i64)| {
            c.bus.poke8(a, v as u8);
            Ok(())
        });
        m.add_method_mut("write16", |_, c, (a, v): (u32, i64)| {
            c.bus.poke16(a, v as u16);
            Ok(())
        });
        m.add_method_mut("write32", |_, c, (a, v): (u32, i64)| {
            c.bus.poke32(a, v as u32);
            Ok(())
        });
        m.add_method("read_bytes", |lua, c, (a, n): (u32, u32)| {
            let v: Vec<u8> = (0..n.min(1 << 20)).map(|i| c.bus.peek8(a.wrapping_add(i))).collect();
            lua.create_string(&v)
        });
        m.add_method_mut("write_bytes", |_, c, (a, s): (u32, mlua::String)| {
            for (i, b) in s.as_bytes().iter().enumerate() {
                c.bus.poke8(a.wrapping_add(i as u32), *b);
            }
            Ok(())
        });
        m.add_method("reg", |_, c, r: Value| Ok(c.cpu.regs[reg_index(&r)?]));
        m.add_method_mut("set_reg", |_, c, (r, v): (Value, i64)| {
            let i = reg_index(&r)?;
            if i != 0 {
                c.cpu.set_reg(i, v as u32);
            }
            Ok(())
        });
        m.add_method("pc", |_, c, ()| Ok(c.cpu.pc));
        m.add_method_mut("set_pc", |_, c, a: u32| {
            c.cpu.set_pc(a);
            Ok(())
        });
        // Return from the current function (at its entry): v0 = value.
        m.add_method_mut("ret", |_, c, v: Option<i64>| {
            if let Some(v) = v {
                c.cpu.set_reg(2, v as u32);
            }
            let ra = c.cpu.regs[31];
            c.cpu.set_pc(ra);
            Ok(true)
        });
        m.add_method("buttons", |_, c, ()| Ok(c.bus.sio.pads[0].state.buttons));
        m.add_method("pressed", |_, c, name: String| {
            let b = button_from_name(&name).ok_or_else(|| mlua::Error::runtime(format!("unknown button '{name}'")))?;
            Ok(c.bus.sio.pads[0].state.pressed(b))
        });
    }
}

fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => serde_json::Value::Object(t.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect()),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
    }
}

/// Builds a Lua state with only the safe libraries, a text-only `load`, a
/// `require` confined to `root`, and `print` routed to the log.
fn sandboxed_lua(root: &std::path::Path, id: &str) -> mlua::Result<Lua> {
    use mlua::{ChunkMode, LuaOptions, StdLib};
    let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE, LuaOptions::default())?;
    lua.set_memory_limit(256 << 20)?;
    let g = lua.globals();
    for name in ["dofile", "loadfile"] {
        g.raw_set(name, Value::Nil)?;
    }
    let string: mlua::Table = g.get("string")?;
    string.raw_set("dump", Value::Nil)?;
    // load(): text chunks only (binary chunks can break out of the VM).
    lua.load(
        r#"
        local _load = load
        load = function(chunk, name, _, env) return _load(chunk, name, "t", env) end
        "#,
    )
    .set_mode(ChunkMode::Text)
    .exec()?;
    // A small, harmless part of `os`.
    let os = lua.create_table()?;
    os.set("time", lua.create_function(|_, t: Option<mlua::Table>| {
        use std::time::{SystemTime, UNIX_EPOCH};
        if t.is_some() {
            return Err(mlua::Error::runtime("os.time(table) is not available to mods"));
        }
        Ok(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
    })?)?;
    let start = std::time::Instant::now();
    os.set("clock", lua.create_function(move |_, ()| Ok(start.elapsed().as_secs_f64()))?)?;
    os.set("difftime", lua.create_function(|_, (a, b): (f64, Option<f64>)| Ok(a - b.unwrap_or(0.0)))?)?;
    g.set("os", os)?;
    let mid = id.to_string();
    g.set(
        "print",
        lua.create_function(move |_, args: mlua::MultiValue| {
            let parts: Vec<String> = args.iter().map(|v| v.to_string().unwrap_or_else(|_| "?".into())).collect();
            log::info!("[{mid}] {}", parts.join("\t"));
            Ok(())
        })?,
    )?;
    // require(): modules from the mod's own folder ("a.b" -> a/b.lua).
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let loaded = lua.create_table()?;
    g.set(
        "require",
        lua.create_function(move |lua, name: String| {
            if let Ok(v) = loaded.raw_get::<Value>(name.as_str()) {
                if !v.is_nil() {
                    return Ok(v);
                }
            }
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) || name.contains("..") {
                return Err(mlua::Error::runtime(format!("require: invalid module name '{name}'")));
            }
            let file = root.join(format!("{}.lua", name.replace('.', "/")));
            let ok = file.canonicalize().map(|f| f.starts_with(&root)).unwrap_or(false);
            if !ok {
                return Err(mlua::Error::runtime(format!("require: module '{name}' not found in the mod folder")));
            }
            let src = std::fs::read_to_string(&file).map_err(|e| mlua::Error::runtime(format!("require '{name}': {e}")))?;
            let v: Value = lua.load(&src).set_name(name.as_str()).set_mode(ChunkMode::Text).call(name.as_str())?;
            let v = if v.is_nil() { Value::Boolean(true) } else { v };
            loaded.raw_set(name.as_str(), v.clone())?;
            Ok(v)
        })?,
    )?;
    Ok(lua)
}

impl ScriptHost {
    pub fn load(id: &str, game: dwrc_disc::GameId, path: &std::path::Path, options: crate::OptionValues, data_dir: PathBuf) -> Result<Self> {
        let lua = sandboxed_lua(path.parent().unwrap_or(std::path::Path::new(".")), id).map_err(|e| anyhow!("{e}"))?;
        let state = Arc::new(Mutex::new(HostState::default()));
        {
            let api = lua.create_table().map_err(|e| anyhow!("{e}"))?;
            let le = |e: mlua::Error| anyhow!("{e}");
            api.set("game", game.slug()).map_err(le)?;
            api.set("version", env!("CARGO_PKG_VERSION")).map_err(le)?;
            let mid = id.to_string();
            api.set(
                "log",
                lua.create_function(move |_, msg: String| {
                    log::info!("[{mid}] {msg}");
                    Ok(())
                })
                .map_err(le)?,
            )
            .map_err(le)?;
            let st = state.clone();
            api.set(
                "on_frame",
                lua.create_function(move |lua, f: Function| {
                    let key = lua.create_registry_value(f)?;
                    st.lock().unwrap().frame_cbs.push(key);
                    Ok(())
                })
                .map_err(le)?,
            )
            .map_err(le)?;
            let st = state.clone();
            api.set(
                "on_input",
                lua.create_function(move |lua, f: Function| {
                    let key = lua.create_registry_value(f)?;
                    st.lock().unwrap().input_cbs.push(key);
                    Ok(())
                })
                .map_err(le)?,
            )
            .map_err(le)?;
            let buttons = lua.create_table().map_err(le)?;
            for b in Button::ALL {
                buttons.set(format!("{b:?}"), 1u32 << (b as u32)).map_err(le)?;
            }
            api.set("buttons", buttons).map_err(le)?;
            let st = state.clone();
            api.set(
                "on_exec",
                lua.create_function(move |lua, (addr, f): (u32, Function)| {
                    if addr & 3 != 0 {
                        return Err(mlua::Error::runtime("hook address must be word aligned"));
                    }
                    let key = lua.create_registry_value(f)?;
                    st.lock().unwrap().exec_hooks.push((addr, Arc::new(key)));
                    Ok(())
                })
                .map_err(le)?,
            )
            .map_err(le)?;
            let opts: serde_json::Map<String, serde_json::Value> =
                options.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect();
            api.set("options", lua.to_value(&serde_json::Value::Object(opts)).map_err(le)?).map_err(le)?;
            let data_json: serde_json::Value = std::fs::read_to_string(data_dir.join("data.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let data_val = lua.to_value(&data_json).map_err(le)?;
            let data_val = if matches!(data_val, Value::Table(_)) { data_val } else { Value::Table(lua.create_table().map_err(le)?) };
            api.set("data", data_val).map_err(le)?;
            let dd = data_dir.clone();
            api.set(
                "save",
                lua.create_function(move |lua, ()| {
                    let api: mlua::Table = lua.globals().get("dwrc")?;
                    let data: Value = api.get("data")?;
                    let j: serde_json::Value = lua.from_value(data)?;
                    let _ = std::fs::create_dir_all(&dd);
                    std::fs::write(dd.join("data.json"), serde_json::to_string_pretty(&j).unwrap_or_default())
                        .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                    Ok(())
                })
                .map_err(le)?,
            )
            .map_err(le)?;
            lua.globals().set("dwrc", api).map_err(le)?;
        }
        let src = std::fs::read_to_string(path)?;
        lua.load(&src)
            .set_name(path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
            .set_mode(mlua::ChunkMode::Text)
            .exec()
            .map_err(|e| anyhow!("{e}"))?;
        Ok(Self { id: id.to_string(), lua, state, data_dir, installed: Vec::new() })
    }

    pub fn mod_id(&self) -> &str {
        &self.id
    }

    pub fn install_hooks(&mut self, bus: &mut Bus) {
        let hooks = self.state.lock().unwrap().exec_hooks.clone();
        for (addr, key) in hooks {
            let lua = self.lua.clone();
            let id = self.id.clone();
            let key = key.clone();
            bus.add_hook(
                addr,
                Box::new(move |cpu, bus| {
                    let frame = 0;
                    let r = lua.scope(|scope| {
                        let ctx = scope.create_userdata(Ctx { cpu, bus, frame })?;
                        let f: Function = lua.registry_value(&key)?;
                        f.call::<Value>(ctx)
                    });
                    match r {
                        Ok(Value::Boolean(true)) => HookAction::Redirect,
                        Ok(_) => HookAction::Continue,
                        Err(e) => {
                            log::warn!("[{id}] hook at {addr:08x}: {e}");
                            HookAction::Continue
                        }
                    }
                }),
            );
            self.installed.push(addr);
        }
    }

    pub fn on_frame(&mut self, cpu: &mut Cpu, bus: &mut Bus, frame: u64) -> Result<()> {
        let st = self.state.lock().unwrap();
        if st.frame_cbs.is_empty() {
            return Ok(());
        }
        let lua = &self.lua;
        lua.scope(|scope| {
            let ctx = scope.create_userdata(Ctx { cpu, bus, frame })?;
            for key in &st.frame_cbs {
                let f: Function = lua.registry_value(key)?;
                f.call::<()>(&ctx)?;
            }
            Ok(())
        })
        .map_err(|e| anyhow!("{e}"))
    }

    pub fn filter_input(&mut self, buttons: u16) -> Result<u16> {
        let st = self.state.lock().unwrap();
        let mut b = buttons;
        for key in &st.input_cbs {
            let f: Function = self.lua.registry_value(key).map_err(|e| anyhow!("{e}"))?;
            let r: Option<u32> = f.call(b).map_err(|e| anyhow!("{e}"))?;
            if let Some(v) = r {
                b = v as u16;
            }
        }
        Ok(b)
    }

    pub fn save_data(&mut self) {
        let r: mlua::Result<()> = (|| {
            let api: mlua::Table = self.lua.globals().get("dwrc")?;
            let save: Function = api.get("save")?;
            save.call(())
        })();
        if let Err(e) = r {
            log::warn!("[{}] saving data: {e}", self.id);
        }
        let _ = &self.data_dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_blocks_the_host() {
        let d = std::env::temp_dir().join(format!("dwrc-sandbox-{}", std::process::id()));
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::write(d.join("lib/util.lua"), "return { twice = function(x) return x * 2 end }").unwrap();
        std::fs::write(d.join("secret.txt"), "x").unwrap();
        let lua = sandboxed_lua(&d, "t").unwrap();
        let run = |code: &str| lua.load(code).exec();
        run("assert(require('lib.util').twice(21) == 42)").unwrap();
        run("assert(io == nil and debug == nil and package == nil and dofile == nil and loadfile == nil)").unwrap();
        run("assert(os.execute == nil and os.remove == nil and os.getenv == nil and type(os.time()) == 'number')").unwrap();
        run("assert(string.dump == nil)").unwrap();
        run("assert(load('return 1 + 1')() == 2)").unwrap();
        assert!(run("require('..secret')").is_err());
        assert!(run("require('missing')").is_err());
        // Binary chunks are refused even when asked for explicitly.
        run("local f, e = load('\\27Lua', 'x', 'b'); assert(f == nil)").unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
