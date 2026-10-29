# Any other endpoint

Two tools reach any part of the FreeAgent API the other tools do not cover, as the signed-in user. Prefer the specific tools when one fits: they fill in defaults, check arguments and shape the answers.

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `api_get` | read | Read any endpoint, such as `capital_assets`, `stock_items`, `hire_purchases`, `properties`, `account_locks`, `cis_bands`, `sales_tax_periods`, `credit_note_reconciliations`, `payroll_profiles/2026` or `accounting/trial_balance/summary/opening_balances`. Returns FreeAgent's JSON with ids added | `path`, `query` |
| `api_request` | write, destructive | Create, change or delete through any endpoint with `POST`, `PUT` or `DELETE`, with the body FreeAgent's API documentation gives | `method`, `path`, `query`, `body` |

Because `api_get` is a read and `api_request` a write, a [read-only](../security.md#read-only-mode) server still allows the first and refuses the second.

## Paths

- `path` is the endpoint below `/v2`, such as `stock_items/12`. A path starting `/v2/`, or a full URL on the configured FreeAgent API host, is accepted too.
- Segments may hold letters, digits, `-`, `_` and `.`; `.` and `..` on their own are refused, and so is a path over 200 characters.
- A URL on any other host is refused, so the tools never send the person's access token elsewhere.
- FreeAgent's OAuth endpoints, `token_endpoint` and `approve_app`, are refused.
- Query parameters go in `query`, never in `path`.

## Query and body

- `query` is an object of up to 20 parameters, such as `{"view": "all", "page": "2"}`. Names are lower-case letters, digits and `_`; values are strings, numbers or booleans.
- `body` is a JSON object, the one FreeAgent documents for the endpoint, with its root key, such as `{"price_list_item": {...}}`.

`api_get` answers `{"body": ..., "total": ..., "has_next_page": true}`, with `total` and `has_next_page` present when FreeAgent's paging headers give them. `api_request` answers `{"body": ...}`, with `location` when FreeAgent names a created resource.

The [FreeAgent API documentation](https://dev.freeagent.com/docs) lists every endpoint and its fields.
