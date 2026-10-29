# Getting started

This page takes you from nothing to a running server. You need a FreeAgent login (or a [FreeAgent sandbox](https://signup.sandbox.freeagent.com/signup) company to try it on), and a machine that can run Docker or a single binary.

## 1. Create a FreeAgent app

Each person signs in through FreeAgent's own OAuth, under an app you register once.

1. Open the [FreeAgent Developer Dashboard](https://dev.freeagent.com/apps) and create an app.
2. Note its **OAuth identifier** and **OAuth secret**. They become `FREEAGENT_CLIENT_ID` and `FREEAGENT_CLIENT_SECRET`.
3. Add the redirect URI `<PUBLIC_URL>/callback`, for example `https://freeagent-mcp.example.com/callback`. For local use, add `http://localhost:7771/callback`. FreeAgent matches redirect URIs exactly, port included.

[Signing in with FreeAgent](signing-in.md) explains what happens with the app during a sign-in.

## 2. Choose a secret for the server

The server needs one secret of its own besides the app secret:

- `OAUTH_SIGNING_KEY`: at least 32 bytes, used to sign the server's OAuth tokens and to encrypt the FreeAgent refresh token inside them. Generate one with `openssl rand -hex 32` and keep it stable: changing it signs everyone out.

There is no admin password and no static token. `OAUTH_ADMIN_PASSWORD` is not read, and setting `MCP_AUTH_TOKEN` stops the server at startup, because everyone signs in with their own FreeAgent account.

Store the secrets in a secret manager or an environment file readable only by you. Every variable can also be read from a file: append `_FILE` to its name, for example `FREEAGENT_CLIENT_SECRET_FILE=/run/secrets/freeagent_secret`.

## 3. Run it

### Locally, to try it

Plain HTTP on `localhost` is enough for Claude Code on the same machine, because the sign-in happens in your own browser. With the redirect URI `http://localhost:7771/callback` registered:

```bash
export FREEAGENT_CLIENT_ID=your-oauth-identifier
export FREEAGENT_CLIENT_SECRET=your-oauth-secret
PUBLIC_URL=http://localhost:7771 BIND_ADDR=127.0.0.1:7771 INSECURE_HTTP=true \
  OAUTH_SIGNING_KEY=$(openssl rand -hex 32) LOG_FORMAT=pretty \
  ./mcp-freeagent
```

From a clone of the repository, `cargo run -p mcp-freeagent` instead of `./mcp-freeagent` does the same. Add `FREEAGENT_ENVIRONMENT=sandbox` to use a sandbox company instead of real books. Then [connect Claude Code](connecting-clients.md#claude-code).

### With Docker

```bash
mkdir -p data && sudo chown 65532:65532 data
docker run -d --name mcp-freeagent -p 8443:8443 \
  -e PUBLIC_URL=https://freeagent-mcp.example.com \
  -e OAUTH_SIGNING_KEY="$(openssl rand -hex 32)" \
  -e FREEAGENT_CLIENT_ID=your-oauth-identifier \
  -e FREEAGENT_CLIENT_SECRET=your-oauth-secret \
  -e FREEAGENT_TOKEN_STORE=/data/tokens.json \
  -v "$PWD/data:/data" \
  ghcr.io/trozz/mcp-freeagent:latest
```

`PUBLIC_URL` is the address clients and FreeAgent use to reach the server. See [TLS and public access](tls.md) for how to make it reachable with a trusted certificate. `FREEAGENT_TOKEN_STORE` keeps sign-ins working across restarts; see [Tokens](signing-in.md#tokens). The server runs as UID 65532, so the directory must be writable by it.

### With Docker Compose and a Cloudflare tunnel

A tunnel gives the server a public HTTPS address with a trusted certificate, without opening a port on your network. Create a tunnel in the Cloudflare dashboard, add a public hostname such as `freeagent-mcp.example.com` pointing to `https://mcp-freeagent:8443` with **No TLS Verify** turned on (the server's own certificate is self-signed in this setup), and copy the tunnel token.

`compose.yaml`:

```yaml
services:
  mcp-freeagent:
    image: ghcr.io/trozz/mcp-freeagent:latest
    container_name: mcp-freeagent
    restart: unless-stopped
    env_file: app.env
    ports:
      - "127.0.0.1:8443:8443"
    volumes:
      - ./data:/data
      - ./audit:/audit
  tunnel:
    image: cloudflare/cloudflared:latest
    restart: unless-stopped
    command: tunnel --no-autoupdate run
    environment:
      TUNNEL_TOKEN: ${TUNNEL_TOKEN}
    depends_on:
      - mcp-freeagent
```

`app.env` (readable only by you, for example `chmod 600 app.env`):

```bash
PUBLIC_URL=https://freeagent-mcp.example.com
OAUTH_SIGNING_KEY=replace-with-the-output-of-openssl-rand-hex-32
FREEAGENT_CLIENT_ID=your-oauth-identifier
FREEAGENT_CLIENT_SECRET=your-oauth-secret
FREEAGENT_TOKEN_STORE=/data/tokens.json
AUDIT_LOG_PATH=/audit/audit.jsonl
```

The server runs as UID 65532, so both directories must be writable by it: `mkdir -p data audit && sudo chown 65532:65532 data audit`. Then start both containers with `TUNNEL_TOKEN=<token> docker compose up -d`, and register `https://freeagent-mcp.example.com/callback` with the FreeAgent app.

### With the binary

Download the archive for your platform from [Releases](https://github.com/Trozz/mcp-freeagent/releases) (Linux x86_64 and arm64, macOS arm64), verify the checksum, extract it and run:

```bash
shasum -a 256 -c mcp-freeagent-<version>-<target>.tar.gz.sha256
tar -xzf mcp-freeagent-<version>-<target>.tar.gz
cd mcp-freeagent-<version>-<target>
PUBLIC_URL=https://freeagent-mcp.example.com OAUTH_SIGNING_KEY=... \
  FREEAGENT_CLIENT_ID=... FREEAGENT_CLIENT_SECRET=... \
  ./mcp-freeagent
```

All configuration comes from environment variables; the [configuration reference](configuration.md) lists every one.

## 4. Check it is running

```bash
curl -k https://127.0.0.1:8443/healthz
```

prints `ok` (use `http://` with `INSECURE_HTTP=true`). The startup log has a `freeagent settings` line with the API address and the app's client id, a `service enabled` line with the service's URL and tools, and a `combined endpoint enabled` line for `/mcp`.

`mcp-freeagent --version` prints the version, and `mcp-freeagent --healthcheck` exits 0 when the local server answers `/healthz` (the container image uses it as its `HEALTHCHECK`).

## 5. Choose what runs

- `FREEAGENT_READ_ONLY=freeagent` (or `all`) hides and refuses every write tool. Reports, lists and `api_get` keep working. See [Security](security.md#read-only-mode).
- `FREEAGENT_COMBINED=false` turns off the combined `/mcp` endpoint.
- `FREEAGENT_SERVICES` selects services. There is one, `freeagent`, so it only matters for keeping configuration explicit.

Next: [Signing in with FreeAgent](signing-in.md).
