# mcp-freeagent

A self-hosted [Model Context Protocol](https://modelcontextprotocol.io) server for [FreeAgent](https://www.freeagent.com), the UK accounting software. MCP clients such as claude.ai, Claude Desktop and Claude Code can read a company's books and keep them: contacts, projects and time, invoices, estimates and credit notes, bills, expenses and mileage, bank transactions and their explanations, reports, tax returns and payroll.

| Endpoint | What it does |
|---|---|
| `/freeagent` | 82 tools and 13 prompts for the books of the company whose FreeAgent user signed in |
| `/mcp` | Every enabled service's tools and prompts in one connector, named `<service>_<tool>`, for example `freeagent_list_invoices` |

- **People sign in with FreeAgent itself.** The sign-in page asks the person to approve the MCP client, sends them to FreeAgent to log in and approve this server's app, and keeps the refresh token FreeAgent grants only in encrypted form inside their own tokens. There are no API keys, and the server stores no credential (see [Tokens](#tokens) for the one optional file). Every call acts as that FreeAgent user, within their access level.
- **Read and write.** Lists and reports, plus tools that create and change records: raise and send invoices, explain bank transactions, approve FreeAgent's guesses, record bills and expenses, log time, post journals. `api_get` and `api_request` reach every other endpoint.
- **Prompts as slash commands.** 13 ready-made jobs, such as monthly bookkeeping, explaining bank transactions and chasing overdue invoices. Clients show them as slash commands, and the [Claude Code plugin](#claude-code-plugin) adds them as `/freeagent:...` commands.
- **Safe by default.** Write tools are annotated so clients ask before running them, and the prompts show what will change and wait for agreement. `FREEAGENT_READ_ONLY=freeagent` hides and refuses every write tool. Every write is recorded in an audit log (tool, outcome and FreeAgent user; never arguments or results).
- A single static Rust binary, also built as a `FROM scratch` container image. TLS end to end, a built-in OAuth 2.1 server for claude.ai custom connectors, and no database.

Built on [mcp-server-core](https://github.com/Trozz/mcp-server-core-rs), the same base as [mcp-companies-house](https://github.com/Trozz/mcp-companies-house), [mcp-twenty-crm](https://github.com/Trozz/mcp-twenty-crm) and [mcp-icloud](https://github.com/Trozz/mcp-icloud). Signing in through FreeAgent uses the core's upstream OAuth sign-in (`mcp_server_core::upstream`).

## Quick start

1. **Create a FreeAgent app** in the [FreeAgent Developer Dashboard](https://dev.freeagent.com/apps). Note its **OAuth identifier** and **OAuth secret**, and add the redirect URI `https://<your server>/callback`; for local use, `http://localhost:7771/callback`. FreeAgent matches redirect URIs exactly, port included.
2. **Run the server locally** (no TLS, on the redirect URI above):

   ```bash
   export FREEAGENT_CLIENT_ID=your-oauth-identifier
   export FREEAGENT_CLIENT_SECRET=your-oauth-secret
   PUBLIC_URL=http://localhost:7771 BIND_ADDR=127.0.0.1:7771 INSECURE_HTTP=true \
   OAUTH_SIGNING_KEY=$(openssl rand -hex 32) LOG_FORMAT=pretty \
   cargo run -p mcp-freeagent
   ```

   Add `FREEAGENT_ENVIRONMENT=sandbox` to use a [FreeAgent sandbox](https://signup.sandbox.freeagent.com/signup) company instead of real books.
3. **Connect Claude Code**: `claude mcp add --transport http freeagent http://localhost:7771/freeagent`, then run `/mcp`, choose freeagent and authenticate. Your browser shows the server's approval page, then FreeAgent's login.

To serve it to claude.ai, run it on a public HTTPS address:

```bash
docker build -t mcp-freeagent .
docker run -d --name mcp-freeagent -p 8443:8443 \
  -e PUBLIC_URL=https://freeagent-mcp.example.com \
  -e OAUTH_SIGNING_KEY="$(openssl rand -hex 32)" \
  -e FREEAGENT_CLIENT_ID=your-oauth-identifier \
  -e FREEAGENT_CLIENT_SECRET_FILE=/run/secrets/freeagent_secret \
  -e FREEAGENT_TOKEN_STORE=/data/tokens.json \
  -v mcp-freeagent-data:/data \
  -v /path/to/certs:/certs:ro \
  -e TLS_CERT_PATH=/certs/fullchain.pem -e TLS_KEY_PATH=/certs/privkey.pem \
  mcp-freeagent
```

Register `https://freeagent-mcp.example.com/callback` with the FreeAgent app, then in claude.ai add a custom connector for `https://freeagent-mcp.example.com/freeagent` (Settings, Connectors). Keep `OAUTH_SIGNING_KEY` stable and secret: changing it signs everyone out.

## Configuration

The FreeAgent service reads:

| Variable | Default | Meaning |
|---|---|---|
| `FREEAGENT_CLIENT_ID` | required | The app's OAuth identifier. |
| `FREEAGENT_CLIENT_SECRET` | required | The app's OAuth secret. `FREEAGENT_CLIENT_SECRET_FILE` reads it from a file. |
| `FREEAGENT_ENVIRONMENT` | `production` | `production` or `sandbox`. Changing it signs everyone out. |
| `FREEAGENT_API_URL` | from the environment | The API base, ending `/v2`. https, or http to a loopback address only (for tests). |
| `FREEAGENT_API_VERSION` | none | An `X-Api-Version` date to send, such as `2026-09-01`. |
| `FREEAGENT_TOKEN_STORE` | none | A file that keeps each sign-in's latest refresh token, encrypted, across restarts. See [Tokens](#tokens). |
| `FREEAGENT_SERVICES` | all | Services to run. There is one, `freeagent`. |
| `FREEAGENT_READ_ONLY` | none | Services whose write tools are hidden and refused, e.g. `freeagent`. |
| `FREEAGENT_COMBINED` | `true` | Also serve every service at `/mcp`. |

The server itself (`PUBLIC_URL`, `OAUTH_SIGNING_KEY`, TLS, ports, logging, limits, `AUDIT_LOG_PATH`) is configured as described in the [mcp-server-core configuration reference](https://github.com/Trozz/mcp-server-core-rs). `MCP_AUTH_TOKEN` cannot be used, because everyone signs in with their own FreeAgent account. The paths `callback` (FreeAgent's redirect) and `upload` (upload links) are reserved.

### Tokens

FreeAgent grants an access token (valid for an hour) and a refresh token. The refresh token is sealed into the person's MCP tokens; the server refreshes access tokens from it as needed, one refresh at a time per person, and keeps access tokens only in memory.

FreeAgent may hand out a new refresh token on each refresh and does not say whether the old one keeps working. The server always uses the newest one. Without `FREEAGENT_TOKEN_STORE`, a restart forgets the newest ones and people whose token was replaced must sign in again. With it, the newest refresh token of each sign-in is written to that file, encrypted with ChaCha20-Poly1305 under a key derived from the original refresh token, which only that person's MCP tokens carry: the file alone gives nothing away. Keep it on a persistent volume. Run one replica, or send each person to the same replica (sticky sessions), each with its own file.

A grant lasts at most 90 days, as with every mcp-server-core server; then the person signs in again.

FreeAgent documents HTTP Basic authentication for the app's credentials. If it refuses them that way, the server tries them in the request body, keeps doing so if that works, and logs a warning naming the variables to check.

## Tools

| Area | Tools |
|---|---|
| Company | `get_company`, `get_current_user`, `list_users`, `get_tax_timeline`, `business_snapshot` (balances, money owed each way, unexplained transactions and deadlines in one call) |
| Contacts | `search_contacts` (by name or email), `get_contact`, `create_contact`, `update_contact`, `delete_contact`, `list_notes`, `create_note` |
| Projects and time | `list_projects`, `get_project`, `create_project`, `update_project`, `list_tasks`, `create_task`, `update_task`, `list_timeslips`, `log_time`, `update_timeslip`, `delete_timeslip`, `start_timer`, `stop_timer` |
| Sales | `list_invoices`, `get_invoice`, `create_invoice` (from lines, or a project's unbilled time and expenses), `update_invoice`, `delete_invoice` (drafts only), `invoice_action` (mark sent, draft, scheduled, cancelled; convert to credit note; duplicate), `list_estimates`, `get_estimate`, `create_estimate`, `estimate_action`, `list_credit_notes`, `get_credit_note`, `create_credit_note`, `list_recurring_invoices`, `send_document_email`, `get_document_pdf`, `list_price_list_items`, `list_email_addresses` |
| Purchases | `list_bills`, `get_bill`, `create_bill`, `update_bill`, `delete_bill`, `list_expenses`, `get_expense`, `create_expense`, `create_mileage_claim`, `update_expense`, `delete_expense` |
| Banking | `list_bank_accounts`, `get_bank_account`, `list_bank_transactions`, `get_bank_transaction`, `list_bank_transaction_explanations`, `explain_bank_transaction` (a category, invoice, bill, transfer or user payment), `update_bank_transaction_explanation`, `delete_bank_transaction_explanation`, `approve_explanations` (FreeAgent's guesses), `upload_bank_statement`, `list_bank_feeds` |
| Accounting | `list_categories` (find a nominal code by words), `get_profit_and_loss`, `get_balance_sheet`, `get_trial_balance`, `get_cashflow`, `list_accounting_transactions`, `list_journal_sets`, `create_journal_set` (must balance), `list_tax_returns`, `get_tax_return`, `mark_tax_return`, `get_payroll` |
| Attachments | `create_upload_link` (a single-use link for the person to pick a receipt or invoice for an expense, bill or bank explanation), `get_upload_status`, `attach_file` (base64, for files the client can read itself) |
| Anything else | `api_get` (read-only) and `api_request` (POST, PUT, DELETE) on any FreeAgent API path |

Good to know:

- Tools take ids or the FreeAgent URLs they returned, and every result has an `id` beside each `url`. Money is a decimal string; dates are YYYY-MM-DD.
- Bank amounts follow FreeAgent: money out is negative. `explain_bank_transaction` takes the transaction's unexplained amount and date when not given, and refuses an amount with the wrong sign or more than is unexplained. `create_expense` takes what was spent as a positive amount.
- FreeAgent has no contact search, so `search_contacts` reads up to 3000 contacts and matches every word locally.
- `mark_tax_return` only records that a return was filed or paid elsewhere; nothing is filed with HMRC.
- Attachments: models cannot pass files shared in a chat to a tool, and requests to the MCP endpoints are limited to 1 MB. So `create_upload_link` gives the person a link, `PUBLIC_URL/upload/<token>`, to open on their phone or computer and pick a PDF, PNG, JPEG or GIF of up to 5 MB, which the server attaches in FreeAgent as them (`curl -F file=@receipt.pdf <link>` works too). A link works once, for 30 minutes, and lives in the server's memory, so a restart or another replica does not know it. Bank explanations get files through FreeAgent's newer attachments endpoint (`X-Api-Version: 2026-09-01`), since the single `attachment` field is withdrawn from 1 December 2026. `attach_file` and the `attachment` arguments still take base64 for small files.
- FreeAgent allows 120 requests a minute and 3600 an hour per user. Tools say how long to wait when the limit is hit. Reads are retried once after a network or server error; writes never are. A refused access token is refreshed and the request sent once more.

## Prompts

| Prompt | What it does |
|---|---|
| `business_snapshot` | A short briefing on cash, money owed, profit so far and what needs attention. |
| `monthly_bookkeeping` | A month's checklist: transactions to explain, guesses to approve, drafts to send, overdue invoices, unbilled time, bills, missing receipts. |
| `explain_transactions` | Suggests what each unexplained bank transaction was (invoice, bill, transfer, director, category with VAT) and explains them once you agree. |
| `approve_guesses` | Reviews FreeAgent's guessed explanations, flags doubtful ones, approves the rest. |
| `chase_overdue` | Drafts reminders for overdue invoices by customer, sends approved ones and notes each chase. |
| `create_invoice` | Raises an invoice from a plain description, then offers to send it. |
| `invoice_unbilled_time` | Turns unbilled time and rebillable expenses into a draft invoice. |
| `log_time` | Logs time from a description such as "3h on the Acme website yesterday". |
| `record_expense` | Records a receipt or supplier invoice as an expense, a bill or a bank explanation. |
| `vat_check` | Checks a VAT return's boxes and what could make it wrong before you file. |
| `tax_deadlines` | Upcoming and overdue tax and filing deadlines, most urgent first. |
| `month_end_close` | Checks a month is ready to close: bank, drafts, suspense account, trial balance. |
| `cashflow_review` | Cash in and out by month, money due each way, and runway. |

On `/mcp` they are named `freeagent_<prompt>`, and the tools in their text carry the same prefix.

### Claude Code plugin

`plugin/` is a Claude Code plugin with the same 13 jobs as slash commands (`/freeagent:monthly-bookkeeping`, `/freeagent:explain-transactions September 2026`, ...) and the server's MCP connection. Install it from this repository:

```text
/plugin marketplace add Trozz/mcp-freeagent
/plugin install freeagent@mcp-freeagent
```

It connects to `http://localhost:7771/freeagent` unless `FREEAGENT_MCP_URL` names your server. The command files are generated from the prompts (`UPDATE_PLUGIN=1 cargo test -p mcp-freeagent-books plugin`), and a test fails if they drift apart.

## Security

- The FreeAgent app secret is the server's only secret besides `OAUTH_SIGNING_KEY`. Keep both in a secret store (`_FILE` variables read them from files) and never commit them. FreeAgent lets an app have two secrets at once, so a leaked secret can be replaced without downtime.
- Every MCP client is shown on the approval page before the browser goes to FreeAgent, because all clients reach FreeAgent under this server's one app. The pending sign-in travels in an encrypted, `HttpOnly`, `SameSite=Lax` cookie bound to a random `state`, so FreeAgent's code is only accepted from the browser that started the sign-in.
- Book text (descriptions, names, bank references) is written by customers, suppliers and banks. The server's instructions and every prompt tell the model to treat it as data, not instructions.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check
bash scripts/test-next-version.sh
```

The tests run against an in-memory fake of the FreeAgent API (`crates/mcp-freeagent-books/tests/support/fake_freeagent.rs`) with a real token endpoint, expiring access tokens and optionally rotating refresh tokens, including the whole sign-in through FreeAgent and MCP sessions through the OAuth and bearer guards.

Releases: merging into `develop` publishes a pre-release (`ghcr.io/trozz/mcp-freeagent:develop`); merging `develop` into `main` publishes a release with `X.Y.Z`, `X.Y`, `X` and `latest` image tags and binaries. The bump comes from a `major`, `minor` or `patch` label on the PR, or `[major]`/`[minor]`/`[patch]` in its title (default patch).

Layout:

- `crates/mcp-freeagent`: the binary, wiring services into `mcp_server_core::app::Server`.
- `crates/mcp-freeagent-books`: the `freeagent` service: OAuth and tokens (`oauth.rs`), sign-in through FreeAgent (`verifier.rs`), API client (`api.rs`), tools (`tools/`), output shaping (`view.rs`) and prompts.
- `plugin/`: the Claude Code plugin; `.claude-plugin/marketplace.json` lists it.

## License

MIT
