# Security

mcp-freeagent gives MCP clients access to a company's books, and is reachable from the internet. This page explains how access is controlled and what the server records.

## Secrets

The server holds two secrets: `OAUTH_SIGNING_KEY` and the FreeAgent app's `FREEAGENT_CLIENT_SECRET`.

- Keep both in a secret store, and read them from files with the `_FILE` variables (`OAUTH_SIGNING_KEY_FILE`, `FREEAGENT_CLIENT_SECRET_FILE`). Never commit them.
- Whoever has `OAUTH_SIGNING_KEY` can mint tokens and open the FreeAgent refresh tokens sealed inside them. Rotating it signs everyone out.
- The app secret alone gives no access to anyone's books: a sign-in also needs that person to log in at FreeAgent and approve the app. FreeAgent lets an app have two secrets at once, so a leaked secret can be replaced without downtime.

The server keeps no FreeAgent credential of its own and no database. Each person's refresh token lives sealed in their own MCP tokens, plus, when [`FREEAGENT_TOKEN_STORE`](signing-in.md#rotating-refresh-tokens-and-the-token-store) is set, an encrypted copy of a replaced one that only their tokens can open.

## Sign-in

The server is its own OAuth 2.1 authorization server for MCP clients, and signs people in through FreeAgent; [Signing in with FreeAgent](signing-in.md) has the whole flow.

- `/freeagent` and `/mcp` are separate OAuth protected resources (RFC 9728), with metadata at `/.well-known/oauth-protected-resource/<endpoint>`. Tokens are bound to the endpoint they were issued for (RFC 8707).
- Every MCP client is shown on the consent page before the browser goes to FreeAgent, because all clients reach FreeAgent under this server's one app.
- The pending sign-in travels in an encrypted, `HttpOnly`, `SameSite=Lax` cookie bound to a random `state`, so FreeAgent's code is only accepted from the browser that started the sign-in.
- MCP access tokens last 1 hour, refresh tokens 30 days, and a grant at most 90 days. There is no per-client revocation: revoke the app's access in FreeAgent to cut one person off, or rotate `OAUTH_SIGNING_KEY` to sign everyone out.
- Clients cannot skip OAuth: there is no static token (`MCP_AUTH_TOKEN` is refused at startup), and a FreeAgent token sent as a bearer token is not accepted.
- MCP sessions are bound to the sign-in that opened them, so nobody can use another person's session, even knowing its id.

## Sign-in limits

- At most 5 failed attempts per minute per client address (an IPv4 address or IPv6 /64), and 25 per network (an IPv4 /24 or IPv6 /48).
- Once 30 sign-ins have been rejected across all addresses in a minute, only addresses with a rejection of their own are refused; everyone else signs in as usual.
- A code FreeAgent refuses counts as a failed attempt; FreeAgent being unreachable does not.
- Tokens already issued keep working throughout.
- The client address is the TCP peer. Behind a reverse proxy, set `TRUSTED_PROXIES` to the proxy addresses so the client address from `X-Forwarded-For` is used; otherwise every request shares the proxy's address.

## Read-only mode

`FREEAGENT_READ_ONLY=freeagent` (or `all`) makes the FreeAgent service read-only on this server:

- Clients see only the tools marked read-only: the lists, reports, `get_` tools and `api_get`.
- Any other tool call returns `read_only: the freeagent service is read-only on this server`, and is audited as `refused`.
- Prompts stay listed; a prompt step that needs a hidden tool tells the model to say so and skip it.
- It applies on `/freeagent` and under `/mcp`.

Read-only mode limits what the server lets clients do. What a person can see is still decided by their FreeAgent access level.

## Writes need agreement

Write tools carry MCP annotations, so clients ask before running them, and deletes and other hard-to-undo changes are marked destructive (see [Reads and writes](tools/overview.md#reads-and-writes)). The server's instructions and every prompt also tell the model to show the person what will change and wait for agreement.

## Text in the books

Contact names, descriptions, bank references and other text in the books are written by customers, suppliers and banks. The server's instructions and every prompt tell the model to treat them as data, not instructions. Prompt arguments are fenced in their own tags and cannot close them.

## Audit log

Every call of a tool that is not read-only, including refused ones, produces one audit line:

```json
{"ts": "2026-10-18T10:00:00Z", "service": "freeagent", "endpoint": "/freeagent", "tool": "create_invoice", "outcome": "ok", "error_kind": null, "caller": "oauth:<client_id>", "subject": "freeagent:example-ltd:user:7", "elapsed_ms": 412}
```

- `outcome` is `ok`, `error` or `refused` (a read-only service); `error_kind` is the error kind, such as `validation` (`read_only` for a refusal), or `null` on success.
- `subject` is the FreeAgent user who signed in, as `freeagent:<company subdomain>:user:<id>`.
- Arguments, ids and results are never recorded.
- Lines go to the `audit` log target, which stays on at `info` whatever `LOG_LEVEL` says, and are appended to `AUDIT_LOG_PATH` when it is set (the file is created with mode 0600).
- If the file cannot be written, the server logs a warning at most once a minute and the tool call still succeeds.
- In Docker, mount a volume for the file, for example `./audit:/audit` with `AUDIT_LOG_PATH=/audit/audit.jsonl`, so it survives container restarts.

## What is never logged

- The app secret, refresh and access tokens, MCP tokens and the `Authorization` header.
- The books' content: invoice lines, contact details, transaction descriptions, tool results and FreeAgent's response bodies.

Tool error messages, which can quote ids and values, are logged only at debug level. The exception is `DEBUG_UNSAFE_LOG_CONTENT=true`, which writes tool results and FreeAgent response bodies into the logs for debugging and prints a warning at startup. Credentials are never logged, even then.

## Talking to FreeAgent

- Every request uses TLS with certificate verification, to the configured API host only. `FREEAGENT_API_URL` must be `https`, except a loopback address for tests.
- Redirects are never followed, and `api_get` and `api_request` refuse URLs on any other host, so a person's access token never leaves for another address.
- Revoking the app's access in FreeAgent, or deleting the app in the Developer Dashboard, cuts the server off.

## The container

The image is `FROM scratch`: the static binary and CA certificates only, running as UID 65532.
