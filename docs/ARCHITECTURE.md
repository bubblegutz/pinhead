```mermaid
flowchart TB
    subgraph Lua["Lua Script (compile time)"]
        S[Script]
    end

    subgraph Compile["compile()"]
        C[Lua::new] --> CR[Register route.register\nroute.default\nconvenience wrappers]
        CR --> CS[Execute script\nbuild route table]
        CS --> CF[Config, RouteRegistrations,\nSharedBytecodes, WorkerConfig]
    end

    subgraph Frontend["Frontends (runtime)"]
        FUSE[FUSE]
        P9S[9P Unix]
        P9T[9P TCP mux]
        P9L[9P TLS]
        SSH[SSH/SFTP]
    end

    subgraph Router["Path Router"]
        RR[router::run_router] --> MATCH[matchit trie lookup]
        MATCH --> WH[Worker Pool]
    end

    subgraph Workers["Worker Pool"]
        DISPATCH[dispatcher_loop] --> W1[Worker 1]
        DISPATCH --> W2[Worker 2]
        DISPATCH --> WN[Worker N]
    end

    subgraph Store["Store (doc.* / sql.*)"]
        LUA_CALL[Lua handler\ncalls doc.set / sql.query] --> WT[Writer task\nrusqlite]
        LUA_CALL --> RT[Reader task\nrusqlite]
    end

    subgraph Mux["9P TCP Mux (per connection)"]
        direction TB
        READER[Reader task\nreads mux frames] --> STXDISPATCH[Stream dispatcher]
        STXDISPATCH --> MUXS[MuxStream]
        MUXS --> BUNDLE["run_connection\n9P handler"]
        BUNDLE --> WRITER[Writer task\nsends frames on TCP]
    end

    S --> Compile
    CF --> Workers
    CF --> Router
    CF --> Frontend

    FUSE --> Router
    P9S --> Router
    P9T --> Router
    P9L --> Router
    SSH --> Router
```

---

## Component Details

### Path Router

```mermaid
flowchart LR
    subgraph Frontends["Frontends (all transports)"]
        FUSE
        P9SOCK
        P9TCP
        P9TLS
        SSH
    end
    subgraph Router["router::run_router (single task)"]
        RX["mpsc::Receiver<Request>"] --> MATCH["matchit::Router.at(path)"]
        MATCH --> HLOOKUP["RouteMeta.handlers.get(op)"]
        HLOOKUP --> HREQ["mpsc::Sender<HandlerRequest>"]
        HREQ --> WRX["mpsc::Receiver (worker dispatch)"]
        WRX --> ONESHOT["oneshot::Sender<Response>"]
    end
    FUSE -->|blocking_send| RX
    P9SOCK -->|send| RX
    P9TCP -->|send| RX
    P9TLS -->|send| RX
    SSH -->|send| RX
    ONESHOT -->|block_on reply_rx| FUSE
    ONESHOT -->|recv| P9SOCK
    ONESHOT -->|recv| P9TCP
    ONESHOT -->|recv| P9TLS
    ONESHOT -->|recv| SSH
```

### Worker Pool

```mermaid
flowchart TB
    subgraph Dispatcher["dispatcher_loop"]
        RX["mpsc::Receiver<HandlerRequest>"] --> ROUND["Round-robin select"]
        ROUND -->|mpsc::UnboundedSender| W1Q
        ROUND -->|mpsc::UnboundedSender| W2Q
        ROUND -->|mpsc::UnboundedSender| WNQ
    end

    subgraph W1["Worker 1 (pinned thread)"]
        W1Q["mpsc::UnboundedReceiver"] --> L1["Lua::new\nfrom_bytecodes"]
        L1 --> CALL1["call_lua(HandlerRequest)"]
        CALL1 -->|oneshot::Sender| ROUND
    end

    subgraph W2["Worker 2 (pinned thread)"]
        W2Q --> L2["Lua::new\nfrom_bytecodes"]
        L2 --> CALL2["call_lua(HandlerRequest)"]
        CALL2 --> ROUND
    end

    subgraph WN["Worker N (pinned thread)"]
        WNQ --> LN["Lua::new\nfrom_bytecodes"]
        LN --> CALLN["call_lua(HandlerRequest)"]
        CALLN --> ROUND
    end
```

### 9P TCP Mux (per connection)

