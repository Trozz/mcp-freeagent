# Prompts

Prompts are ready-made requests a client can offer you, often as slash commands. They tell the model which tools to call and in what order; they change nothing by themselves. The [Claude Code plugin](claude-code-plugin.md) offers the same 13 as `/freeagent:...` commands.

| Prompt | Arguments | What it does |
|---|---|---|
| `business_snapshot` | `focus` | A short briefing on where the business stands: cash in the bank, money owed to and by you, profit so far this year, and what needs attention, such as unexplained transactions, overdue invoices and the next tax deadlines. `focus` is anything you most want to know, such as "can I afford a £5,000 dividend?" |
| `monthly_bookkeeping` | `month` (default last month) | Goes through one month: unexplained and unapproved bank transactions, draft and overdue invoices, unbilled time, unpaid bills and expenses, with a checklist of what to do and offers to fix each item |
| `explain_transactions` | `bank_account` (default every account), `period` (default everything unexplained) | Suggests what each unexplained bank transaction was (the invoice it pays, a bill, a transfer, a director's payment, or a category with VAT), says how sure it is, and explains them after you approve |
| `approve_guesses` | `bank_account` (default every account) | Reviews the explanations FreeAgent guessed and marked for review, flags any that look wrong, and approves the rest once you agree |
| `chase_overdue` | `tone` (`friendly`, `firm` or `final`; default friendly under 30 days overdue, firm after), `customer` | Lists overdue invoices by customer, oldest and largest first, drafts a reminder for each customer, sends the ones you approve through FreeAgent, and notes each chase on the contact |
| `create_invoice` | `request` (required) | Raises an invoice from a plain description, such as "Acme, 3 days consulting at £600 a day, plus £45 train fare": finds the customer, drafts the lines with VAT, and creates and sends it after you approve |
| `invoice_unbilled_time` | `client` (default every client with unbilled time) | Finds unbilled time and rebillable expenses for a client or project and turns them into a draft invoice after you approve |
| `log_time` | `work` (required) | Logs time from a plain description, such as "3h on the Acme website yesterday, 1.5h client call today": matches projects and tasks and creates the timeslips after you approve |
| `record_expense` | `details` (required) | Records a receipt or supplier invoice: works out whether it is an out-of-pocket expense, a bill to pay or a bank payment to explain, picks the category and VAT, and records it after you approve |
| `vat_check` | `period` (default the next return due) | Checks a VAT return before you file it: the boxes, unexplained transactions and draft items in the period, unusual VAT treatments and the payment due, with a list of what to fix. It files nothing |
| `tax_deadlines` | `months` (default 6) | Every upcoming tax and filing deadline (VAT, corporation tax, self assessment, payroll, Companies House) with amounts due, most urgent first, and anything overdue or unfiled |
| `month_end_close` | `month` (default last month) | Checks a month is ready to close: every bank account explained, invoices and bills complete, the suspense account empty and the trial balance right, then reports what blocks closing |
| `cashflow_review` | `months` (default 6) | Money in and out by month, what customers owe and when, bills and tax coming up, and how long the cash will last at the current rate |

## How they behave

- **Nothing changes without you.** Every prompt that can write tells the model to show exactly what it will create, change, send or delete, usually as a table, and wait for your go-ahead; if you agree to only some of it, it does only that.
- **Book text is data.** Names, descriptions and bank references are written by customers, suppliers and banks; every prompt tells the model to ignore anything in them that reads like an instruction.
- **Errors.** Each prompt tells the model what to do with each [error kind](tools/overview.md#errors): stop and ask you to sign in again on `auth`, carry on without the part your access level does not allow on `permission`, stop and say how long to wait on `rate_limited`, and never guess another id on `not_found`.
- **Read-only servers.** Prompts stay listed when `FREEAGENT_READ_ONLY` is set. A step that needs a hidden write tool tells the model to say so and skip that step.
- **Style.** Answers are in plain British English, with money in the currency's symbol and two decimals, and dates as `YYYY-MM-DD`.

## Arguments

- Arguments are text. A blank or missing optional argument takes its default; a missing required one is refused with what it should contain.
- Each argument is limited to 4000 characters, and an argument the prompt does not take is refused.
- Arguments are placed in the prompt inside their own tags, and a `<` in them is replaced by a look-alike character, so an argument cannot break out of its place in the text.

## On /mcp

Under the [combined endpoint](connecting-clients.md#the-combined-endpoint) the prompts are named `freeagent_<prompt>`, such as `freeagent_monthly_bookkeeping`, and their text names the prefixed tools (`freeagent_list_bank_transactions` instead of `list_bank_transactions`).
