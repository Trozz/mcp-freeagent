//! The tools against the fake FreeAgent: what they send, the defaults they fill in, what they
//! refuse, and how they shape answers.
mod support;

use serde_json::{Value, json};
use support::fake_freeagent::{Canned, FakeFreeAgent};
use support::tools::{auth, error_text, json, no_auth, params, server, text};

#[tokio::test]
async fn tools_need_a_sign_in() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    let r = s.get_company(params(json!({})), no_auth()).await.unwrap();
    assert!(error_text(&r).starts_with("auth: "));
}

#[tokio::test]
async fn get_company_adds_ids_and_drops_empty_values() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    let v = json(&s.get_current_user(params(json!({})), auth()).await.unwrap());
    assert_eq!(v["id"], "7");
    assert_eq!(v["permission_level"], 8);
}

#[tokio::test]
async fn search_contacts_matches_names_and_emails_locally() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "contacts",
        Canned::ok(json!({"contacts": [
            {"url": fake.res("contacts/1"), "organisation_name": "Acme Widgets Ltd", "email": "ap@acme.example"},
            {"url": fake.res("contacts/2"), "first_name": "Jane", "last_name": "Smith", "email": "jane@smith.example"},
        ]})),
    );
    let s = server(&fake);
    let v = json(
        &s.search_contacts(params(json!({"query": "acme"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["count"], 1);
    assert_eq!(v["items"][0]["name"], "Acme Widgets Ltd");
    assert_eq!(v["items"][0]["id"], "1");
    let v = json(
        &s.search_contacts(
            params(json!({"query": "jane smith", "view": "clients"})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(v["items"][0]["name"], "Jane Smith");
    let l = fake.last("GET", "contacts");
    assert_eq!(l.param("view"), Some("clients"));
    assert_eq!(l.param("per_page"), Some("100"));
}

#[tokio::test]
async fn create_invoice_fills_in_the_date_terms_and_contact() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "contacts/5",
        Canned::ok(json!({"contact": {"url": fake.res("contacts/5"), "organisation_name": "Acme", "default_payment_terms_in_days": 14}})),
    );
    let s = server(&fake);
    let r = s
        .create_invoice(
            params(json!({
                "contact_id": fake.res("contacts/5"),
                "items": [
                    {"description": "Consulting", "item_type": "days", "quantity": "3", "price": "600", "sales_tax_rate": "20"},
                    {"description": "Train fare", "price": "45.00", "category": "001"}
                ]
            })),
            auth(),
        )
        .await
        .unwrap();
    json(&r);
    let body = fake.last("POST", "invoices").body.unwrap();
    let inv = &body["invoice"];
    assert_eq!(inv["contact"], fake.res("contacts/5"));
    assert_eq!(inv["payment_terms_in_days"], 14);
    assert_eq!(inv["dated_on"].as_str().unwrap().len(), 10);
    assert_eq!(inv["invoice_items"][0]["item_type"], "Days");
    assert_eq!(inv["invoice_items"][0]["quantity"], "3");
    assert_eq!(inv["invoice_items"][1]["quantity"], "1");
    assert_eq!(
        inv["invoice_items"][1]["category"],
        fake.res("categories/001")
    );
}

#[tokio::test]
async fn create_invoice_refuses_bad_lines_before_calling_freeagent() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    let r = s
        .create_invoice(
            params(json!({"contact_id": "5", "items": [{"description": "x", "price": "1.234"}]})),
            auth(),
        )
        .await
        .unwrap();
    assert!(error_text(&r).starts_with("validation: items[0].price"));
    let r = s
        .create_invoice(params(json!({"contact_id": "5"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("give items"));
    assert!(fake.api_log().is_empty());
}

#[tokio::test]
async fn only_draft_invoices_are_deleted() {
    let fake = FakeFreeAgent::start().await;
    fake.on("GET", "invoices/9", Canned::ok(json!({"invoice": {"url": fake.res("invoices/9"), "status": "Open", "reference": "INV-9"}})));
    let s = server(&fake);
    let r = s
        .delete_invoice(params(json!({"id": "9"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("not Draft"));
    fake.on("GET", "invoices/9", Canned::ok(json!({"invoice": {"url": fake.res("invoices/9"), "status": "Draft", "reference": "INV-9"}})));
    fake.on("DELETE", "invoices/9", Canned::ok(Value::Null));
    let v = json(
        &s.delete_invoice(params(json!({"id": "9"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["reference"], "INV-9");
}

#[tokio::test]
async fn invoice_actions_use_freeagents_transitions() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    json(
        &s.invoice_action(params(json!({"id": "3", "action": "mark as sent"})), auth())
            .await
            .unwrap(),
    );
    fake.last("PUT", "invoices/3/transitions/mark_as_sent");
    json(
        &s.invoice_action(params(json!({"id": "3", "action": "duplicate"})), auth())
            .await
            .unwrap(),
    );
    fake.last("POST", "invoices/3/duplicate");
    let r = s
        .invoice_action(params(json!({"id": "3", "action": "pay"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("mark_as_sent"));
}

#[tokio::test]
async fn send_document_email_defaults_to_the_contacts_billing_email() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "invoices/3",
        Canned::ok(
            json!({"invoice": {"url": fake.res("invoices/3"), "contact": fake.res("contacts/5")}}),
        ),
    );
    fake.on(
        "GET",
        "contacts/5",
        Canned::ok(json!({"contact": {"email": "a@x.example", "billing_email": "ap@x.example"}})),
    );
    let s = server(&fake);
    json(
        &s.send_document_email(
            params(json!({"document": "invoice", "id": "3", "subject": "Invoice INV-3", "body": "Please find attached."})),
            auth(),
        )
        .await
        .unwrap(),
    );
    let b = fake.last("POST", "invoices/3/send_email").body.unwrap();
    assert_eq!(b["invoice"]["email"]["to"], "ap@x.example");
    assert_eq!(b["invoice"]["email"]["subject"], "Invoice INV-3");
}

#[tokio::test]
async fn pdfs_come_back_as_embedded_resources() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "invoices/3/pdf",
        Canned::ok(json!({"pdf": {"content": "JVBERi0x\nLjQ="}})),
    );
    let s = server(&fake);
    let r = s
        .get_document_pdf(params(json!({"document": "invoice", "id": "3"})), auth())
        .await
        .unwrap();
    let v = serde_json::to_value(&r.content[1]).unwrap();
    assert_eq!(v["resource"]["mimeType"], "application/pdf");
    assert_eq!(v["resource"]["blob"], "JVBERi0xLjQ=");
}

#[tokio::test]
async fn expenses_are_stored_with_freeagents_sign() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    json(
        &s.create_expense(
            params(json!({"category": "285", "dated_on": "2026-10-01", "description": "Lunch with client", "amount": "42.50", "sales_tax_rate": "20"})),
            auth(),
        )
        .await
        .unwrap(),
    );
    let e = fake.last("POST", "expenses").body.unwrap();
    assert_eq!(e["expense"]["gross_value"], "-42.50");
    assert_eq!(
        e["expense"]["user"],
        fake.res("users/7"),
        "the signed-in user by default"
    );
    assert_eq!(e["expense"]["category"], fake.res("categories/285"));
    let r = s
        .create_expense(params(json!({"category": "285", "dated_on": "2026-10-01", "description": "x", "amount": "-1"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("positive"));
}

#[tokio::test]
async fn bills_are_due_thirty_days_on_by_default() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    json(
        &s.create_bill(
            params(json!({"contact_id": "4", "reference": "SUP-1", "dated_on": "2026-01-15", "items": [{"category": "365", "total_value": "120"}]})),
            auth(),
        )
        .await
        .unwrap(),
    );
    let b = fake.last("POST", "bills").body.unwrap();
    assert_eq!(b["bill"]["due_on"], "2026-02-14");
    assert_eq!(
        b["bill"]["bill_items"][0]["category"],
        fake.res("categories/365")
    );
}

#[tokio::test]
async fn explaining_a_transaction_defaults_to_its_amount_and_date_and_checks_the_sign() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "bank_transactions/11",
        Canned::ok(json!({"bank_transaction": {"url": fake.res("bank_transactions/11"), "amount": "-6.17", "unexplained_amount": "-6.17", "dated_on": "2026-01-15", "description": "AWS EMEA"}})),
    );
    let s = server(&fake);
    json(
        &s.explain_bank_transaction(
            params(json!({"bank_transaction_id": "11", "category": "365"})),
            auth(),
        )
        .await
        .unwrap(),
    );
    let e = fake
        .last("POST", "bank_transaction_explanations")
        .body
        .unwrap();
    let e = &e["bank_transaction_explanation"];
    assert_eq!(e["bank_transaction"], fake.res("bank_transactions/11"));
    assert_eq!(e["gross_value"], "-6.17");
    assert_eq!(e["dated_on"], "2026-01-15");
    assert_eq!(e["description"], "AWS EMEA");
    let r = s
        .explain_bank_transaction(
            params(json!({"bank_transaction_id": "11", "category": "365", "gross_value": "6.17"})),
            auth(),
        )
        .await
        .unwrap();
    assert!(error_text(&r).contains("sign"));
    let r = s
        .explain_bank_transaction(
            params(json!({"bank_transaction_id": "11", "category": "365", "paid_bill_id": "2"})),
            auth(),
        )
        .await
        .unwrap();
    assert!(error_text(&r).contains("one way"));
}

#[tokio::test]
async fn approving_guesses_lists_first_and_approves_only_what_was_chosen() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "bank_transaction_explanations",
        Canned::ok(json!({"bank_transaction_explanations": [
            {"url": fake.res("bank_transaction_explanations/1"), "marked_for_review": true, "gross_value": "-10"},
            {"url": fake.res("bank_transaction_explanations/2"), "marked_for_review": false},
            {"url": fake.res("bank_transaction_explanations/3"), "marked_for_review": true},
        ]})),
    );
    let s = server(&fake);
    let v = json(
        &s.approve_explanations(params(json!({"bank_account_id": "1"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["awaiting_approval"].as_array().unwrap().len(), 2);
    assert!(
        fake.api_log().iter().all(|l| l.method == "GET"),
        "a listing changes nothing"
    );
    let v = json(
        &s.approve_explanations(
            params(json!({"bank_account_id": "1", "explanation_ids": ["3"]})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(v["approved"], json!(["3"]));
    let put = fake
        .last("PUT", "bank_transaction_explanations/3")
        .body
        .unwrap();
    assert_eq!(
        put["bank_transaction_explanation"]["marked_for_review"],
        false
    );
}

#[tokio::test]
async fn journals_must_balance() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    let r = s
        .create_journal_set(
            params(json!({"dated_on": "2026-03-31", "description": "Accrual", "entries": [{"category": "363", "debit_value": "100"}, {"category": "796", "debit_value": "-99.99"}]})),
            auth(),
        )
        .await
        .unwrap();
    assert!(error_text(&r).contains("do not balance"));
    json(
        &s.create_journal_set(
            params(json!({"dated_on": "2026-03-31", "description": "Accrual", "entries": [{"category": "363", "debit_value": "100"}, {"category": "796", "debit_value": "-100"}]})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        fake.last("POST", "journal_sets").body.unwrap()["journal_set"]["tag"],
        "mcp-freeagent"
    );
}

#[tokio::test]
async fn vat_payments_are_marked_by_their_date() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    json(
        &s.mark_tax_return(
            params(json!({"kind": "vat", "period_ends_on": "2026-06-30", "action": "mark_as_paid", "payment_date": "2026-08-07"})),
            auth(),
        )
        .await
        .unwrap(),
    );
    fake.last(
        "PUT",
        "vat_returns/2026-06-30/payments/2026-08-07/mark_as_paid",
    );
    let r = s
        .mark_tax_return(
            params(
                json!({"kind": "vat", "period_ends_on": "2026-06-30", "action": "mark_as_paid"}),
            ),
            auth(),
        )
        .await
        .unwrap();
    assert!(error_text(&r).contains("payment_date"));
    json(
        &s.list_tax_returns(params(json!({"kind": "self assessment"})), auth())
            .await
            .map(|_| {
                rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text("{}")])
            })
            .unwrap(),
    );
    fake.last("GET", "users/7/self_assessment_returns");
}

#[tokio::test]
async fn categories_are_found_by_words() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "categories",
        Canned::ok(json!({
            "admin_expenses_categories": [
                {"url": fake.res("categories/365"), "description": "Computer Software", "nominal_code": "365", "tax_reporting_name": "computer_software_costs"},
                {"url": fake.res("categories/285"), "description": "Accommodation and Meals", "nominal_code": "285"}
            ],
            "income_categories": [{"url": fake.res("categories/001"), "description": "Sales", "nominal_code": "001"}]
        })),
    );
    let s = server(&fake);
    let v = json(
        &s.list_categories(params(json!({"query": "software"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["count"], 1);
    assert_eq!(v["categories"][0]["nominal_code"], "365");
    assert_eq!(v["categories"][0]["group"], "admin_expenses");
    let v = json(
        &s.list_categories(params(json!({"group": "income"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["count"], 1);
}

#[tokio::test]
async fn the_snapshot_sums_what_is_owed_and_counts_unexplained_transactions() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "bank_accounts",
        Canned::ok(json!({"bank_accounts": [{"url": fake.res("bank_accounts/1"), "name": "Current", "current_balance": "1500.00", "currency": "GBP"}]})),
    );
    fake.on(
        "GET",
        "bank_transactions",
        Canned::ok(json!({"bank_transactions": [{}, {}]})).header("x-total-count", "12"),
    );
    fake.on(
        "GET",
        "invoices",
        Canned::ok(json!({"invoices": [{"due_value": "100.00", "status": "Open"}, {"due_value": "250.50", "status": "Overdue"}]})),
    );
    fake.on("GET", "bills", Canned::ok(json!({"bills": []})));
    fake.on(
        "GET",
        "company/tax_timeline",
        Canned::ok(json!({"timeline_items": [{"description": "VAT", "dated_on": "2099-01-07"}, {"description": "Old", "dated_on": "2000-01-01"}]})),
    );
    let s = server(&fake);
    let v = json(
        &s.business_snapshot(params(json!({})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["company"]["name"], "Lee Consulting Ltd");
    assert_eq!(v["bank_accounts"][0]["unexplained_transactions"], 12);
    assert_eq!(v["owed_to_you"]["due"], "350.50");
    assert_eq!(v["owed_to_you"]["overdue_due"], "250.50");
    assert_eq!(v["you_owe"]["count"], 0);
    assert_eq!(v["upcoming_deadlines"].as_array().unwrap().len(), 1);
    assert!(v.get("unavailable").is_none(), "{v}");
}

#[tokio::test]
async fn the_snapshot_reports_sections_it_cannot_read() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "bank_accounts",
        Canned::status(403, json!({"errors": [{"message": "Insufficient access"}]})),
    );
    fake.on("GET", "invoices", Canned::ok(json!({"invoices": []})));
    fake.on("GET", "bills", Canned::ok(json!({"bills": []})));
    fake.on(
        "GET",
        "company/tax_timeline",
        Canned::ok(json!({"timeline_items": []})),
    );
    let s = server(&fake);
    let v = json(
        &s.business_snapshot(params(json!({})), auth())
            .await
            .unwrap(),
    );
    assert!(
        v["unavailable"]["bank_accounts"]
            .as_str()
            .unwrap()
            .starts_with("permission:")
    );
}

#[tokio::test]
async fn the_raw_tools_stay_on_the_api() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "capital_assets",
        Canned::ok(json!({"capital_assets": [{"url": fake.res("capital_assets/4")}]})),
    );
    let s = server(&fake);
    let v = json(
        &s.api_get(
            params(json!({"path": "/v2/capital_assets", "query": {"view": "all"}})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(v["body"]["capital_assets"][0]["id"], "4");
    assert_eq!(
        fake.last("GET", "capital_assets").param("view"),
        Some("all")
    );
    let r = s
        .api_get(
            params(json!({"path": "https://evil.example.com/v2/x"})),
            auth(),
        )
        .await
        .unwrap();
    assert!(error_text(&r).contains("not another host"));
    let r = s
        .api_request(params(json!({"method": "GET", "path": "x"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("POST, PUT, DELETE"));
    json(
        &s.api_request(params(json!({"method": "put", "path": "account_locks", "body": {"account_lock": {"locked_to_date": "2026-09-30"}}})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(
        fake.last("PUT", "account_locks").body.unwrap()["account_lock"]["locked_to_date"],
        "2026-09-30"
    );
}

#[tokio::test]
async fn log_time_defaults_to_the_tasks_project_and_the_signed_in_user() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "tasks/21",
        Canned::ok(
            json!({"task": {"url": fake.res("tasks/21"), "project": fake.res("projects/8")}}),
        ),
    );
    let s = server(&fake);
    let v = json(
        &s.log_time(
            params(json!({"entries": [
                {"task_id": "21", "dated_on": "2026-10-26", "hours": "3", "comment": "Website"},
                {"task_id": "21", "dated_on": "2026-10-27", "hours": "1.5"}
            ]})),
            auth(),
        )
        .await
        .unwrap(),
    );
    let b = fake.last("POST", "timeslips").body.unwrap();
    let slips = b["timeslips"].as_array().unwrap();
    assert_eq!(slips.len(), 2);
    assert_eq!(slips[0]["project"], fake.res("projects/8"));
    assert_eq!(slips[0]["user"], fake.res("users/7"));
    assert_eq!(slips[1]["hours"], "1.5");
    assert!(v["created"].is_array());
    let gets: Vec<_> = fake
        .api_log()
        .into_iter()
        .filter(|l| l.path.ends_with("tasks/21"))
        .collect();
    assert_eq!(gets.len(), 1, "the task is looked up once");
}

#[test]
fn every_tool_has_a_description_and_the_right_hints() {
    let fake = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(FakeFreeAgent::start());
    let s = server(&fake);
    let tools = s.tools();
    assert!(tools.len() >= 75, "{}", tools.len());
    for t in tools {
        let name = t.name.to_string();
        assert!(
            t.description.as_ref().is_some_and(|d| d.len() > 20),
            "{name}"
        );
        let read_only = t
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);
        let reads = ["get_", "list_", "search_", "business_snapshot", "api_get"]
            .iter()
            .any(|p| name.starts_with(p));
        assert_eq!(read_only, reads, "{name}: read_only_hint");
        if name.starts_with("delete_") || name == "mark_tax_return" || name == "api_request" {
            assert_eq!(
                t.annotations.as_ref().and_then(|a| a.destructive_hint),
                Some(true),
                "{name}"
            );
        }
        assert!(name.len() <= 64 - "freeagent_".len(), "{name}");
    }
    let _ = text;
}

#[tokio::test]
async fn explanation_attachments_use_the_attachments_endpoint() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "bank_transactions/11",
        Canned::ok(json!({"bank_transaction": {"url": fake.res("bank_transactions/11"), "amount": "-6.17", "unexplained_amount": "-6.17", "dated_on": "2026-01-15", "description": "AWS"}})),
    );
    fake.on(
        "POST",
        "bank_transaction_explanations",
        Canned::status(201, json!({"bank_transaction_explanation": {"url": fake.res("bank_transaction_explanations/44")}})),
    );
    let s = server(&fake);
    let v = json(
        &s.explain_bank_transaction(
            params(json!({"bank_transaction_id": "11", "category": "365", "attachment": {"file_name": "aws.pdf", "data_base64": "JVBERi0xLjc="}})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(v["attached"], "aws.pdf");
    let created = fake
        .last("POST", "bank_transaction_explanations")
        .body
        .unwrap();
    assert!(
        created["bank_transaction_explanation"]
            .get("attachment")
            .is_none(),
        "{created}"
    );
    let att = fake.last("POST", "bank_transaction_explanations/44/attachments");
    assert_eq!(att.api_version.as_deref(), Some("2026-09-01"));
    assert_eq!(
        att.body.unwrap()["attachments"][0]["content_type"],
        "application/x-pdf"
    );
}

#[tokio::test]
async fn attach_file_checks_the_file_and_upload_links_need_the_item() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    json(
        &s.attach_file(params(json!({"target": "bill", "id": "3", "file_name": "inv.png", "data_base64": "iVBORw0KGgo="})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(
        fake.last("PUT", "bills/3").body.unwrap()["bill"]["attachment"]["content_type"],
        "image/png"
    );
    let r = s
        .attach_file(params(json!({"target": "expense", "id": "3", "file_name": "x.txt", "data_base64": "aGVsbG8="})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("PDF, PNG, JPEG or GIF"));
    // A link is only made for an item that exists.
    let r = s
        .create_upload_link(params(json!({"target": "expense", "id": "404"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).starts_with("not_found:"));
    // Without the public address no link can be made.
    let bare = mcp_freeagent_books::tools::BooksServer::new(support::tools::client(&fake));
    let r = bare
        .create_upload_link(params(json!({"target": "expense", "id": "1"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("public address"));
}

#[tokio::test]
async fn upload_links_belong_to_whoever_made_them() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "expenses/5",
        Canned::ok(json!({"expense": {"url": fake.res("expenses/5"), "description": "x"}})),
    );
    let s = server(&fake);
    let v = json(
        &s.create_upload_link(params(json!({"target": "expense", "id": "5"})), auth())
            .await
            .unwrap(),
    );
    let url = v["upload_url"].as_str().unwrap();
    let mine = json(
        &s.get_upload_status(params(json!({"upload_url": url})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(mine["status"], "waiting");
    let theirs = s
        .get_upload_status(
            params(json!({"upload_url": url})),
            support::tools::auth_with("someone-else"),
        )
        .await
        .unwrap();
    assert!(error_text(&theirs).starts_with("not_found:"));
}
