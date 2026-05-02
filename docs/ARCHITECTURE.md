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

    subgraph Mux["9P Mux (TCP / TLS)"]
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
    FRONT["Frontends\n(FUSE / 9P sock / 9P TCP mux / 9P TLS / SSH)"] -->|Request| ROUTER
    
    subgraph ROUTER["router::run_router (single task)"]
        RX["mpsc::Receiver<Request>"] --> MATCH["matchit::Router.at(path)"]
        MATCH --> HLOOKUP["RouteMeta.handlers.get(op)"]
        HLOOKUP --> HREQ["mpsc::Sender<HandlerRequest> > Worker Pool"]
        HREQ --> ONESHOT["oneshot::Sender<Response> > Frontend"]
    end

    ONESHOT -->|Response| FRONT
```


### Worker Pool

```mermaid
flowchart TB
    subgraph Dispatcher["dispatcher_loop"]
        RX["Receiver<HandlerRequest>"] --> SELECT["Round-robin select"]
        SELECT -->|mpsc::UnboundedSender| Q1
        SELECT -->|mpsc::UnboundedSender| Q2
        SELECT -->|mpsc::UnboundedSender| QN
    end

    Q1 --> W1["Worker 1\ncall_lua"]
    Q2 --> W2["Worker 2\ncall_lua"]
    QN --> WN["Worker N\ncall_lua"]

    W1 -->|oneshot::Sender| SELECT
    W2 -->|oneshot::Sender| SELECT
    WN -->|oneshot::Sender| SELECT
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


### 9P TCP Mux (per connection)

```mermaid
flowchart TB
    subgraph Conn["TCP Connection"]
        S[TcpStream]
    end

    subgraph Mux["Mux Layer"]
        S -->|tokio::io::split| READER[Reader Task]
        S -->|tokio::io::split| WRITER[Writer Task]
        READER --> FRAMES["read mux frames"]
        FRAMES --> DISPATCH["Stream Dispatcher"]
        DISPATCH --> CHAN["MuxStream"]
        WRITER -->|encode_mux_frame| S
    end

    subgraph NineP["9P Session (same as Unix socket)"]
        CHAN --> RUN["run_connection(MuxStream)"]
        RUN --> HANDLE["handle_message"]
        HANDLE --> ROUTER["Router -> Worker -> response"]
        ROUTER -->|MuxWriter::send| WRITER
        HANDLE -->|loop| RUN
    end
```


### 9P TLS (per connection)

```mermaid
flowchart LR
    subgraph T["TLS Handshake"]
        S[TcpStream] -->|acceptor.accept| TLS[tokio_rustls::TlsStream]
    end
    TLS -->|run_connection| R["9P Session\n(version > attach > walk > open > read > clunk)"]
```


### 9P UDP (connectionless)

```mermaid
flowchart LR
    S[UdpSocket] -->|recv_from| MSG["read 9P message"]
    MSG --> H["handle_udp_message
(VirtualStream wrapper)"]
    H --> R["send response
back to peer"]
    R -->|loop| S
```

### 9P Unix Socket (per connection)

```mermaid
flowchart LR
    U[UnixStream] -->|run_connection| R["9P Session\n(version > attach > walk > open > read > clunk)"]
```


### SSH/SFTP (per connection)

```mermaid
flowchart TB
    subgraph Accept["TCP Listener"]
        S[TcpListener] --> ACC[accept]
    end
    ACC --> SP["tokio::spawn"]
    SP -->|russh handles auth| SS[SshSession]
    SS -->|SFTP subsystem| SFTP[SFTP Request Loop]
    SFTP -->|FsOperation| REQ["Router"]
    REQ -->|HandlerResponse| SFTP
    SFTP -->|SFTP protocol response| SS
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

    subgraph Bridge["Sync>Async Bridge"]
        SEND --> BLOCK["blocking_send(Request)"]
        BLOCK -->|mpsc::Sender| RTR["> Router"]
        SEND --> WAIT["block_on(reply_rx)"]
        WAIT -->|oneshot::Receiver| REPLY["return response\nto FUSE kernel"]
    end
```

