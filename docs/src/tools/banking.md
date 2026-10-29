# Banking

Bank accounts, their transactions, and explaining what each transaction was. Conventions for ids, money, paging and errors are on [Working with the tools](overview.md).

Bank amounts follow FreeAgent: money out is negative and money in positive.

## Accounts and transactions

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_bank_accounts` | read | The company's bank, credit card and PayPal accounts with id, name, type, currency and FreeAgent's `current_balance` (from explained transactions, not the bank's live balance) | `view`: `standard_bank_accounts`, `credit_card_accounts` or `paypal_accounts` (default all) |
| `get_bank_account` | read | One bank account in full: type, currency, balance, account details and latest activity | `id` |
| `list_bank_transactions` | read | One account's transactions with date, amount, description, `unexplained_amount` and their explanations | `bank_account_id`, `view`, `from_date`, `to_date`, `updated_since`, `last_uploaded`, `page`, `per_page` |
| `get_bank_transaction` | read | One bank transaction in full, with its explanations | `id` |
| `list_bank_feeds` | read | Which accounts are connected to their bank, the feed state, and when its authorisation (SCA) expires and must be renewed in FreeAgent | none |
| `upload_bank_statement` | write | Add transactions to an account from statement lines, for example for an account without a bank feed | `bank_account_id`, `lines` (1 to 100, below) |

`list_bank_transactions` `view`: `all` (default), `unexplained` (what still needs explaining), `explained`, `manual`, `imported` or `marked_for_review` (explained by FreeAgent's guess, awaiting approval). `last_uploaded: true` shows only the transactions of the latest statement upload.

### Statement lines

Each line of `upload_bank_statement`'s `lines`:

| Field | Meaning |
|---|---|
| `dated_on`, `description` | Required |
| `amount` | Required. Money out negative, money in positive |
| `fitid` | The bank's own transaction id, if known, so it is not imported twice |
| `transaction_type` | `CREDIT`, `DEBIT`, `INT`, `DIV`, `FEE`, `SRVCHG`, `DEP`, `ATM`, `POS`, `XFER`, `CHECK`, `PAYMENT`, `CASH`, `DIRECTDEP`, `DIRECTDEBIT`, `REPEATPMT` or `OTHER` (default) |

Upload whole days: FreeAgent skips lines that match one already there by date, amount and description. Afterwards, `list_bank_transactions` with `last_uploaded: true` shows what was imported.

## Explanations

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_bank_transaction_explanations` | read | The explanations of one account's transactions: what each payment or receipt was (category, invoice, bill, transfer or user), with VAT | `bank_account_id`, `marked_for_review_only`, `from_date`, `to_date`, `updated_since`, `page`, `per_page` |
| `explain_bank_transaction` | write | Explain (categorise) a transaction: what the money was (below) | `bank_transaction_id`, and one of `category`, `paid_invoice_id`, `paid_bill_id`, `transfer_bank_account_id` or `paid_user_id` |
| `update_bank_transaction_explanation` | write | Change only the fields given. `marked_for_review: false` approves one FreeAgent guessed | `id`, explanation fields, `marked_for_review` |
| `delete_bank_transaction_explanation` | write, destructive | Delete an explanation, leaving the transaction unexplained again | `id` |
| `approve_explanations` | write | Approve explanations FreeAgent guessed in one account (below) | `bank_account_id`, `explanation_ids` or `approve_all`, `from_date`, `to_date` |

### Explaining a transaction

`explain_bank_transaction` takes the transaction (`bank_transaction_id`, from `list_bank_transactions` with `view` `unexplained`) and one way of saying what the money was:

| Field | The money was |
|---|---|
| `category` | An ordinary payment or receipt, by nominal code such as `285` or `001`; see `list_categories` |
| `paid_invoice_id` | A customer paying an invoice (or a credit note refunded) |
| `paid_bill_id` | Paying a supplier's bill (or a bill refund) |
| `transfer_bank_account_id` | Moved to or from another of the company's own accounts |
| `paid_user_id` | Paid to or received from a director or employee (expenses repaid, salary, director's loan). Add `category` for a specific kind, such as `902` net salary |

Other fields: `gross_value` (the amount explained), `dated_on`, `description`, `sales_tax_rate` (default from the category), `sales_tax_status` (`TAXABLE`, `EXEMPT` or `OUT_OF_SCOPE`), `ec_status` (`UK/Non-EC`, `EC Goods`, `EC Services`, `Reverse Charge` or `EC VAT MOSS`), `foreign_currency_value` (for a foreign-currency invoice or bill, the amount in its currency), `project_id` and `rebill_type` (`cost`, `markup` or `price`), `receipt_reference`, `attachment` (see [Attachments](purchases.md#attachments)) and `extra` for any other explanation field, such as a stock item or capital asset.

- `gross_value` and `dated_on` default to the transaction's unexplained amount and date.
- An amount with the wrong sign, or more than is unexplained, is refused. So is a transaction that is already fully explained: change its explanation with `update_bank_transaction_explanation` instead.
- A part explanation leaves the rest unexplained, to explain separately.

### Approving FreeAgent's guesses

FreeAgent guesses explanations for imported transactions and marks them for review. `approve_explanations` works on one account:

- With neither `explanation_ids` nor `approve_all`, it only lists the explanations awaiting approval (in the dates given) and changes nothing, so you can show them to the person.
- With `explanation_ids`, it approves those, up to 200; the dates are then not used.
- With `approve_all: true`, it approves every explanation awaiting approval in the account and dates, up to 200.

The result lists the ids approved and any that failed. `update_bank_transaction_explanation` with `marked_for_review: false` approves a single one.
