//! Timeslips: time logged against a project's tasks, and their timers.

use super::contacts::IdParams;
use super::{
    BooksServer, MAX_ITEMS, Query, choice, date_range, decimal_arg, fields, id_arg, json_result,
    opt_decimal, opt_s, paging, push, ref_arg, required_date, run, since_arg, text_arg, wrap,
};
use crate::error::FaError;
use crate::view;
use axum::http::request::Parts;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListTimeslipsParams {
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    /// all (default), unbilled or running.
    pub view: Option<String>,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub updated_since: Option<String>,
    /// Include each timeslip's user, project and task in full.
    pub nested: Option<bool>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct TimeEntry {
    /// The task worked on (list_tasks).
    pub task_id: String,
    /// The task's project. Default: the task's own project.
    pub project_id: Option<String>,
    /// Who did the work. Default: the signed-in user.
    pub user_id: Option<String>,
    /// The day, YYYY-MM-DD.
    pub dated_on: String,
    /// Hours as a decimal: 1h30 is 1.5. 0 with start_timer to time it live.
    pub hours: String,
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct LogTimeParams {
    /// One or more timeslips, up to 100.
    pub entries: Vec<TimeEntry>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateTimeslipParams {
    pub id: String,
    pub hours: Option<String>,
    pub dated_on: Option<String>,
    pub comment: Option<String>,
    pub task_id: Option<String>,
}

#[tool_router(router = time_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "List timeslips (logged time) with date, hours, comment, task, project and user, and whether each is billed. view unbilled shows time not yet invoiced; running shows live timers.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_timeslips(
        &self,
        Parameters(p): Parameters<ListTimeslipsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_timeslips", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
            push(
                &mut q,
                "view",
                choice("view", p.view.as_deref(), &["all", "unbilled", "running"])?
                    .map(String::from),
            );
            push(
                &mut q,
                "user",
                ref_arg(&c, "user_id", "users", p.user_id.as_deref())?,
            );
            push(
                &mut q,
                "project",
                ref_arg(&c, "project_id", "projects", p.project_id.as_deref())?,
            );
            push(
                &mut q,
                "task",
                ref_arg(&c, "task_id", "tasks", p.task_id.as_deref())?,
            );
            push(
                &mut q,
                "updated_since",
                since_arg(p.updated_since.as_deref())?,
            );
            if p.nested == Some(true) {
                q.push(("nested", "true".into()));
            }
            Ok(json_result(
                &c.list("timeslips", &["timeslips"], "timeslips", q, pg)
                    .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Log time: create one or more timeslips against tasks. The user defaults to the signed-in user and the project to the task's own. Returns the timeslips created.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn log_time(
        &self,
        Parameters(p): Parameters<LogTimeParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("log_time", async {
            let c = self.caller(&parts)?;
            if p.entries.is_empty() || p.entries.len() > MAX_ITEMS {
                return Err(FaError::invalid(format!(
                    "entries must hold 1 to {MAX_ITEMS} timeslips"
                )));
            }
            let mut me: Option<String> = None;
            let mut task_projects: std::collections::HashMap<String, String> = Default::default();
            let mut slips = Vec::new();
            for (i, e) in p.entries.iter().enumerate() {
                let n = |f: &str| format!("entries[{i}].{f}");
                let task_id = id_arg(&n("task_id"), &e.task_id)?;
                let project =
                    match ref_arg(&c, &n("project_id"), "projects", e.project_id.as_deref())? {
                        Some(p) => p,
                        None => match task_projects.get(&task_id) {
                            Some(p) => p.clone(),
                            None => {
                                let t = c.one("task", &["tasks", &task_id], "task", &[]).await?;
                                let p = t
                                    .get("project")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        FaError::invalid(format!(
                                            "task {task_id} has no project; give project_id"
                                        ))
                                    })?
                                    .to_string();
                                task_projects.insert(task_id.clone(), p.clone());
                                p
                            }
                        },
                    };
                let user = match ref_arg(&c, &n("user_id"), "users", e.user_id.as_deref())? {
                    Some(u) => u,
                    None => match &me {
                        Some(u) => u.clone(),
                        None => {
                            let u = self.signed_in_user(&c).await?;
                            me = Some(u.clone());
                            u
                        }
                    },
                };
                slips.push(Value::Object(fields(vec![
                    ("task", Some(Value::from(c.url("tasks", &task_id)))),
                    ("project", Some(Value::from(project))),
                    ("user", Some(Value::from(user))),
                    (
                        "dated_on",
                        Some(Value::from(required_date(&n("dated_on"), &e.dated_on)?)),
                    ),
                    (
                        "hours",
                        Some(Value::from(decimal_arg(&n("hours"), &e.hours)?)),
                    ),
                    (
                        "comment",
                        opt_s(text_arg(&n("comment"), e.comment.as_deref())?),
                    ),
                ])));
            }
            let body = if slips.len() == 1 {
                json!({"timeslip": slips.pop()})
            } else {
                json!({"timeslips": slips})
            };
            let a = c
                .send(
                    Method::POST,
                    "create_timeslips",
                    &["timeslips"],
                    &[],
                    Some(&body),
                )
                .await?;
            let created = match a.body.get("timeslips") {
                Some(_) => view::items(&a.body, "timeslips"),
                None => vec![view::resource(view::unwrap(&a.body, "timeslip"))],
            };
            Ok(json_result(&json!({"created": created})))
        })
        .await
    }

    #[tool(
        description = "Change a timeslip's hours, date, comment or task. Billed timeslips cannot be changed.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_timeslip(
        &self,
        Parameters(p): Parameters<UpdateTimeslipParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_timeslip", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let body = fields(vec![
                ("hours", opt_s(opt_decimal("hours", p.hours.as_deref())?)),
                (
                    "dated_on",
                    opt_s(super::date_arg("dated_on", p.dated_on.as_deref())?),
                ),
                ("comment", opt_s(text_arg("comment", p.comment.as_deref())?)),
                (
                    "task",
                    opt_s(ref_arg(&c, "task_id", "tasks", p.task_id.as_deref())?),
                ),
            ]);
            if body.is_empty() {
                return Err(FaError::invalid("give at least one field to change"));
            }
            let out = c
                .write(
                    Method::PUT,
                    "update_timeslip",
                    &["timeslips", &id],
                    &[],
                    Some(&wrap("timeslip", body)),
                    "timeslip",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Delete a timeslip. Confirm with the person first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_timeslip(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("delete_timeslip", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            c.send(
                Method::DELETE,
                "delete_timeslip",
                &["timeslips", &id],
                &[],
                None,
            )
            .await?;
            Ok(json_result(&json!({"deleted": c.url("timeslips", &id)})))
        })
        .await
    }

    #[tool(
        description = "Start the timer on a timeslip (create one with log_time and hours 0 to time new work). Only one timer runs at a time.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn start_timer(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("start_timer", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let out = c
                .write(
                    Method::POST,
                    "start_timer",
                    &["timeslips", &id, "timer"],
                    &[],
                    None,
                    "timeslip",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Stop the timer on a timeslip; its hours then include the time timed.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn stop_timer(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("stop_timer", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let out = c
                .write(
                    Method::DELETE,
                    "stop_timer",
                    &["timeslips", &id, "timer"],
                    &[],
                    None,
                    "timeslip",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }
}
