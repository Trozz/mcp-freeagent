# TLS and public access

claude.ai connects to your server from Anthropic's infrastructure, so for claude.ai the server must be reachable from the internet over HTTPS with a **publicly trusted** certificate. Claude Code and other clients on your own machine can use plain HTTP on `localhost`, or a certificate you trust yourself.

The person signing in also needs to reach the server from their browser: FreeAgent sends the browser back to `<PUBLIC_URL>/callback`.

## How the server picks its certificate

At startup, in this order:

1. `INSECURE_HTTP=true`: plain HTTP, no TLS. Use it on your own machine, or when TLS is terminated in front of the server on a network you trust.
2. `TLS_CERT_PATH` and `TLS_KEY_PATH` both set: the server loads them, and reloads them automatically when the files change (for example after a certificate renewal). If either file is missing or invalid the server refuses to start; it never falls back silently. Setting only one of them is an error.
3. Neither set: the server generates an in-memory self-signed certificate for the `PUBLIC_URL` host, `localhost`, `127.0.0.1` and `::1`. It is never written to disk, changes on every restart, and its SHA-256 fingerprint is logged.

The self-signed certificate is fine for encrypting the hop from a tunnel or reverse proxy that skips verification. It does not work for claude.ai directly, and a browser warns about it on the sign-in pages.

## Local use

The simplest setup for Claude Code on the same machine is plain HTTP on loopback:

```bash
PUBLIC_URL=http://localhost:7771 BIND_ADDR=127.0.0.1:7771 INSECURE_HTTP=true ./mcp-freeagent
```

`PUBLIC_URL` may be `http` only for `localhost`, `127.0.0.1` or `[::1]`. Register `http://localhost:7771/callback` as the FreeAgent app's redirect URI. See [Getting started](getting-started.md#locally-to-try-it) for the other variables.

## Option A: a Cloudflare tunnel

The simplest way to get a trusted public address. The tunnel's edge certificate is trusted by claude.ai and browsers, and the server's self-signed certificate protects the hop from the tunnel to the server. See the Docker Compose example in [Getting started](getting-started.md#with-docker-compose-and-a-cloudflare-tunnel). In the tunnel's public hostname settings, point the service to `https://mcp-freeagent:8443` and turn on **No TLS Verify**.

Set `PUBLIC_URL` to the tunnel hostname, for example `https://freeagent-mcp.example.com`.

## Option B: a reverse proxy

Run the server behind nginx, Caddy or Traefik with a trusted certificate at the proxy:

- Proxy to `https://<server>:8443` (verification off, or pinned to the logged fingerprint), or set `INSECURE_HTTP=true` if the proxy and server share a trusted network.
- Pass the `Host` header through unchanged and do not buffer responses (MCP uses streaming HTTP responses).
- Set `TRUSTED_PROXIES` to the proxy's IP address so the server uses the client address from `X-Forwarded-For` for sign-in rate limiting.
- Requests to the server are limited to 1 MiB; set the proxy's limit to the same, for example nginx's `client_max_body_size 1m`.

## Option C: the server holds the certificate

Give the server a certificate from Let's Encrypt or another public CA:

```bash
docker run -d --name mcp-freeagent -p 443:8443 \
  -v /etc/letsencrypt/live/freeagent-mcp.example.com:/certs:ro \
  -e TLS_CERT_PATH=/certs/fullchain.pem \
  -e TLS_KEY_PATH=/certs/privkey.pem \
  --env-file app.env \
  ghcr.io/trozz/mcp-freeagent:latest
```

Renewals are picked up without a restart. With certificate files, the server also sends `Strict-Transport-Security`.

## A trusted local certificate

For HTTPS on your own machine, for example to test the same `PUBLIC_URL` scheme you deploy with.

With mkcert:

```bash
brew install mkcert
mkcert -install
mkcert -cert-file cert.pem -key-file key.pem localhost 127.0.0.1 ::1
TLS_CERT_PATH=$PWD/cert.pem TLS_KEY_PATH=$PWD/key.pem ./mcp-freeagent
```

With openssl:

```bash
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 825 \
  -keyout key.pem -out cert.pem -subj "/CN=freeagent-mcp.example.com" \
  -addext "subjectAltName=DNS:freeagent-mcp.example.com,DNS:localhost,IP:127.0.0.1"
```

Add `cert.pem` to the trust store of the machine running the client and the browser, or to `NODE_EXTRA_CA_CERTS` for Node based clients.
