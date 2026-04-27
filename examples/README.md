# pinhead Examples

This directory contains example Lua scripts demonstrating pinhead's filesystem
routing, serialization, HTTP client, database, and authentication features.
Each example lives in its own subdirectory with `main.lua` as the entry point.

## Quick Start

```bash
# Build pinhead
cargo build

# Run any example
cargo run -- examples/simpler/main.lua
cargo run -- examples/serialization/main.lua

# Explore the mounted filesystem (via 9P at /tmp/pinhead-<name>.sock)
cat /tmp/pinhead-simpler.sock   # not a real file — use a 9P client
# Or run with FUSE:
PINHEAD_FUSE_MOUNT=/tmp/mnt cargo run -- examples/simpler/main.lua
ls /tmp/mnt
cat /tmp/mnt/hello.txt
```

## Examples

### Core File Operations (`simpler/`)

`examples/simpler/main.lua`

Basic routing — readdir, read, write, create with path parameters via `{wildcard}`.

### Minimal Filesystem (`basic/`)

`examples/basic/main.lua`

Minimal route setup with `route.register()` — registers individual ops per path.

### Multi-file Module (`modular/`)

`examples/modular/main.lua` + `config.lua` + `routes.lua` + `utils.lua`

Multi-file script via `dofile()`. Demonstrates structured application layout.

### Comprehensive Handler (`handler/`)

`examples/handler/main.lua`

Full-featured demo with path params (`/users/{id}/profile`), read/lookup/getattr/open,
default fallback, and all three frontends (9P, SSH, FUSE).

### JSON / YAML / TOML / CSV Serialization (`serialization/`)

`examples/serialization/main.lua`

`json.enc()`, `json.dec()`, `json.q()`, `json.jq()`, `yaml.*`, `toml.*`, `csv.*` encode/decode/query.

### JSON ↔ YAML Conversion (`conversion/`)

`examples/conversion/main.lua`

Bidirectional JSON ↔ YAML conversion with auto-detection via `json.from_yaml()` / `yaml.from_json()`.

### HTTP Client (`http/`)

`examples/http/main.lua`

`req.get()`, `req.post()`, `req.json()`, `req.form()` — HTTP requests with bearer auth, query params,
JSON body, form body, and response decoding.

### Simple HTTP Test (`simple-http/`)

`examples/simple-http/main.lua`

Minimal HTTP client test — single GET request to an external API.

### Document Store (`doc/`)

`examples/doc/main.lua`

`doc.open()`, `doc.set()`, `doc.get()`, `doc.find()`, `doc.all()` — SQLite-backed document store
with JSON path queries.

### SQL Database (`sql/`)

`examples/sql/main.lua`

`sql.open()`, `sql.exec()`, `sql.query()`, `sql.row()` — raw SQL access via SQLite.

### Environment Variables (`env/`)

`examples/env/main.lua`

`env.get()`, `env.set()`, `env.unset()`, and table-style `env.KEY` access.

### Real Filesystem Access (`fs-ops/`)

`examples/fs_ops/main.lua`

`fs.read()`, `fs.write()`, `fs.ls()`, `fs.stat()`, `fs.exists()`, `fs.mkdir()`, `fs.remove()`, `fs.copy()`, `fs.rename()`.

### Real Filesystem Mirror (`fs-mirror/`)

`examples/fs_mirror/main.lua`

Mirrors a real directory tree as a virtual filesystem — reads actual files/dirs through pinhead's
route handlers.

### Comprehensive Demo (`comprehensive/`)

`examples/comprehensive/main.lua`

**All API surfaces in one virtual filesystem.** Demonstrates env, log, json, yaml,
toml, csv (encode/decode/query/jq), doc.* (open/set/get/find), sql.* (open/exec/
query/row), req.* (HTTP GET with decode, pcall error handling), route bundles
(route.read, route.readdir), route.default fallback, and SSH/9P/FUSE frontend
configuration with env-var-driven overrides.

## Route Definition API

Routes are registered via the global `route` object:

