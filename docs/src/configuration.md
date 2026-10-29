# Configuration reference

Everything is configured with environment variables. The server checks all of them at startup and refuses to start with a list of every problem it found, so a typo never leaves it half configured.

General rules:

- An empty value counts as unset.
- Every variable can also be read from a file by appending `_FILE` to its name, for example `FREEAGENT_CLIENT_SECRET_FILE=/run/secrets/freeagent_secret`. A value set directly wins over the file, and a trailing newline in the file is ignored. Use this for secrets with Docker or Kubernetes secrets.
- Booleans accept `true`, `false`, `1`, `0`, `yes`, `no`, `on` and `off`.
- Lists are comma separated; spaces around names are ignored.

## Server and HTTP

| Variable | Required | Default | Example | Description |
|---|---|---|---|---|
| `PUBLIC_URL` | yes | | `https://freeagent-mcp.example.com` | External base URL clients and FreeAgent use, without a path. Must be `https`; `http` is accepted only for `localhost`, `127.0.0.1` or `[::1]`. FreeAgent's redirect URI is this plus `/callback` |
| `BIND_ADDR` | no | `0.0.0.0:8443` | `127.0.0.1:7771` | Listen address, an IP address and port. It must accept loopback connections (`0.0.0.0` does) so `--healthcheck` can reach `127.0.0.1` |
| `TLS_CERT_PATH`, `TLS_KEY_PATH` | no | | `/certs/fullchain.pem`, `/certs/privkey.pem` | PEM certificate chain and private key. Both or neither; reloaded automatically when the files change. See [TLS and public access](tls.md) |
| `INSECURE_HTTP` | no | `false` | `true` | Serve plain HTTP. For local use, or when TLS is terminated in front of the server on a network you trust |
| `ALLOWED_ORIGINS` | no | | `http://localhost:6274` | Extra browser `Origin` values accepted on MCP requests, comma separated. The `PUBLIC_URL` origin is always allowed |
| `TRUSTED_PROXIES` | no | | `10.0.0.2,10.0.0.3` | Proxy IP addresses whose `X-Forwarded-For` is trusted for sign-in rate limiting |

## Sign-in and tokens

| Variable | Required | Default | Example | Description |
|---|---|---|---|---|
| `OAUTH_SIGNING_KEY` | yes | | output of `openssl rand -hex 32` | Key that signs the server's OAuth tokens and encrypts the FreeAgent refresh token inside them, at least 32 bytes. Changing it signs everyone out |
| `FREEAGENT_CLIENT_ID` | yes | | `abcd1234EFGH5678` | The FreeAgent app's OAuth identifier: 8 to 128 letters, digits, `-` and `_`. Changing it signs everyone out |
| `FREEAGENT_CLIENT_SECRET` | yes | | the app's OAuth secret | The FreeAgent app's OAuth secret |
| `FREEAGENT_TOKEN_STORE` | no | | `/data/tokens.json` | File that keeps each sign-in's newest refresh token, encrypted, across restarts. See [Tokens](signing-in.md#tokens) |

`OAUTH_ADMIN_PASSWORD` is not read: people sign in with FreeAgent instead. `MCP_AUTH_TOKEN` must not be set; the server refuses to start with it, because every request needs a FreeAgent sign-in.

## FreeAgent

| Variable | Required | Default | Example | Description |
|---|---|---|---|---|
| `FREEAGENT_ENVIRONMENT` | no | `production` | `sandbox` | `production` (or `live`) for the live API, `sandbox` for FreeAgent's sandbox. Anything else stops startup. Changing it signs everyone out |
| `FREEAGENT_API_URL` | no | from `FREEAGENT_ENVIRONMENT` | `http://127.0.0.1:9999/v2` | The API base, ending `/v2`. Must be `https`, or `http` to a loopback address (for tests); no credentials, query or fragment. Overrides `FREEAGENT_ENVIRONMENT`. Changing it signs everyone out |
| `FREEAGENT_API_VERSION` | no | | `2026-09-01` | An `X-Api-Version` date to send with every API request, as `YYYY-MM-DD` |

## Services

| Variable | Required | Default | Example | Description |
|---|---|---|---|---|
| `FREEAGENT_SERVICES` | no | all (`freeagent`) | `freeagent` | Services to run. There is one, `freeagent`. An unknown name or an empty list stops startup |
| `FREEAGENT_READ_ONLY` | no | none | `freeagent` or `all` | Services whose write tools are hidden and refused, on their own endpoint and under `/mcp`. An unknown name stops startup. See [Security](security.md#read-only-mode) |
| `FREEAGENT_COMBINED` | no | `true` | `false` | Serve the [combined endpoint](connecting-clients.md#the-combined-endpoint) `/mcp`. With `false`, `/mcp` returns 404 |

## Logging, metrics and audit

| Variable | Required | Default | Example | Description |
|---|---|---|---|---|
| `LOG_LEVEL` | no | `info` | `info,mcp_freeagent=debug` | Log filter in `tracing` syntax. Targets: `mcp_freeagent` (which covers `mcp_freeagent_books`) and `mcp_server_core`. The `audit` target always stays on at `info` |
| `LOG_FORMAT` | no | `json` | `pretty` | `json` or `pretty` |
| `DEBUG` | no | `false` | `true` | Debug logging: each FreeAgent request's method, path, status and duration, retries, token requests, and tool error messages (which can quote ids and values from the books). Never response bodies, tool results or credentials |
| `DEBUG_UNSAFE_LOG_CONTENT` | no | `false` | `true` | Also logs full tool results and FreeAgent response bodies, which **puts the company's books in the logs**. A warning is printed at startup. Credentials are never logged |
| `METRICS_ADDR` | no | | `0.0.0.0:9090` | Starts a separate plain-HTTP Prometheus listener. Keep it on an internal network. See [Operations](operations.md#metrics) |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | no | | `http://otel-collector:4318` | Exports traces over OTLP/HTTP. The standard `OTEL_EXPORTER_OTLP_HEADERS` is honoured. Nothing is exported unless it is set |
| `AUDIT_LOG_PATH` | no | | `/audit/audit.jsonl` | File every write tool call is appended to as one JSON line (created with mode 0600). Audit lines are always logged too. See [Security](security.md#audit-log) |

The server variables (everything not starting with `FREEAGENT_`) come from mcp-server-core; its [configuration reference](https://trozz.github.io/mcp-server-core-rs/configuration.html) gives every rule in full.

## A complete example

```bash
PUBLIC_URL=https://freeagent-mcp.example.com
OAUTH_SIGNING_KEY_FILE=/run/secrets/oauth_signing_key
FREEAGENT_CLIENT_ID=your-oauth-identifier
FREEAGENT_CLIENT_SECRET_FILE=/run/secrets/freeagent_secret
FREEAGENT_TOKEN_STORE=/data/tokens.json
FREEAGENT_READ_ONLY=freeagent
AUDIT_LOG_PATH=/audit/audit.jsonl
LOG_FORMAT=json
```
