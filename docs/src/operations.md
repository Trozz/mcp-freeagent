# Operations

## Health

- `GET /healthz` returns `ok` without authentication. It does not contact FreeAgent, so a FreeAgent outage does not get the container restarted.
- `mcp-freeagent --healthcheck` connects to `127.0.0.1` on the `BIND_ADDR` port and exits 0 when `/healthz` answers. The container image uses it as its `HEALTHCHECK`, so `docker ps` shows `healthy`.

## Logs

- Logs go to stdout, as JSON by default (`LOG_FORMAT=pretty` for humans).
- Every tool call logs a `tool call` line with the tool, outcome and duration, and a failed one a `tool failed` warning with its error kind. Startup logs `freeagent settings` (API address and the app's client id), `service enabled` and `combined endpoint enabled`.
- A sign-in logs `signed in to FreeAgent` with the person's subject.
- `LOG_LEVEL` takes `tracing` filters, for example `info,mcp_freeagent=debug`. Avoid `debug` or `trace` for everything (`LOG_LEVEL=debug`): that also turns on the HTTP libraries' own debug output, which this server does not cap. Raise only the `mcp_freeagent` target, or use `DEBUG=true`.
- `DEBUG=true` adds each FreeAgent request's method, path, status and duration, retries, token requests and tool error messages, without response bodies.
- `DEBUG_UNSAFE_LOG_CONTENT=true` adds tool results and FreeAgent response bodies. Use it only briefly; see [Security](security.md#what-is-never-logged).

## Metrics

`METRICS_ADDR` (for example `0.0.0.0:9090`) starts a separate plain-HTTP Prometheus listener. Keep it on an internal network. Metrics:

| Metric | What it counts |
|---|---|
| `mcp_tool_calls_total`, `mcp_tool_duration_seconds` | Tool calls by tool and outcome, and their duration |
| `freeagent_requests_total`, `freeagent_request_duration_seconds` | FreeAgent API requests by endpoint label and status class, and their duration |
| `freeagent_rate_limited_total` | API requests FreeAgent refused with 429 |
| `freeagent_token_requests_total` | Requests to FreeAgent's token endpoint by grant (`authorization_code` or `refresh_token`) and HTTP status |
| `freeagent_refresh_token_rotations_total` | Refreshes where FreeAgent handed out a new refresh token |
| `freeagent_token_store_errors_total` | Failed writes of `FREEAGENT_TOKEN_STORE` |
| `oauth_requests_total`, `oauth_login_failures_total` | Requests to the server's own OAuth endpoints, and failed sign-ins |
| `http_requests_total` | HTTP requests by route and status |

`freeagent_rate_limited_total` rising means people are reaching FreeAgent's limit of 120 requests a minute and 3600 an hour per user. `freeagent_token_store_errors_total` above zero means replaced refresh tokens may not survive a restart: check the file's directory is writable by the server.

## Traces

`OTEL_EXPORTER_OTLP_ENDPOINT`, with the standard `OTEL_EXPORTER_OTLP_HEADERS` when your collector needs them, exports traces over OTLP/HTTP. Nothing is exported unless it is set.

## Audit file

With `AUDIT_LOG_PATH`, the audit file only grows: one short line per write. Rotate it with a tool that truncates in place, such as logrotate with `copytruncate`. See [Security](security.md#audit-log) for the line format.

## Upgrading

- Docker: pull the new image and recreate the container (`docker compose pull && docker compose up -d`). Keep the `FREEAGENT_TOKEN_STORE` volume.
- Binary: replace the file and restart.
- Connectors stay signed in across upgrades and restarts as long as `OAUTH_SIGNING_KEY`, `PUBLIC_URL`, `FREEAGENT_CLIENT_ID` and the FreeAgent environment stay the same, and the token store is kept.
- Release notes list every change; see [Releases](releases.md).

## Backups

The books live in FreeAgent; the server keeps only caches in memory. Keep your configuration and secrets somewhere safe, and keep the `FREEAGENT_TOKEN_STORE` file on a persistent volume. Losing it does not lose data: at worst, people whose refresh token FreeAgent replaced sign in again.

## Scaling

Run one replica. Access tokens, sign-in limits and MCP sessions live in memory per replica, and the token store is one file per replica. If you need more than one, send each person to the same replica (sticky sessions), and give each replica its own token store file.

## Resource use

Memory holds each signed-in person's current FreeAgent access token (at most 10,000 sign-ins at once; the least recently used are dropped first and simply refreshed again). The container needs no writable filesystem apart from the token store and audit volumes.
