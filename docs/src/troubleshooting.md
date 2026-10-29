# Troubleshooting

## The server does not start

The server lists every configuration problem at startup, each on a `configuration error:` line, and exits. Common ones:

| Message | Fix |
|---|---|
| `PUBLIC_URL must be an https URL (http is only allowed for localhost)` | Set `PUBLIC_URL` to the external `https` address, without a path |
| `OAUTH_SIGNING_KEY must be at least 32 bytes` | Use `openssl rand -hex 32` |
| `FREEAGENT_CLIENT_ID is required`, `FREEAGENT_CLIENT_SECRET is required` | Set the app's OAuth identifier and secret from the [FreeAgent Developer Dashboard](https://dev.freeagent.com/apps) |
| `FREEAGENT_CLIENT_ID must be the OAuth identifier from the FreeAgent Developer Dashboard (letters, digits, - and _)` | Copy the OAuth identifier, not the app's name |
| `FREEAGENT_ENVIRONMENT must be production or sandbox, got "..."` | Use `production` or `sandbox` |
| `FREEAGENT_API_URL must use https (http is only allowed for loopback addresses)` | Leave it unset unless you test against a local fake |
| `FREEAGENT_API_VERSION must be a date as YYYY-MM-DD, such as 2026-09-01` | Use a date, or leave it unset |
| `MCP_AUTH_TOKEN cannot be used when people sign in with their own FreeAgent account` | Remove `MCP_AUTH_TOKEN`; every client signs in with OAuth |
| `FREEAGENT_TOKEN_STORE: cannot read ...` or `... is not a FreeAgent token store: ...` | The file exists but is unreadable by UID 65532, or is something else. Point it at a new path; the file is created on the first write |
| `..._FILE: cannot read ...` | The secret file is missing or unreadable by UID 65532 in the container |

## FreeAgent shows an error instead of its login

- **The redirect URI does not match.** Register exactly `<PUBLIC_URL>/callback` with the app, scheme and port included, for example `http://localhost:7771/callback` for local use. `PUBLIC_URL` must be exactly the address the server is reached at.
- **Wrong environment.** A sandbox company needs `FREEAGENT_ENVIRONMENT=sandbox`, and the live one `production` (the default).

## The sign-in page reports a problem

| The page says | Cause and fix |
|---|---|
| `FreeAgent refused this server's app credentials; ask the server's operator to check them` | `FREEAGENT_CLIENT_ID` or `FREEAGENT_CLIENT_SECRET` is wrong, or the secret was replaced. The log has `FreeAgent refused this server's OAuth app: check FREEAGENT_CLIENT_ID and FREEAGENT_CLIENT_SECRET` |
| `FreeAgent is limiting requests; try again in a minute` | FreeAgent's rate limit. Wait a minute |
| `FreeAgent granted no refresh token` | FreeAgent's answer had no refresh token, so the sign-in cannot last. Try again; report it if it repeats |
| An error page after coming back from FreeAgent | The sign-in took more than 10 minutes, or was finished in another browser than it started in. Start the connection again from the client |
| `The login form expired or was modified. Start the connection again.` | The consent page was open for more than 10 minutes. Start again |
| `Too many attempts from this address ...` | Too many failed sign-ins from this address. Wait a minute. Behind a proxy, check `TRUSTED_PROXIES` |

## claude.ai cannot connect

- The server must be reachable from the internet with a publicly trusted certificate. A self-signed certificate works only behind a tunnel or proxy; see [TLS and public access](tls.md).
- With a Cloudflare tunnel, the public hostname must point to `https://mcp-freeagent:8443` with **No TLS Verify** on.
- `PUBLIC_URL` must be exactly the address claude.ai uses. OAuth metadata, token audiences and the FreeAgent redirect URI are built from it.

## A connector stops working

- Tools fail with `auth: FreeAgent no longer accepts this connection's sign-in ...`: FreeAgent refused the refresh token. It was revoked, expired, or replaced by FreeAgent and then lost in a restart without `FREEAGENT_TOKEN_STORE`. Reconnect the connector, or run `/mcp` in Claude Code.
- The client gets 401 everywhere: `OAUTH_SIGNING_KEY` or `PUBLIC_URL` changed, which signs every client out. Remove the connector and add it again.
- Everyone had to sign in again after a configuration change: changing `FREEAGENT_ENVIRONMENT`, `FREEAGENT_API_URL` or `FREEAGENT_CLIENT_ID` ends every sign-in.
- 90 days after signing in, every grant ends and the person signs in again.
- A token for `/freeagent` is not accepted on `/mcp`, and the other way round. Make sure the connector URL is the endpoint it signed in to.

## Tool errors

Tool errors start with a kind, such as `permission: ...`; [Working with the tools](tools/overview.md#errors) lists them. The common ones:

| Error | What to do |
|---|---|
| `permission: FreeAgent refused ... (HTTP 403) ...` | The signed-in user's access level does not allow it, or the feature does not apply to this company. `get_current_user` shows the level |
| `rate_limited: the FreeAgent API rate limit ... is used up; try again in N seconds` | Wait as long as it says. The limit is per FreeAgent user |
| `validation: FreeAgent rejected ...` | FreeAgent refused the request; the rest of the message is FreeAgent's own reason |
| `internal: FreeAgent refused this server's OAuth app credentials ...` | The operator needs to check `FREEAGENT_CLIENT_ID` and `FREEAGENT_CLIENT_SECRET` |
| `upstream: ... returned invalid JSON ...; check FREEAGENT_API_URL points at the FreeAgent API` | `FREEAGENT_API_URL` points somewhere else |
| `read_only: the freeagent service is read-only on this server` | `FREEAGENT_READ_ONLY` is set |

## Warnings in the log

- `FreeAgent refused this app's credentials as HTTP Basic but accepted them in the request body; sending them in the body from now on`: harmless; the server adapted. If FreeAgent refuses them both ways, check the app's id and secret.
- `writing the FreeAgent token store failed`: the token store file or its directory is not writable by the server. Replaced refresh tokens are then kept only in memory.
- `could not write the audit log`: `AUDIT_LOG_PATH` is not writable. Tool calls still succeed.

## More detail

Set `LOG_LEVEL=info,mcp_freeagent=debug` or `DEBUG=true` and reproduce the problem. These log each FreeAgent request's method, path and status, and the full text of tool errors, but not response bodies. When reporting an issue, include the version (`mcp-freeagent --version`) and the log lines, and check that they contain nothing private.
