# Sales

Invoices, estimates and credit notes, and sending them. Conventions for ids, money, paging and errors are on [Working with the tools](overview.md).

## Invoices

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_invoices` | read | Invoices with reference, contact, dates, status (Draft, Open, Overdue, Paid and so on), `total_value` and `due_value` | `view`, `contact_id`, `project_id`, `sort`, `updated_since`, `with_items`, `page`, `per_page` |
| `get_invoice` | read | One invoice in full, with its line items, totals, VAT, what is paid and due, and its payment link | `id` |
| `create_invoice` | write | Create a draft invoice from line items, a project's unbilled time and expenses, or both. Returns it with its id and totals | `contact_id`, `items`, invoice fields (below) |
| `update_invoice` | write | Change only the fields given. `items` adds lines; an item with `id` changes that line, and with `id` and `delete: true` removes it. Sent invoices may be locked: `mark_as_draft` first | `id`, invoice fields |
| `delete_invoice` | write, destructive | Delete a draft invoice. Sent invoices are refused: cancel them with `invoice_action` `mark_as_cancelled`, or issue a credit note | `id` |
| `invoice_action` | write, destructive | Move an invoice through its life (below) | `id`, `action` |
| `list_recurring_invoices` | read | Recurring invoice profiles FreeAgent invoices from on a schedule, with frequency, `next_recurs_on` and status | `contact_id`, `view`: `draft`, `active` or `inactive` (default all) |

`list_invoices` `view`: `all` (default), `recent_open_or_overdue`, `open`, `overdue`, `open_or_overdue`, `draft`, `paid`, `scheduled_to_email`, `thank_you_emails`, `reminder_emails`, or `last_N_months` such as `last_3_months`. `open_or_overdue` shows what customers owe. `sort` is `created_at` (default) or `updated_at`, prefixed `-` for newest first.

### Invoice fields

| Field | Meaning |
|---|---|
| `contact_id` | The customer. Required on create |
| `items` | Line items (below), up to 100 |
| `dated_on` | The invoice date. Default on create: today |
| `payment_terms_in_days` | Days until due; `0` means due on receipt. Default on create: the contact's payment terms, otherwise 30 |
| `due_on` | A fixed due date instead of payment terms |
| `reference` | Invoice number. Default: the next in sequence |
| `currency` | ISO 4217 code. Default: the company's currency |
| `comments` | Text shown on the invoice, such as payment details or thanks |
| `discount_percent`, `po_reference`, `project_id` | |
| `ec_status` | `UK/Non-EC` (default), `EC Goods`, `EC Services`, `Reverse Charge` or `EC VAT MOSS` |
| `include_timeslips`, `include_expenses` | A project's unbilled time and rebillable expenses; see below. Need `project_id` |
| `extra` | Any other invoice field by FreeAgent's name, such as `{"payment_methods": {"stripe": true}}` |

An invoice stays a draft until `invoice_action` `mark_as_sent` or `send_document_email`.

### Invoicing a project's time and expenses

`create_invoice` with `project_id` adds:

- `include_timeslips`: the project's unbilled time, grouped as `billed_grouped_by_single_timeslip`, `billed_grouped_by_timeslip`, `billed_grouped_by_timeslip_task` or `billed_grouped_by_timeslip_date`.
- `include_expenses`: its rebillable expenses, grouped as `billed_grouped_by_single_expense` or `billed_grouped_by_expense`.

### Invoice actions

`invoice_action` `action`:

| Action | Effect |
|---|---|
| `mark_as_sent` | Marks it sent without emailing it. Also re-opens a cancelled invoice |
| `mark_as_draft` | Back to draft, so it can be edited |
| `mark_as_scheduled` | To be emailed by FreeAgent |
| `mark_as_cancelled` | Writes off an unpaid invoice |
| `convert_to_credit_note` | Makes it a draft negative invoice |
| `duplicate` | A new draft copy dated today |

To email an invoice, use `send_document_email`. To record a payment, explain the bank transaction with `paid_invoice_id` ([Banking](banking.md#explaining-a-transaction)).

## Estimates

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_estimates` | read | Estimates and quotes with reference, contact, status and totals | `view`: `all` (default), `recent`, `draft`, `non_draft`, `sent`, `approved`, `rejected` or `invoiced`; `contact_id`, `project_id`, `from_date`, `to_date`, `updated_since`, `with_items`, `page`, `per_page` |
| `get_estimate` | read | One estimate or quote in full, with its items | `id` |
| `create_estimate` | write | Create a draft estimate, quote or proposal from line items | `contact_id`, `reference` (such as `EST-012`), `items`, `estimate_type` (`Estimate` (default), `Quote` or `Proposal`), `dated_on` (default today), `currency`, `discount_percent`, `ec_status`, `notes`, `project_id`, `extra` |
| `estimate_action` | write | `mark_as_sent`, `mark_as_draft`, `mark_as_approved`, `mark_as_rejected`, `convert_to_invoice` (makes a draft invoice from it) or `duplicate` | `id`, `action` |

