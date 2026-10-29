# Accounting and tax

The chart of accounts, reports, the general ledger and manual journals, tax returns and payroll. Conventions for ids, money, paging and errors are on [Working with the tools](overview.md).

## Categories

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_categories` | read | Find categories (the chart of accounts) by words in their name, code or tax reporting name. Their `nominal_code` is what bills, expenses, invoice lines and bank explanations take as `category` | `query` (such as `travel`, `software` or `285`; leave out for all), `group`, `sub_accounts` |

- `group`: `income`, `cost_of_sales`, `admin_expenses` or `general` (the balance sheet). Default: all.
- `sub_accounts: true` includes sub-accounts such as each bank account (`750-x`) and each user (`900-x`).

## Reports

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `get_profit_and_loss` | read | Income, expenses, operating profit, deductions such as corporation tax and dividends, and retained profit. Default: the current accounting year to date | `from_date`, `to_date`, or `accounting_period` (a whole year, such as `2025/26`) |
| `get_balance_sheet` | read | Capital assets, current assets and liabilities, net current assets, total assets and owners' equity at a date (FreeAgent rounds these to whole units) | `as_at_date` (default today) |
| `get_trial_balance` | read | Every category with its total for a period. A non-zero `999` Suspense or large unexplained balances point at bookkeeping to finish | `from_date`, `to_date` (default today) |
| `get_cashflow` | read | Cash in and out by month, from the bank accounts, with the closing balance. Historic only. Default: the last 12 months | `from_date`, `to_date` (default today) |
| `list_accounting_transactions` | read | The general ledger: every posting for a period, optionally for one category, with `debit_value` (credit negative) and the item it came from | `from_date`, `to_date`, `nominal_code` (such as `285` or `750-1`), `page`, `per_page` |

For `get_trial_balance` and `get_cashflow`, the period must be at most 12 months or within one accounting year.

## Journals

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_journal_sets` | read | Manual journals, optionally by tag | `tag`, `from_date`, `to_date`, `page`, `per_page` |
| `create_journal_set` | write | Post a manual journal, such as a payroll journal or an accrual. Its entries must balance. Only for someone who knows double entry | `dated_on`, `description`, `entries` (2 to 100, below), `tag` |

Each entry of `entries`:

| Field | Meaning |
|---|---|
| `category` | Required. The category's nominal code, such as `401` or `814` |
| `debit_value` | Required. Debit positive, credit negative, such as `2500.00` or `-2500.00`. The entries' debit values must add up to zero |
| `description` | |
| `user_id` | For user categories (900 to 910): the user |

Journals are tagged `mcp-freeagent` unless another `tag` is given, so `list_journal_sets` with `tag` `mcp-freeagent` finds the ones posted through this server.

## Tax returns

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_tax_returns` | read | Tax returns of one kind with periods, filing due dates, filing status and payments due | `kind`, `user_id` |
| `get_tax_return` | read | One return in full by its period end date. A VAT return includes its box 1 to 9 breakdown and payments | `kind`, `period_ends_on` (as `list_tax_returns` gives it), `user_id` |
| `mark_tax_return` | write, destructive | Record that a return was filed or paid outside FreeAgent, or undo that: `mark_as_filed`, `mark_as_unfiled`, `mark_as_paid` or `mark_as_unpaid`. Needs full access | `kind`, `period_ends_on`, `action`, `payment_date`, `user_id` |

- `kind` is `vat`, `corporation_tax`, `self_assessment` or `final_accounts`.
- `user_id` applies to `self_assessment` only. Default: the signed-in user.
- Marking a VAT or self assessment payment needs `payment_date`: the payment's due date, as the return's payments list it.

**Nothing is filed with HMRC.** FreeAgent files returns from its own interface; `mark_tax_return` only records what happened elsewhere. The [`vat_check`](../prompts.md) prompt checks a VAT return before you file it in FreeAgent.

## Payroll

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `get_payroll` | read | Payroll for a tax year (UK, read only): its pay periods with filing status and the PAYE payments due; with `period`, that period's payslips with pay, tax, NI, pensions and student loans | `year`, `period` |

- `year` is the tax year by the year it ends: `2026` for April 2025 to April 2026.
- `period` is a pay period, `0` to `11`. Leave it out for the year's periods and payments.
