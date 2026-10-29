# Connecting clients

The FreeAgent tools are at `/freeagent`, and `/mcp` offers every enabled service together. Add one connector, for either:

| Endpoint | Connector URL | Tool names |
|---|---|---|
| FreeAgent | `https://freeagent-mcp.example.com/freeagent` | `list_invoices` |
| All services | `https://freeagent-mcp.example.com/mcp` | `freeagent_list_invoices` |

Use one or the other, not both: with both, every tool appears twice.

Every client signs in through FreeAgent with OAuth. There is no static token, and a client cannot send a FreeAgent token as its bearer token instead.

## claude.ai, Claude Desktop and Claude mobile

These connect from Anthropic's servers, so the server must be reachable from the internet with a publicly trusted certificate (see [TLS and public access](tls.md)).

1. In claude.ai open **Settings**, then **Connectors**, then **Add custom connector**.
2. Enter the connector URL, for example `https://freeagent-mcp.example.com/freeagent`.
3. The server's consent page opens and names the client. Continue to FreeAgent, log in, and approve the app.

The connector then signs in with OAuth on its own. Its access tokens last one hour and its refresh tokens 30 days, and the client refreshes them itself; after 90 days you sign in again. Connectors added in claude.ai are also available in Claude Desktop and the Claude mobile apps.

## Claude Code

```bash
claude mcp add --transport http freeagent https://freeagent-mcp.example.com/freeagent
```

Then run `/mcp` in Claude Code, choose `freeagent` and authenticate. Your browser shows the server's consent page, then FreeAgent's login. For a server on your own machine, use `http://localhost:7771/freeagent` (see [Getting started](getting-started.md#locally-to-try-it)).

The [Claude Code plugin](claude-code-plugin.md) sets up the same connection and adds the prompts as slash commands.

## Other MCP clients

Any client that speaks the MCP Streamable HTTP transport and completes the OAuth flow (dynamic client registration, authorization code with PKCE) works. A request without a token gets a `401` whose `WWW-Authenticate` header points to `/.well-known/oauth-protected-resource/freeagent` (or `/mcp`), where discovery starts. The client must open the authorization URL in a browser, since the person signs in at FreeAgent.

## The combined endpoint

`/mcp` serves every enabled service's tools and prompts as `<service>_<name>`. mcp-freeagent has one service, so `/mcp` offers the same 79 tools and 13 prompts as `/freeagent`, prefixed `freeagent_`, and the prompts' text names the prefixed tools.

- It is its own OAuth protected resource, with metadata at `/.well-known/oauth-protected-resource/mcp`. A token issued for `/mcp` works only there, and a token for `/freeagent` does not work on `/mcp`.
- `FREEAGENT_READ_ONLY` applies there too: the write tools are hidden and refused.
- Audit lines name the real service and tool, with `"endpoint": "/mcp"`.
- `FREEAGENT_COMBINED=false` turns it off; `/mcp` then returns 404.

`/freeagent` keeps tool names short. `/mcp` is useful when you want the `freeagent_` prefix to tell these tools apart from other connectors' tools of the same name.

## Icons

The endpoint sends its own icon in `serverInfo`, and `/favicon.ico` serves the project icon. Some clients, including claude.ai at the time of writing, show a generic icon for custom connectors regardless.
