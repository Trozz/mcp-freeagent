# Claude Code plugin

The repository is also a Claude Code plugin marketplace. Its `freeagent` plugin adds the server's MCP connection and the 13 [prompts](prompts.md) as slash commands.

## Install

In Claude Code:

```text
/plugin marketplace add Trozz/mcp-freeagent
/plugin install freeagent@mcp-freeagent
```

The plugin connects to `http://localhost:7771/freeagent`, the address of a server run locally as in [Getting started](getting-started.md#locally-to-try-it). To use another server, set `FREEAGENT_MCP_URL` in the environment Claude Code starts in:

```bash
export FREEAGENT_MCP_URL=https://freeagent-mcp.example.com/freeagent
```

Then run `/mcp`, choose `freeagent` and authenticate: your browser shows the server's consent page, then FreeAgent's login.

## Commands

Each prompt is a command named after it, with `-` for `_`:

| Command | Prompt |
|---|---|
| `/freeagent:business-snapshot [focus]` | `business_snapshot` |
| `/freeagent:monthly-bookkeeping [month]` | `monthly_bookkeeping` |
| `/freeagent:explain-transactions [bank_account] [period]` | `explain_transactions` |
| `/freeagent:approve-guesses [bank_account]` | `approve_guesses` |
| `/freeagent:chase-overdue [tone] [customer]` | `chase_overdue` |
| `/freeagent:create-invoice <request>` | `create_invoice` |
| `/freeagent:invoice-unbilled-time [client]` | `invoice_unbilled_time` |
| `/freeagent:log-time <work>` | `log_time` |
| `/freeagent:record-expense <details>` | `record_expense` |
| `/freeagent:vat-check [period]` | `vat_check` |
| `/freeagent:tax-deadlines [months]` | `tax_deadlines` |
| `/freeagent:month-end-close [month]` | `month_end_close` |
| `/freeagent:cashflow-review [months]` | `cashflow_review` |

Type the arguments in plain words after the command, for example `/freeagent:explain-transactions September 2026` or `/freeagent:log-time 3h on the Acme website yesterday`. The command reads each argument from what you typed and asks for a required one that is missing. Commands behave as the prompts do: nothing changes in FreeAgent until you agree.

The commands use the plugin's own `freeagent` server, whose tools Claude Code shows with a prefix such as `mcp__freeagent__`.

## What is in it

| Path | Contents |
|---|---|
| `.claude-plugin/marketplace.json` | The marketplace, listing the `freeagent` plugin from `./plugin` |
| `plugin/.claude-plugin/plugin.json` | The plugin's name, description and version |
| `plugin/.mcp.json` | The MCP server: HTTP at `${FREEAGENT_MCP_URL:-http://localhost:7771/freeagent}` |
| `plugin/commands/*.md` | One command per prompt |

The command files are generated from the prompts in `crates/mcp-freeagent-books/src/prompts.rs`, so the commands and the server's prompts say the same thing. After changing a prompt, regenerate them:

```bash
UPDATE_PLUGIN=1 cargo test -p mcp-freeagent-books plugin
```

A test fails if the files and the prompts drift apart.
