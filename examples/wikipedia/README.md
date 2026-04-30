# Wikipedia Example for pinhead

A Wikipedia client as a virtual filesystem. Search articles, browse
content with lazy-loaded links, and save bookmarks in a SQLite
document store -- all through standard filesystem operations.

## Quick Start

```bash
# Build pinhead
cargo build --release

# Run the example (listens on /tmp/wikipedia.sock by default)
./target/release/pinhead examples/wikipedia/main.lua &

# Search Wikipedia
echo "american folk music" > /tmp/wikipedia.sock/search

# List results
ls /tmp/wikipedia.sock/result/

# Read an article
cat /tmp/wikipedia.sock/result/woody_guthrie.md
```

Or override the address:

```bash
PINHEAD_LISTEN="sock:/tmp/custom.sock" ./target/release/pinhead examples/wikipedia/main.lua
```

To use FUSE:

```bash
PINHEAD_FUSE_MOUNT=/tmp/wikipedia ./target/release/pinhead examples/wikipedia/main.lua
```

## Filesystem Layout

```
/search             (write)   — submit a Wikipedia search
/result/            (readdir) — list search results
/result/{title}.md  (read)    — read an article in markdown
/article/{title}/links/        (readdir) — list article links
/article/{title}/links/{id}    (read)    — read a linked article
/bookmarks/         (readdir) — list bookmark categories
/bookmarks/{path}   (read)    — read a bookmark
/bookmarks/{path}   (write)   — save a bookmark
/bookmarks/{path}   (create)  — create a bookmark directory
/bookmarks/{path}   (unlink)  — remove a bookmark or directory
/README.md          (read)    — this documentation
```

## Features

- Real Wikipedia API integration via `req.get()`
- Lazy link loading — linked articles are fetched on demand
- Persistent bookmarks via `doc.*` / `sql.*` (SQLite)
- Demonstrates: read, write, readdir, create, unlink operations
