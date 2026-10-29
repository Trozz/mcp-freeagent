# mcp-freeagent

mcp-freeagent is a self-hosted [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server for [FreeAgent](https://www.freeagent.com), the UK accounting software. It lets MCP clients such as claude.ai, Claude Desktop and Claude Code read a company's books and keep them: contacts, projects and time, invoices, estimates and credit notes, bills, expenses and mileage, bank transactions and their explanations, reports, tax returns and payroll.

| Endpoint | What it does |
|---|---|
| `/freeagent` | 79 [tools](tools/overview.md) and 13 [prompts](prompts.md) for the books of the company whose FreeAgent user signed in |
| `/mcp` | Every enabled service's tools and prompts in one connector, named `<service>_<tool>`, for example `freeagent_list_invoices`. See [Connecting clients](connecting-clients.md#the-combined-endpoint) |

## Highlights

- **People sign in with FreeAgent itself.** The server sends each person to FreeAgent to log in and approve its app, and keeps the refresh token FreeAgent grants only in encrypted form inside that person's own MCP tokens. There are no API keys to paste, and every call acts as that FreeAgent user, within their access level. See [Signing in with FreeAgent](signing-in.md).
- **Read and write.** Lists and reports, plus tools that create and change records: raise and send invoices, explain bank transactions, approve FreeAgent's guesses, record bills and expenses, log time, post journals. `api_get` and `api_request` reach every other FreeAgent endpoint.
- **Prompts as slash commands.** 13 ready-made jobs, such as monthly bookkeeping, explaining bank transactions and chasing overdue invoices. The [Claude Code plugin](claude-code-plugin.md) adds them as `/freeagent:...` commands.
- **Safe by default.** Write tools are annotated so clients ask before running them, and the prompts show what will change and wait for agreement. `FREEAGENT_READ_ONLY=freeagent` hides and refuses every write tool. Every write is recorded in an audit log that never contains arguments or results.
- A single static Rust binary, also shipped as a `FROM scratch` container image (`ghcr.io/trozz/mcp-freeagent`). TLS end to end, a built-in OAuth 2.1 server for claude.ai custom connectors, and no database.

mcp-freeagent is built on [mcp-server-core](https://github.com/Trozz/mcp-server-core-rs), the same base as [mcp-icloud](https://github.com/Trozz/mcp-icloud), [mcp-companies-house](https://github.com/Trozz/mcp-companies-house) and [mcp-twenty-crm](https://github.com/Trozz/mcp-twenty-crm).

## How it talks to FreeAgent

The server uses the FreeAgent API v2 and FreeAgent's own OAuth:

| Purpose | Address |
|---|---|
| The live API | `https://api.freeagent.com/v2` |
| The sandbox API (`FREEAGENT_ENVIRONMENT=sandbox`) | `https://api.sandbox.freeagent.com/v2` |
| Approving the app (in the person's browser) | `approve_app` below the API base |
| Exchanging and refreshing tokens | `token_endpoint` below the API base |

Every request names the server in its `User-Agent`, as FreeAgent requires, and FreeAgent's rate limits apply per user: 120 requests a minute and 3600 an hour.

## What it does not do

- **File returns with HMRC.** FreeAgent files VAT, corporation tax and self assessment returns from its own interface. `mark_tax_return` only records that a return was filed or paid elsewhere.
- **Read the bank's live balance.** Bank balances are FreeAgent's, built from explained transactions.
- **Run payroll.** `get_payroll` reads pay periods, payments and payslips; it changes nothing.
- **Act beyond the signed-in user.** FreeAgent refuses anything above that user's access level, and the tool says so.

## Privacy

The server keeps no copy of the books. Logs record tool names, outcomes and durations, never the content of invoices, contacts or transactions, unless you explicitly turn on unsafe content logging while debugging. See [Security](security.md).

Next: [Getting started](getting-started.md).
