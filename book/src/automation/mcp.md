# MCP

`photocraft-automation` implements an MCP server over stdio with two backends:

- **Headless:** an in-process `Headless` session owns the engine and performs file I/O.
- **Bridge:** `BridgeClient` forwards tools to a desktop application started with `photocraft --control <port>`.

Start the headless server with:

```sh
photocraft-cli mcp --automation-read-root /work/project --automation-write-root /work/project
```

Bridge to a running desktop application with the same private token file used by the app:

```sh
photocraft-cli mcp --bridge 127.0.0.1:7878 --control-token-file /private/path/control.token
```

Tools cover session/document operations, command discovery and execution, batching, and—when bridged—UI inspection and control. Tool schemas improve correctness but are not authorization boundaries.

## Security notes

Stdio MCP does not open a network listener. Its client receives only the read and write roots
explicitly granted at launch; omitting a root omits that authority. Bridge mode authenticates its
loopback control connection with a bearer token before invoking any control method and uses the
roots configured on the desktop process.

The current server does not issue scoped sessions or distinguish trusted tools from high-impact tools. Authentication proves possession of the token; it does not restrict an authenticated client to document-only access. Do not expose either transport through an untrusted broker, shell, proxy, or remote service without adding an external policy boundary.

Filesystem capabilities are implemented. General tool and method capabilities remain
[proposed](../security/automation-security.md).