| Method | Default | Description |
|---|---|---|
| `route.register(path, ops, func)` | — | Register handler for specific ops (string, table, or `true` for all) |
| `route.read(path, func)` | `route.read.default(func)` | Bundle: lookup, getattr, open, read, release, flush |
| `route.write(path, func)` | `route.write.default(func)` | Bundle: lookup, getattr, open, read, write, release, flush, fsync |
| `route.readdir(path, func)` | `route.readdir.default(func)` | Bundle: lookup, getattr, opendir, readdir, releasedir |
| `route.create(path, func)` | `route.create.default(func)` | Bundle: lookup, getattr, create, open, read, write, release, flush |
| `route.unlink(path, func)` | `route.unlink.default(func)` | Bundle: unlink, lookup, getattr |
| `route.mkdir(path, func)` | `route.mkdir.default(func)` | Bundle: mkdir, lookup, getattr, opendir, readdir, releasedir |
| `route.lookup(path, func)` | `route.lookup.default(func)` | Single op: lookup |
| `route.getattr(path, func)` | `route.getattr.default(func)` | Single op: getattr |
| `route.open(path, func)` | `route.open.default(func)` | Single op: open |
| `route.release(path, func)` | `route.release.default(func)` | Single op: release |
| `route.all(path, func)` | `route.all.default(func)` | All 25 FUSE ops |
| `route.default(func)` | — | Catch-all for unregistered paths (no specific op binding) |

Each `.default` form registers the handler at `/{*path}` (catch-all path) for the same operation set as the
named bundle, making it a fallback for that specific operation type.

**Handler signature**: `function(params, data)` — `params` is a table of path parameters
(empty `{}` if none), `data` is a string or nil (write payload). Returns a string.

**Path patterns** use matchit syntax: `/users/{id}`, `/files/{*path}`.

## Built-in Globals

| Module | Description |
|---|---|
| `json.*` | JSON encode (`enc`), decode (`dec`), pretty-print (`enc_pretty`), path query (`q`), jq filter (`jq`), YAML conversion (`from_yaml`) |
| `yaml.*` | YAML encode/decode/query/JSON conversion |
| `toml.*` | TOML encode/decode/query |
| `csv.*` | CSV encode/decode/filter query |
| `req.*` | HTTP client — `get()`, `post()`, `put()`, `delete()`, `patch()`, `head()`, `options()` |
| `log.*` | `print(msg)` and `debug(msg)` to stderr |
| `env.*` | `get(key)`, `set(key, val)`, `unset(key)`, table-style access |
| `doc.*` | SQLite document store — `open`, `close`, `set`, `get`, `delete`, `find`, `all`, `count` |
| `sql.*` | Raw SQL — `open`, `close`, `exec`, `query`, `row` |
| `fs.*` | Real filesystem — `read`, `write`, `ls`, `stat`, `exists`, `mkdir`, `remove`, `copy`, `rename`, etc. |
| `oauth.*` | OAuth 2.0 client — `client(config)` |
| `fuse.*` | FUSE mount management — `mount()`, `unmount()`, `unmountall()` |
| `ninep.*` | 9P2000 listener management — `listen()`, `kill()`, `killall()` |
| `sshfs.*` | SSH/SFTP server — `listen()`, `kill()`, `killall()`, `password()`, `authorized_keys()`, `userpasswd()` |

## Running

```bash
# Run with a specific script
cargo run -- examples/handler/main.lua

# Run with FUSE mount
PINHEAD_FUSE_MOUNT=/tmp/mnt cargo run -- examples/handler/main.lua

# Scripts listen on 9P by default — override via env:
PINHEAD_LISTEN="sock:/tmp/custom.sock" cargo run -- examples/handler/main.lua
PINHEAD_SSH_LISTEN="127.0.0.1:2222" cargo run -- examples/handler/main.lua
```

If no script is provided, pinhead looks for `pinhead.lua` in the current directory,
then falls back to `examples/handler/main.lua`.

## Testing

Each example has a corresponding end-to-end test in `tests/` that spawns the binary,
exercises routes through the 9P frontend, and verifies output:

```bash
# Run all tests
cargo test --no-default-features

# Run a specific example's test
cargo test --test simpler_demo_e2e
```

## Troubleshooting

- **Module not found**: pinhead's Lua globals are pre-registered — no `require()` needed.
  For multi-file scripts, use `dofile("relative/path.lua")`.
- **No frontend configured**: Scripts must call `ninep.listen()` or set `PINHEAD_LISTEN`.
- **FUSE not available**: Use `cargo test --no-default-features` to skip FUSE-dependent tests.
- **Address in use**: Change listener addresses via `PINHEAD_LISTEN` / `PINHEAD_SSH_LISTEN`.
