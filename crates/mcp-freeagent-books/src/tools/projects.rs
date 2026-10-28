//! Projects and their tasks, which time is logged against.

use super::contacts::IdParams;
use super::{
    BooksServer, Query, choice, currency_arg, date_arg, fields, id_arg, json_result, merge,
    opt_decimal, opt_money, opt_s, paging, push, ref_arg, required_ref, required_text, run,
    since_arg, text_arg, wrap,
};
use crate::error::FaError;
use axum::http::request::Parts;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListProjectsParams {
    /// active (default), completed, cancelled or hidden.
    pub view: Option<String>,
    /// Only this contact's projects.
    pub contact_id: Option<String>,
    pub page: Option<u32>,
    /// 1 to 100. Default: 25.
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ProjectFields {
    pub name: Option<String>,
    /// Active, Completed, Cancelled or Hidden.
    pub status: Option<String>,
    /// Hours, Days or Monetary: what budget counts.
    pub budget_units: Option<String>,
    /// The budget in budget_units.
    pub budget: Option<String>,
    /// Default rate for the project's tasks, e.g. 450.
    pub normal_billing_rate: Option<String>,
    /// hour or day: what normal_billing_rate is per.
    pub billing_period: Option<String>,
    /// Hours in a working day, for day rates, e.g. 7.5.
    pub hours_per_day: Option<String>,
    /// ISO 4217 code, e.g. GBP.
    pub currency: Option<String>,
    pub starts_on: Option<String>,
    pub ends_on: Option<String>,
    pub contract_po_reference: Option<String>,
    /// Any other project field by FreeAgent's name, e.g. {"is_ir35": true}.
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateProjectParams {
    /// The client the project is for.
    pub contact_id: String,
    #[serde(flatten)]
    pub fields: ProjectFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateProjectParams {
    pub id: String,
    #[serde(flatten)]
    pub fields: ProjectFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListTasksParams {
    /// Only this project's tasks.
    pub project_id: Option<String>,
    /// all (default), active, completed or hidden.
    pub view: Option<String>,
    pub updated_since: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct TaskFields {
    pub name: Option<String>,
    /// Whether time on it is billed. Default: true.
    pub is_billable: Option<bool>,
    /// Rate for time on this task, e.g. 60.
    pub billing_rate: Option<String>,
    /// hour or day.
    pub billing_period: Option<String>,
    /// Active, Completed or Hidden.
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateTaskParams {
    /// The project the task belongs to.
    pub project_id: String,
    #[serde(flatten)]
    pub fields: TaskFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateTaskParams {
    pub id: String,
    #[serde(flatten)]
    pub fields: TaskFields,
}

fn project_body(f: &ProjectFields) -> crate::error::Result<Map<String, Value>> {
    let typed = fields(vec![
        ("name", opt_s(text_arg("name", f.name.as_deref())?)),
        (
            "status",
            choice(
                "status",
                f.status.as_deref(),
                &["Active", "Completed", "Cancelled", "Hidden"],
            )?
            .map(Value::from),
        ),
        (
            "budget_units",
            choice(
                "budget_units",
                f.budget_units.as_deref(),
                &["Hours", "Days", "Monetary"],
            )?
            .map(Value::from),
        ),
        ("budget", opt_s(opt_decimal("budget", f.budget.as_deref())?)),
        (
            "normal_billing_rate",
            opt_s(opt_money(
                "normal_billing_rate",
                f.normal_billing_rate.as_deref(),
            )?),
        ),
        (
            "billing_period",
            choice(
                "billing_period",
                f.billing_period.as_deref(),
                &["hour", "day"],
            )?
            .map(Value::from),
        ),
        (
            "hours_per_day",
            opt_s(opt_decimal("hours_per_day", f.hours_per_day.as_deref())?),
        ),
        ("currency", opt_s(currency_arg(f.currency.as_deref())?)),
        (
            "starts_on",
            opt_s(date_arg("starts_on", f.starts_on.as_deref())?),
        ),
        ("ends_on", opt_s(date_arg("ends_on", f.ends_on.as_deref())?)),
        (
            "contract_po_reference",
            opt_s(text_arg(
                "contract_po_reference",
                f.contract_po_reference.as_deref(),
            )?),
        ),
    ]);
    merge(f.extra.as_ref(), typed)
}

fn task_body(f: &TaskFields) -> crate::error::Result<Map<String, Value>> {
    Ok(fields(vec![
        ("name", opt_s(text_arg("name", f.name.as_deref())?)),
        ("is_billable", f.is_billable.map(Value::from)),
        (
            "billing_rate",
            opt_s(opt_money("billing_rate", f.billing_rate.as_deref())?),
        ),
        (
            "billing_period",
            choice(
                "billing_period",
                f.billing_period.as_deref(),
                &["hour", "day"],
            )?
            .map(Value::from),
        ),
        (
            "status",
            choice(
                "status",
                f.status.as_deref(),
                &["Active", "Completed", "Hidden"],
            )?
            .map(Value::from),
        ),
    ]))
}

#[tool_router(router = projects_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "List projects with their client (contact), status, budget, billing rate and currency.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_projects(
        &self,
        Parameters(p): Parameters<ListProjectsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_projects", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                choice(
                    "view",
                    p.view.as_deref(),
                    &["active", "completed", "cancelled", "hidden"],
                )?
                .map(String::from),
            );
            push(
                &mut q,
                "contact",
                ref_arg(&c, "contact_id", "contacts", p.contact_id.as_deref())?,
            );
            Ok(json_result(
                &c.list("projects", &["projects"], "projects", q, pg).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One project in full: client, status, budget, billing rate, dates and currency.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_project(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_project", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one("project", &["projects", &id], "project", &[]).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Create a project for a client. name, budget_units and currency are needed (status defaults to Active, budget_units to Hours, currency to the company's).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_project(
        &self,
        Parameters(p): Parameters<CreateProjectParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_project", async {
            let c = self.caller(&parts)?;
            let mut body = project_body(&p.fields)?;
            if !body.contains_key("name") {
                return Err(FaError::invalid("name is required"));
            }
            body.insert(
                "contact".into(),
                Value::from(required_ref(&c, "contact_id", "contacts", &p.contact_id)?),
            );
            body.entry("status")
                .or_insert_with(|| Value::from("Active"));
            body.entry("budget_units")
                .or_insert_with(|| Value::from("Hours"));
            if !body.contains_key("currency") {
                let co = c.one("company", &["company"], "company", &[]).await?;
                if let Some(cur) = co.get("currency") {
                    body.insert("currency".into(), cur.clone());
                }
            }
            let out = c
                .write(
                    Method::POST,
                    "create_project",
                    &["projects"],
                    &[],
                    Some(&wrap("project", body)),
                    "project",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Change a project: only the fields given change. Set status Completed to close it.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_project(
        &self,
        Parameters(p): Parameters<UpdateProjectParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_project", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let body = project_body(&p.fields)?;
            if body.is_empty() {
                return Err(FaError::invalid("give at least one field to change"));
            }
            let out = c
                .write(
                    Method::PUT,
                    "update_project",
                    &["projects", &id],
                    &[],
                    Some(&wrap("project", body)),
                    "project",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "List tasks (what time is logged against), optionally for one project, with billing rate and whether they are billable.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_tasks(
        &self,
        Parameters(p): Parameters<ListTasksParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_tasks", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "project",
                ref_arg(&c, "project_id", "projects", p.project_id.as_deref())?,
            );
            push(
                &mut q,
                "view",
                choice(
                    "view",
                    p.view.as_deref(),
                    &["all", "active", "completed", "hidden"],
                )?
                .map(String::from),
            );
            push(
                &mut q,
                "updated_since",
                since_arg(p.updated_since.as_deref())?,
            );
            Ok(json_result(
                &c.list("tasks", &["tasks"], "tasks", q, pg).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Create a task on a project, e.g. Development or Meetings, so time can be logged against it.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_task(
        &self,
        Parameters(p): Parameters<CreateTaskParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_task", async {
            let c = self.caller(&parts)?;
            let project = required_ref(&c, "project_id", "projects", &p.project_id)?;
            let mut body = task_body(&p.fields)?;
            body.insert(
                "name".into(),
                Value::from(required_text(
                    "name",
                    p.fields.name.as_deref().unwrap_or(""),
                )?),
            );
            let out = c
                .write(
                    Method::POST,
                    "create_task",
                    &["tasks"],
                    &[("project", project)],
                    Some(&wrap("task", body)),
                    "task",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Change a task: only the fields given change.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_task(
        &self,
        Parameters(p): Parameters<UpdateTaskParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_task", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let body = task_body(&p.fields)?;
            if body.is_empty() {
                return Err(FaError::invalid("give at least one field to change"));
            }
            let out = c
                .write(
                    Method::PUT,
                    "update_task",
                    &["tasks", &id],
                    &[],
                    Some(&wrap("task", body)),
                    "task",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }
}