```mermaid
flowchart TB
    subgraph TCP["TCP Connection"]
        S[TcpStream]
    end

    subgraph Mux["9P Mux Server"]
        S -->|tokio::io::split| READER[Reader Task]
        S -->|tokio::io::split| WRITERT[Writer Task]

        READER -->|read 8-byte frame header| PARSE["decode_mux_header"]
        PARSE -->|read payload| STXDISPATCH

        STXDISPATCH -->|stream_id exists| EXISTING["mpsc::UnboundedSender\n(per-stream channel)"]
        STXDISPATCH -->|new stream_id| NEWSTREAM["create MuxStream\nmpsc::Receiver + MuxWriter"]
        NEWSTREAM --> SPAWN["tokio::spawn\nrun_connection(MuxStream, Shared)"]

        subgraph StreamN["Per-Stream 9P Session"]
            MUXS[MuxStream] -->|read_exact| H9P[handle_message]
            H9P -->|send_reply → MuxWriter::send| WRITERT
            H9P -->|loop back| MUXS
        end

        WRITERT -->|encode_mux_frame| S
    end
```

### Store (doc.* / sql.*)

```mermaid
flowchart LR
    subgraph Lua["Lua Handler"]
        DOCSET["doc.set(handle, key, value)"]
        SQLQRY["sql.query(handle, sql, params)"]
    end

    subgraph CSP["Background Tasks"]
        WT["Writer Task\nspawn_blocking\nrusqlite Connection"]
        RT["Reader Task\nspawn_blocking\nrusqlite Connection"]
    end

    DOCSET -->|"mpsc::Sender<WriteRequest>"| WT
    WT -->|"oneshot::Sender<Result>"| DOCSET

    SQLQRY -->|"mpsc::Sender<ReadRequest>"| RT
    RT -->|"oneshot::Sender<Result>"| SQLQRY
```

### 9P TLS (per connection)

```mermaid
flowchart LR
    subgraph T["TLS Handshake"]
        S[TcpStream] -->|acceptor.accept| TLS[tokio_rustls::TlsStream]
    end
    TLS -->|run_connection| R["9P Session\n(version → attach → walk → open → read → clunk)"]
```

### 9P Unix Socket (per connection)

```mermaid
flowchart LR
    U[UnixStream] -->|run_connection| R["9P Session\n(version → attach → walk → open → read → clunk)"]
```

### SSH/SFTP (per connection)

```mermaid
flowchart TB
    subgraph Accept["TCP Listener"]
        S[TcpListener] --> ACC[accept]
    end
    ACC --> SPAWN["tokio::spawn"]
    SPAWN -->|russh handles auth| SESSION[SshSession\nper-connection Handler]
    SESSION -->|sfp subsystem| CHAN["Channel<Msg>"]
    CHAN --> SFTP["SFTP Request Loop"]
    SFTP -->|"FsOperation::Read"| REQ["mpsc::Sender<Request> → Router"]
    SFTP -->|"FsOperation::Write"| REQ
    SFTP -->|"FsOperation::ReadDir"| REQ
    SFTP -->|"FsOperation::Create"| REQ
    SFTP -->|"FsOperation::Remove"| REQ
    SFTP -->|"FsOperation::Rename"| REQ
    SFTP -->|"FsOperation::MkDir"| REQ
    SFTP -->|"FsOperation::Stat"| REQ
```

### FUSE (per mount point)

```mermaid
flowchart TB
    subgraph Init["mount()"]
        M[fuser::mount] --> FS[FuseFilesystem\nimplements Filesystem trait]
    end

    subgraph Callbacks["FUSE Callbacks (OS thread)"]
        LOOKUP["fn lookup()"] --> SEND
        GETATTR["fn getattr()"] --> SEND
        READ["fn read()"] --> SEND
        WRITE["fn write()"] --> SEND
        READDIR["fn readdir()"] --> SEND
        CREATE["fn create()"] --> SEND
        RELEASE["fn release()"] --> SEND
        SEND["send_req(FsOperation, path, data)"]
    end

    subgraph Bridge["Sync→Async Bridge"]
        SEND --> BLOCK["blocking_send(Request)"]
        BLOCK -->|mpsc::Sender| RTR["→ Router"]
        SEND --> WAIT["block_on(reply_rx)"]
        WAIT -->|oneshot::Receiver| REPLY["return response\nto FUSE kernel"]
    end
```
