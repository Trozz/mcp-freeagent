# Working with the tools

Endpoint `/freeagent` (or `freeagent_<tool>` under [`/mcp`](../connecting-clients.md#the-combined-endpoint)). The 79 tools cover one company's books: the company of the FreeAgent user who signed in.

| Area | Tools | Page |
|---|---|---|
| Company | 5 | [Company and contacts](company-and-contacts.md#company) |
| Contacts and notes | 7 | [Company and contacts](company-and-contacts.md#contacts-and-notes) |
| Projects and tasks | 7 | [Projects and time](projects-and-time.md#projects-and-tasks) |
| Timeslips and timers | 6 | [Projects and time](projects-and-time.md#timeslips-and-timers) |
| Invoices, estimates, credit notes | 18 | [Sales](sales.md) |
| Bills, expenses and mileage | 11 | [Purchases](purchases.md) |
| Bank accounts, transactions and explanations | 11 | [Banking](banking.md) |
| Categories, reports, journals, tax returns, payroll | 12 | [Accounting and tax](accounting.md) |
| Any other FreeAgent endpoint | 2 | [Any other endpoint](raw-api.md) |

A good start in a conversation is `get_company` (company type, VAT status, accounting year) and `business_snapshot` (balances, money owed, unexplained transactions, deadlines).

## Reads and writes

Every tool tells the client what it does, through MCP annotations:

- **read**: `readOnlyHint`. Lists, reports and `api_get`. Never audited, and still available on a [read-only](../security.md#read-only-mode) server.
- **write**: creates, changes or sends something. Clients usually ask before running these, and every call is audited.
- **write, destructive**: also `destructiveHint`, for deletes and other changes that are hard to undo: every `delete_*`, `invoice_action`, `mark_tax_return` and `api_request`.

The server's instructions and every prompt tell the model to show the person what will be created, changed, sent or deleted, and wait for their agreement first.

## Arguments

- **Ids.** Every resource has an id. Tools take it as the id itself (`123`, or `750-1` for a sub-account) or as the FreeAgent URL a tool returned (`https://api.freeagent.com/v2/contacts/2`). Results carry an `id` beside every `url`. Never guess an id: find it with a `list_` or search tool first.
- **Money** is a decimal string with at most two decimals, such as `"1250.00"`, in the company's currency unless a currency is given. Commas and the symbols `£`, `$` and `€` are removed. Quantities, rates and percentages take up to four decimals.
- **Dates** are `YYYY-MM-DD`. `from_date` must not be after `to_date`.
- **Signs follow FreeAgent.** Bank money out is negative and money in positive. An expense's `gross_value` is negative when it is owed to the claimant, but `create_expense` and `update_expense` take what was spent as a positive `amount`.
- **Categories** are nominal codes such as `285` or `001`. `list_categories` finds them by words.
- **Text** fields are limited to 2000 characters.
- **Lists of items** (invoice and bill lines, journal entries, timeslips, statement lines) hold at most 100.
- **`extra`** on many create and update tools passes any other field by FreeAgent's own name, for fields the tool does not name.
- **Attachments** (receipts, supplier invoices) go in as base64 in `data_base64`, with a `file_name` and optionally a `content_type` (`application/pdf`, `image/png`, `image/jpeg` or `image/gif`, otherwise guessed from the file name). Requests to the server are limited to 1 MiB in all, so files over about 700 KB are refused.

## Lists and paging

List tools take `page` (1 to 10000, default 1) and `per_page` (1 to 100, default 25). The answer is:

```json
{"count": 25, "total": 140, "page": 1, "next_page": 2, "items": [...]}
```

`next_page` is present only when there is a next page; pass it as `page` for more. Answers are compact: FreeAgent's envelopes are unwrapped, and empty values (nulls, empty strings, empty lists) are dropped. `false` and `0` are kept.

## Errors

A failed tool call returns a tool error (`isError: true`) whose text starts with a kind:

| Kind | Meaning |
|---|---|
| `auth` | FreeAgent no longer accepts this connection's sign-in (revoked, expired or replaced). Sign in again: reconnect the connector, or run `/mcp` in Claude Code |
| `permission` | FreeAgent refused (HTTP 403): the signed-in user's access level does not allow it, or the feature does not apply to this company |
| `not_found` | Nothing at that id or path |
| `rate_limited` | FreeAgent's limit of 120 requests a minute and 3600 an hour per user is used up. The message says how long to wait |
| `validation` | An argument is wrong, or FreeAgent rejected the request (HTTP 400, 406, 409 or 422); the message says which |
| `upstream` | FreeAgent could not be reached, timed out or answered unexpectedly; usually temporary |
| `internal` | A problem inside the server, such as FreeAgent refusing the server's own app credentials; check the log |
| `read_only` | The service is read-only on this server (`FREEAGENT_READ_ONLY`) |

## Retries and timeouts

- Reads are retried once, after a short pause, following a network error or a 5xx answer.
- Writes are never retried once they reach FreeAgent, so a slow create cannot run twice.
- If FreeAgent refuses the access token (HTTP 401), the server refreshes it and sends the request once more. This is safe for writes, since FreeAgent refused the request before acting on it.
- Requests time out after 30 seconds (10 seconds to connect). Redirects are never followed.