## Credit notes

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_credit_notes` | read | Credit notes with reference, contact, status and values | `view`: `all` (default), `recent_open_or_overdue`, `open`, `overdue`, `open_or_overdue`, `draft`, `refunded` or `last_N_months`; `contact_id`, `project_id`, `updated_since`, `page`, `per_page` |
| `get_credit_note` | read | One credit note in full, with its items | `id` |
| `create_credit_note` | write | A draft credit note for a customer, for example to credit part of an invoice. Prices are positive | `contact_id`, `items`, `dated_on` (default today), `payment_terms_in_days` (default 0), `reference`, `currency`, `comments`, `project_id`, `extra` |

Apply a credit note to an invoice or refund it in FreeAgent, or with `api_request` on `credit_note_reconciliations` ([Any other endpoint](raw-api.md)).

## Line items

Each item of `items` on invoices, estimates and credit notes:

| Field | Meaning |
|---|---|
| `description` | What was sold, as it reads on the document. Required |
| `quantity` | How many, such as `7.5`. Default: 1 |
| `price` | Price per unit before VAT, such as `80` |
| `item_type` | `Hours`, `Days`, `Weeks`, `Months`, `Years`, `Products`, `Services`, `Training`, `Expenses`, `Comment`, `Bills`, `Discount`, `Credit`, `VAT` or `Stock`; leave out for no unit |
| `category` | Income category as a nominal code, such as `001` (Sales). Default: the company's sales category |
| `sales_tax_rate` | VAT percentage, such as `20`, `5` or `0`. Default: from the contact's `charge_sales_tax` setting |
| `sales_tax_status` | `TAXABLE`, `EXEMPT` or `OUT_OF_SCOPE` |
| `id`, `delete` | On update: the id of an existing item to change, and `delete: true` to remove it. Leave `id` out to add an item |

## Sending and documents

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `send_document_email` | write | Email an invoice, estimate or credit note to the customer through FreeAgent, with a link to view (and pay) it. A draft invoice is marked as sent | `document` (`invoice`, `estimate` or `credit_note`), `id`, `subject` and `body`, or `use_template`; `to`, `from`, `email_to_sender` |
| `get_document_pdf` | read | The PDF of an invoice, estimate or credit note, as an embedded `application/pdf` resource, for example to attach or save | `document`, `id` |
| `list_email_addresses` | read | The verified addresses FreeAgent can send from, for `send_document_email`'s `from` | none |
| `list_price_list_items` | read | The price list: saved products and services with code, description, price, unit and VAT, to reuse as invoice lines | `sort`: `created_at`, `code` or `updated_at`; `page`, `per_page` |

`send_document_email` details:

- `to` is comma-separated addresses. Default: the contact's billing email, otherwise its main email.
- `from` must be a verified address from `list_email_addresses`. Default: FreeAgent's.
- `use_template: true` sends FreeAgent's own email template instead of `subject` and `body`. FreeAgent adds the view and pay link either way.
- `email_to_sender` sends a copy to the sender. Default: true.
