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

## Reviews

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `month_end_check` | read | What to finish before a month end or VAT return, with a `to_do` list: unexplained transactions and guesses to approve per bank account, expenses without receipts and the VAT they reclaim (mileage needs none), bills without the supplier's invoice, draft invoices in the period or over 30 days old, overdue invoices and bills, the `999` Suspense balance and overdrawn directors' loan accounts. Default period: the next VAT return's, or last month for a company not registered for VAT | `from_date` and `to_date`, or neither |
| `director_loan_review` | read | Each director's loan account (positive: they owe the company), the expenses and salary the company owes them, and the net; the loan at the last year end, the s455 deadline nine months later, what was still owed then and the estimated s455 tax at 33.75%; a flag for loans over £10,000 (benefit in kind); and a set-off suggestion when expenses are owed to them. Figures are the balance sheet's, to the nearest pound | `as_at_date` (default today) |

## Planning

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `tax_set_aside` | read | How much to keep back for tax: corporation tax on the operating profit of this accounting year so far (19% up to £50,000, 25% from £250,000 and marginal relief between, with the limits cut for a short year and shared with associated companies) and for the full year if the profit continues; corporation tax, VAT and PAYE already owed on the balance sheet; the next VAT return; the bank balances and what is left after keeping it back | `as_at_date` (default today), `associated_companies` |
| `dividend_check` | read | Whether a dividend can be paid: retained profits carried forward in the profit and loss summary, less corporation tax on this year's profit not yet booked; the cash after keeping back tax; the most that could be paid; and for `amount`, whether it fits. With `shareholders`, each one's part and what crediting it to their director's loan account would do, with the s455 deadline | `amount`, `shareholders` (`name`, `shares`), `as_at_date`, `associated_companies` |
| `draft_dividend_paperwork` | read | Drafts the board minute and a dividend voucher for each shareholder, with the company's name, number and registered office. Text to check and sign; nothing is stored | `declared_on`, `total` or `per_share`, `shareholders`, `paid_on`, `kind` (`interim` or `final`), `share_class`, `directors_present` |
| `accountant_pack` | read | A year-end pack: profit and loss and balance sheet headlines, the trial balance, directors' loan accounts with s455, journals posted in the year, open items, and `questions_for_accountant`, such as income or cost categories with the wrong-way balance, suspense, large bank charges, large journals and corporation tax not yet booked | `year_end` (default the last year that ended) |
| `compare_periods` | read | Profit and loss for a period against the same period a year earlier from the trial balance: income, cost of sales, expenses and profit with the change, and the categories that changed most | `from_date`, `to_date` (default this accounting year to date), `compare_from_date`, `compare_to_date` (default a year earlier) |

- Corporation tax is estimated on the profit in the books, with no adjustment for disallowed costs, capital allowances or losses; the balance sheet is to the nearest pound. FreeAgent keeps no share register, so give the shareholders.
