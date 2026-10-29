# Projects and time

Projects for clients, the tasks time is logged against, and the timeslips themselves. Conventions for ids, money, paging and errors are on [Working with the tools](overview.md).

## Projects and tasks

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_projects` | read | Projects with their client (contact), status, budget, billing rate and currency | `contact_id`, `view`: `active` (default), `completed`, `cancelled` or `hidden`; `page`, `per_page` |
| `get_project` | read | One project in full: client, status, budget, billing rate, dates and currency | `id` |
| `create_project` | write | Create a project for a client. `status` defaults to `Active`, `budget_units` to `Hours` and `currency` to the company's | `contact_id`, `name`, project fields (below) |
| `update_project` | write | Change only the fields given. Set `status` `Completed` to close it | `id`, project fields |
| `list_tasks` | read | Tasks, optionally for one project, with billing rate and whether they are billable | `project_id`, `view`: `all` (default), `active`, `completed` or `hidden`; `updated_since`, `page`, `per_page` |
| `create_task` | write | Create a task on a project, such as Development or Meetings, so time can be logged against it | `project_id`, `name`, `billing_rate`, `billing_period` (`hour` or `day`), `is_billable` (default `true`), `status` |
| `update_task` | write | Change only the fields given | `id`, `name`, `billing_rate`, `billing_period`, `is_billable`, `status` (`Active`, `Completed` or `Hidden`) |

### Project fields

`name`, `status` (`Active`, `Completed`, `Cancelled` or `Hidden`), `budget` and `budget_units` (`Hours`, `Days` or `Monetary`: what the budget counts), `normal_billing_rate` (the default rate for its tasks, such as `450`) with `billing_period` (`hour` or `day`), `hours_per_day` (for day rates, such as `7.5`), `currency` (ISO 4217, such as `GBP`), `starts_on`, `ends_on`, `contract_po_reference`, and `extra` for any other field by FreeAgent's name, such as `{"is_ir35": true}`.

## Timeslips and timers

| Tool | Kind | What it does | Key parameters |
|---|---|---|---|
| `list_timeslips` | read | Logged time with date, hours, comment, task, project and user, and whether each is billed. `view` `unbilled` shows time not yet invoiced; `running` shows live timers | `view`: `all` (default), `unbilled` or `running`; `project_id`, `task_id`, `user_id`, `from_date`, `to_date`, `updated_since`, `nested`, `page`, `per_page` |
| `log_time` | write | Create one or more timeslips. The user defaults to the signed-in user and the project to the task's own. Returns the timeslips created | `entries` (1 to 100, below) |
| `update_timeslip` | write | Change a timeslip's hours, date, comment or task. Billed timeslips cannot be changed | `id`, `hours`, `dated_on`, `comment`, `task_id` |
| `delete_timeslip` | write, destructive | Delete a timeslip | `id` |
| `start_timer` | write | Start the timer on a timeslip. To time new work, create one with `log_time` and `hours` `0` first. Only one timer runs at a time | `id` |
| `stop_timer` | write | Stop the timer; the timeslip's hours then include the time timed | `id` |

`nested: true` on `list_timeslips` includes each timeslip's user, project and task in full.

### Timeslip entries

Each entry of `log_time`'s `entries`:

| Field | Required | Meaning |
|---|---|---|
| `task_id` | yes | The task worked on (`list_tasks`) |
| `dated_on` | yes | The day, `YYYY-MM-DD` |
| `hours` | yes | Hours as a decimal: 1h30 is `1.5`. `0` with `start_timer` to time it live |
| `comment` | no | |
| `project_id` | no | Default: the task's own project |
| `user_id` | no | Who did the work. Default: the signed-in user |

Unbilled time becomes an invoice with `create_invoice` and `include_timeslips`; see [Sales](sales.md#invoicing-a-projects-time-and-expenses).
