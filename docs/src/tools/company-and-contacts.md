# Company and contacts

Who the books belong to, who is signed in, and the customers and suppliers in them. Conventions for ids, money, paging and errors are on [Working with the tools](overview.md).

## Company

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `get_company` | read | The company these books belong to: name, type (limited company, sole trader and so on), currency, accounting year dates, VAT registration and rates, address and registration number. Call it first for context | none |
| `get_current_user` | read | The signed-in FreeAgent user: name, email, role and `permission_level` (see [Access levels](../signing-in.md#access-levels)) | none |
| `list_users` | read | The company's FreeAgent users (directors, employees, accountants) with their ids, roles and permission levels. Needs the tax, accounting and users access level | `view`: `all` (default), `staff`, `active_staff`, `advisors` or `active_advisors` |
| `get_tax_timeline` | read | Upcoming and recent deadlines for VAT returns, corporation tax, self assessment, payroll and Companies House filings, with amounts due where known | none |
| `business_snapshot` | read | Where the business stands, in one call: company basics, each bank account's balance and number of unexplained transactions, money owed to you (open and overdue invoices) and by you (open and overdue bills), and the next tax deadlines | none |

### business_snapshot

- It sums up to 500 open invoices and 500 open bills; when there are more, that section is marked `truncated`.
- It counts unexplained transactions for the first 10 bank accounts.
- A section it cannot read, for example because the user's access level does not cover banking, is listed under `unavailable` with the error, and the rest is still returned.
- Bank balances are FreeAgent's, from explained transactions, not the bank's live balance.

## Contacts and notes

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `search_contacts` | read | Find customers and suppliers. With `query`, every word must appear in the name, organisation or email; without it, lists a page. Each result has its id, name, email and `account_balance` | `query`, `view`, `sort`, `updated_since`, `page`, `per_page` |
| `get_contact` | read | One contact in full: names, emails, phones, address, payment terms, VAT settings, `account_balance` and active projects count | `id` |
| `create_contact` | write | Create a customer or supplier. Give `organisation_name`, or `first_name` and `last_name`. Search first so you do not create a duplicate | contact fields (below) |
| `update_contact` | write | Change only the fields given. `status` `Hidden` hides it from lists | `id`, contact fields, `status` (`Active` or `Hidden`) |
| `delete_contact` | write, destructive | Delete a contact. FreeAgent refuses when it has invoices, bills or projects: hide it with `update_contact` instead | `id` |
| `list_notes` | read | The notes on one contact or project, newest first, with author and dates | `contact_id` or `project_id` |
| `create_note` | write | Add a note to a contact or project, for example a record of a call or a payment promise | `note`, and `contact_id` or `project_id` |

### Searching contacts

FreeAgent has no contact search of its own, so with a `query` the tool reads up to 3000 contacts and matches every word locally, ignoring case. When there are more, it says only the first 3000 were searched; narrow with `view` or `updated_since`.

- `view`: `all`, `active` (default), `clients`, `suppliers`, `active_projects`, `completed_projects`, `open_clients`, `open_suppliers` or `hidden`.
- `sort`: `name` (default), `created_at` or `updated_at`; prefix `-` for newest first, such as `-updated_at`.
- `updated_since`: `YYYY-MM-DD` or an ISO 8601 date-time.

### Contact fields

`organisation_name`, `first_name`, `last_name`, `email`, `billing_email` (where invoices are emailed, when not `email`), `phone_number`, `mobile`, `address1`, `address2`, `address3`, `town`, `region`, `postcode`, `country` (as FreeAgent spells it, such as `United Kingdom`), `default_payment_terms_in_days`, `charge_sales_tax` (`Auto` (default), `Always` or `Never`), `sales_tax_registration_number` (their VAT number) and `extra` for any other field by FreeAgent's name, such as `{"locale": "fr", "contact_name_on_invoices": true}`.
