# Purchases

Bills from suppliers, expenses people paid themselves, and mileage. Conventions for ids, money, paging and errors are on [Working with the tools](overview.md).

Which tool to use for a cost:

- A supplier's invoice the company will pay: a **bill** (`create_bill`). Pay it by explaining the bank transaction with `paid_bill_id`.
- Something a person paid personally and is owed back: an **expense** (`create_expense`).
- A cost paid from a company bank account or card: neither. **Explain the bank transaction** instead ([Banking](banking.md#explaining-a-transaction)).
- Business mileage: `create_mileage_claim`.

## Bills

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_bills` | read | Bills with reference, supplier, dates, status, `total_value` and `due_value`. `view` `open_or_overdue` shows what the company owes | `view`: `all` (default), `open`, `overdue`, `open_or_overdue`, `paid`, `recurring`, `hire_purchase` or `cis`; `contact_id`, `project_id`, `from_date`, `to_date`, `updated_since`, `with_items`, `page`, `per_page` |
| `get_bill` | read | One bill in full, with its lines, VAT and what is paid and due | `id` |
| `create_bill` | write | Record a bill: reference, date, lines with a category and total including VAT, and optionally the supplier's invoice attached. Due 30 days after its date unless `due_on` is given | `contact_id`, `reference`, `dated_on`, `due_on`, `items`, `attachment`, `currency`, `comments`, `ec_status`, `extra` |
| `update_bill` | write | Change only the fields given. `items` adds lines; an item with `id` changes that line, and with `id` and `delete: true` removes it | `id`, bill fields |
| `delete_bill` | write, destructive | Delete a bill | `id` |

`ec_status` is `UK/Non-EC` (default), `EC Goods`, `EC Services` or `Reverse Charge`. `extra` takes any other bill field by FreeAgent's name, such as `{"recurring": "Monthly"}`.

### Bill lines

Each item of a bill's `items`:

| Field | Meaning |
|---|---|
| `category` | Expense category as a nominal code, such as `285` (Accommodation and Meals). Required |
| `total_value` | The line's total including VAT, such as `120.00` |
| `total_value_ex_tax` | Or the line's total before VAT |
| `description`, `quantity`, `project_id` | |
| `unit` | `-no unit-`, `Hours`, `Days`, `Weeks`, `Months`, `Years`, `Products`, `Services`, `Training` or `Stock` |
| `sales_tax_rate` | VAT percentage, such as `20`. Default: from the category |
| `sales_tax_status` | `TAXABLE`, `EXEMPT` or `OUT_OF_SCOPE` |
| `manual_sales_tax_amount` | An exact VAT amount, overriding the rate |
| `id`, `delete` | On update: the line to change, and `delete: true` to remove it |

## Expenses and mileage

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_expenses` | read | Expense claims and mileage with user, category, date, description and `gross_value` (negative is money owed to the claimant) | `view`: `recent` or `recurring` (default all); `project_id`, `from_date`, `to_date`, `updated_since`, `page`, `per_page` |
| `get_expense` | read | One expense or mileage claim in full: user, category, date, amounts, VAT, receipt and rebilling | `id` |
| `create_expense` | write | Record an expense someone paid personally: category, date, description and the amount spent including VAT, optionally with the receipt | expense fields (below) |
| `create_mileage_claim` | write | Claim business mileage at HMRC's approved rates. FreeAgent works out the amount | `dated_on`, `description`, `mileage`, and the fields below |
| `update_expense` | write | Change only the fields given. `amount` is what was spent, positive | `id`, expense fields |
| `delete_expense` | write, destructive | Delete an expense | `id` |

### Expense fields

| Field | Meaning |
|---|---|
| `amount` | What was spent, including VAT, as a positive amount such as `42.50`. The server stores it with FreeAgent's sign |
| `refund` | `true` when the claimant paid money back to the company instead |
| `category` | Expense category as a nominal code, such as `285`; see `list_categories` |
| `dated_on`, `description`, `receipt_reference` | |
| `user_id` | Who paid and is owed it. Default on create: the signed-in user |
| `currency` | ISO 4217 code when not the company's currency |
| `sales_tax_rate` | VAT percentage. Default: from the category |
| `manual_sales_tax_amount` | An exact reclaimable VAT amount, overriding the rate |
| `project_id`, `rebill_type` | A project to rebill it to, and how: `cost`, `markup` or `price` |
| `attachment` | The receipt |
| `extra` | Any other expense field by FreeAgent's name |

### Mileage fields

| Field | Meaning |
|---|---|
| `dated_on`, `description` | Required. The description says where and why, such as "Leeds to Manchester, client meeting" |
| `mileage` | Required. Distance in the company's mileage units (miles unless set to kilometres) |
| `vehicle_type` | `Car` (default), `Motorcycle` or `Bicycle` |
| `engine_type` | `Petrol` (default), `Diesel`, `LPG`, `Electric`, `Electric (Home charger)` or `Electric (Public charger)` |
| `engine_size` | From FreeAgent's mileage settings, such as "Up to 1400cc"; needed for VAT on fuel |
| `have_vat_receipt` | Whether there is a VAT receipt for the fuel |
| `reclaim` | Reclaim at the HMRC approved rate. Default: `true`; `false` only rebills it |
| `user_id` | Who drove. Default: the signed-in user |
| `project_id` | |

## Attachments

`attachment` on bills, expenses and bank explanations:

| Field | Meaning |
|---|---|
| `file_name` | Required, such as `receipt.pdf` |
| `data_base64` | Required. The file, base64-encoded |
| `content_type` | `application/pdf`, `image/png`, `image/jpeg` or `image/gif`. Default: from `file_name` |
| `description` | |

Requests to the server are limited to 1 MiB in all, so a file over about 700 KB is refused before it reaches FreeAgent.
