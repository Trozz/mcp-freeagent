# Architecture

## Request flow

```mermaid
flowchart LR
    C1["MCP client<br/>/freeagent"] --> T
    C2["MCP client<br/>/mcp"] --> T
    B["Browser<br/>/authorize, /callback"] --> T
    T["HTTPS<br/>tunnel, proxy or direct"] --> S

    subgraph S["mcp-freeagent binary (:8443)"]
        direction TB
        P["Public routes<br/>/healthz, /favicon.ico<br/>/.well-known/*, /register<br/>/authorize, /callback, /token"]
        G["Bearer check per endpoint<br/>token audience, sealed refresh token"]
        W["Policy wrapper<br/>read-only and audit"]
        F["FreeAgent service<br/>tools and prompts"]
        X["Combined /mcp<br/>prefixed names"]
        K["Token cache<br/>access tokens per person"]
        A["API client<br/>retries, status mapping"]
        G --> W
        W --> F
        W --> X
        X --> F
        F --> A
        A --> K
    end

    K -->|"token_endpoint"| FA["api.freeagent.com/v2"]
    A -->|"API v2"| FA
    P -.->|"approve_app redirect"| FA
```

## Crates

| Crate | Role |
|---|---|
| `mcp-server-core` (from [mcp-server-core-rs](https://github.com/Trozz/mcp-server-core-rs)) | Configuration reading, the OAuth authorization server and upstream sign-in, bearer guards, TLS, logging, metrics, the read-only and audit wrapper, the combined endpoint, and the `Service` trait |
| `mcp-freeagent-books` | The `freeagent` service: settings (`config.rs`), FreeAgent OAuth, access tokens and the token store (`oauth.rs`), sign-in through FreeAgent (`verifier.rs`), the API client (`api.rs`), the tools (`tools/`), output shaping (`view.rs`) and the prompts (`prompts.rs`) |
| `mcp-freeagent` | The binary: registers the service, sets the `FREEAGENT` variable prefix and the sign-in mode, and runs the server |

`mcp-server-core` is taken from git at the commit that adds upstream OAuth sign-in, until that is released to crates.io.

## A tool call, end to end

```mermaid
sequenceDiagram
    participant C as Client
    participant S as mcp-freeagent /freeagent
    participant F as FreeAgent
    C->>S: tools/call list_invoices (Bearer MCP token)
    S->>S: verify token, open the sealed refresh token
    alt no cached access token, or it expires within 2 minutes
        S->>F: POST token_endpoint (refresh_token)
        F-->>S: access token, maybe a new refresh token
        S->>S: keep the new refresh token (memory and token store)
    end
    S->>F: GET /v2/invoices?view=...&page=1&per_page=25
    F-->>S: invoices, X-Total-Count, Link
    S->>S: unwrap, drop empty values, add ids
    S-->>C: {count, total, page, next_page, items}
```

## Design choices

- **No database.** FreeAgent is the only store. The server keeps access tokens in memory, and the token store file holds only replaced refresh tokens, encrypted per person.
- **People sign in as themselves.** The server holds an OAuth app, not a credential. Every call runs with the signed-in person's FreeAgent token, so FreeAgent's own access levels decide what they can do.
- **Writes are never retried** once they reach FreeAgent, so a slow create cannot happen twice. Reads are retried once.
- **Compact answers.** FreeAgent's JSON is unwrapped, empty values are dropped and every `url` gets an `id` beside it, so models spend fewer tokens and can pass ids back directly.
- **Safe defaults.** Write tools are annotated, prompts wait for agreement, and the whole service can be made read-only.
