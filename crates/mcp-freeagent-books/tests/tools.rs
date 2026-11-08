//! The tools against the fake FreeAgent: what they send, the defaults they fill in, what they
//! refuse, and how they shape answers.
mod support;

use serde_json::{Value, json};
use support::fake_freeagent::{Canned, FakeFreeAgent};
use support::tools::{auth, error_text, is_error, json, no_auth, params, server, text};

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
    // FreeAgent's marked_for_review view of transactions, with the guesses nested; a
    // transaction can also hold an explanation that is already approved.
    fake.on(
        "GET",
        "bank_transactions",
        Canned::ok(json!({"bank_transactions": [
            {"url": fake.res("bank_transactions/10"), "bank_transaction_explanations": [
                {"url": fake.res("bank_transaction_explanations/1"), "marked_for_review": true, "gross_value": "-10"},
                {"url": fake.res("bank_transaction_explanations/2"), "marked_for_review": false},
            ]},
            {"url": fake.res("bank_transactions/11"), "bank_transaction_explanations": [
                {"url": fake.res("bank_transaction_explanations/3"), "marked_for_review": true},
            ]},
        ]})),
    );
    let s = server(&fake);
    let v = json(
        &s.approve_explanations(params(json!({"bank_account_id": "1"})), auth())
            .await
            .unwrap(),
    );
    let pending = v["awaiting_approval"].as_array().unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[1]["id"], "3");
    let l = fake.last("GET", "bank_transactions");
    assert_eq!(l.param("view"), Some("marked_for_review"));
    assert!(
        fake.api_log().iter().all(|l| l.method == "GET"),
        "a listing changes nothing"
    );
    let v = json(
        &s.list_bank_transaction_explanations(
            params(json!({"bank_account_id": "1", "marked_for_review_only": true})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(v["count"], 2);
    assert_eq!(v["items"][0]["id"], "1");
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
async fn pages_follow_freeagents_total_when_the_link_header_names_no_next_page() {
    // Live FreeAgent gave x-total-count but no next page we could read: list tools showed no
    // next_page, and tools reading every page stopped after the first.
    let fake = FakeFreeAgent::start().await;
    let contact = |n: u32| json!({"url": fake.res(&format!("contacts/{n}")), "organisation_name": format!("Supplier {n}")});
    let first: Vec<Value> = (1..=100).map(contact).collect();
    let second: Vec<Value> =
        vec![json!({"url": fake.res("contacts/101"), "organisation_name": "Acme Widgets Ltd"})];
    fake.on_seq(
        "GET",
        "contacts",
        vec![
            Canned::ok(json!({"contacts": first})).header("x-total-count", "101"),
            Canned::ok(json!({"contacts": second})).header("x-total-count", "101"),
        ],
    );
    let s = server(&fake);
    let v = json(
        &s.search_contacts(params(json!({"query": "acme"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["count"], 1, "the second page was not read: {v}");
    assert_eq!(v["items"][0]["id"], "101");

    fake.on(
        "GET",
        "invoices",
        Canned::ok(json!({"invoices": [{"url": fake.res("invoices/1")}]}))
            .header("x-total-count", "155"),
    );
    let v = json(
        &s.list_invoices(params(json!({"per_page": 1})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["next_page"], 2);
    let v = json(
        &s.list_invoices(params(json!({"per_page": 1, "page": 155})), auth())
            .await
            .unwrap(),
    );
    assert!(v.get("next_page").is_none(), "{v}");
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
        let reads = [
            "get_",
            "list_",
            "search_",
            "business_snapshot",
            "api_get",
            "month_end_check",
            "director_loan_review",
            "books_health_check",
            "suggest_explanations",
            "match_receipt",
        ]
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

#[tokio::test]
async fn expense_lists_leave_out_receipt_download_links() {
    // FreeAgent signs three ~2,000 character download links into every attachment, valid for
    // 30 seconds. Fifty expenses with receipts came to over 200,000 characters, too much for a
    // tool result.
    let fake = FakeFreeAgent::start().await;
    let link = format!(
        "https://s3.example/receipt.jpg?X-Amz-Signature={}",
        "a".repeat(2000)
    );
    let expense = |n: u32| {
        json!({
            "url": fake.res(&format!("expenses/{n}")),
            "description": "Breakfast",
            "gross_value": "-4.5",
            "attachment": {
                "url": fake.res(&format!("attachments/{n}")),
                "content_src": link,
                "content_src_medium": link,
                "content_src_small": link,
                "content_type": "image/jpeg",
                "expires_at": "2026-11-01T20:29:40.000Z",
                "file_name": "receipt.jpg",
                "file_size": 531406
            }
        })
    };
    let expenses: Vec<Value> = (1..=50).map(expense).collect();
    fake.on("GET", "expenses", Canned::ok(json!({"expenses": expenses})));
    let s = server(&fake);
    let r = s
        .list_expenses(params(json!({"per_page": 50})), auth())
        .await
        .unwrap();
    let t = text(&r);
    assert!(!t.contains("X-Amz"), "signed links left in the result");
    assert!(
        t.len() < 40_000,
        "50 expenses came to {} characters",
        t.len()
    );
    let v = json(&r);
    assert_eq!(v["count"], 50);
    let a = &v["items"][0]["attachment"];
    assert_eq!(a["id"], "1");
    assert_eq!(a["file_name"], "receipt.jpg");
    assert_eq!(a["content_type"], "image/jpeg");
    assert!(a.get("content_src").is_none() && a.get("expires_at").is_none());
}

#[tokio::test]
async fn the_snapshot_uses_freeagents_own_counts_for_every_visible_account() {
    // Hidden accounts first, then more visible accounts than the snapshot would count by hand:
    // every visible account still gets its counts, and no transactions are read.
    let fake = FakeFreeAgent::start().await;
    let mut accounts: Vec<Value> = (1..=4)
        .map(|n| json!({"url": fake.res(&format!("bank_accounts/{n}")), "name": format!("Closed {n}"), "status": "hidden", "unexplained_transaction_count": 0}))
        .collect();
    accounts.extend((5..=16).map(|n| {
        json!({"url": fake.res(&format!("bank_accounts/{n}")), "name": format!("Account {n}"), "status": "active", "unexplained_transaction_count": n, "marked_for_review_count": 1})
    }));
    fake.on(
        "GET",
        "bank_accounts",
        Canned::ok(json!({"bank_accounts": accounts})),
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
    let banks = v["bank_accounts"].as_array().unwrap();
    assert_eq!(banks.len(), 12);
    assert_eq!(banks[11]["name"], "Account 16");
    assert_eq!(banks[11]["unexplained_transactions"], 16);
    assert_eq!(banks[11]["explanations_to_approve"], 1);
    assert!(
        fake.api_log().iter().all(|l| l.path != "bank_transactions"),
        "transactions were read"
    );
}

#[tokio::test]
async fn a_page_too_large_for_a_tool_result_says_what_per_page_fits() {
    let fake = FakeFreeAgent::start().await;
    let long = "x".repeat(2_000);
    let invoices: Vec<Value> = (1..=100)
        .map(|n| json!({"url": fake.res(&format!("invoices/{n}")), "comments": long}))
        .collect();
    fake.on("GET", "invoices", Canned::ok(json!({"invoices": invoices})));
    let s = server(&fake);
    let r = s
        .list_invoices(params(json!({"per_page": 100})), auth())
        .await
        .unwrap();
    assert!(is_error(&r));
    let t = error_text(&r);
    assert!(t.starts_with("too_large:"), "{t}");
    let fits: usize = t
        .split("per_page ")
        .nth(1)
        .and_then(|r| r.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap();
    // About 2,050 characters an invoice: the suggestion must fit, with room to spare.
    assert!((10..=39).contains(&fits), "{t}");
}

#[tokio::test]
async fn an_upload_link_says_when_it_would_replace_a_receipt() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "expenses/6",
        Canned::ok(json!({"expense": {
            "url": fake.res("expenses/6"),
            "description": "Breakfast ",
            "dated_on": "2026-09-28",
            "gross_value": "-4.5",
            "currency": "GBP",
            "attachment": {"url": fake.res("attachments/9"), "file_name": "old.jpg"}
        }})),
    );
    fake.on(
        "GET",
        "expenses/7",
        Canned::ok(json!({"expense": {"url": fake.res("expenses/7"), "description": "Lunch"}})),
    );
    let s = server(&fake);
    let v = json(
        &s.create_upload_link(params(json!({"target": "expense", "id": "6"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(v["replaces"], "old.jpg");
    assert!(
        v["tell_the_person"]
            .as_str()
            .unwrap()
            .contains("replaces it")
    );
    assert_eq!(v["for"]["item"], "Breakfast, 2026-09-28, 4.50 GBP");
    let v = json(
        &s.create_upload_link(params(json!({"target": "expense", "id": "7"})), auth())
            .await
            .unwrap(),
    );
    assert!(v.get("replaces").is_none());
}

#[tokio::test]
async fn editing_an_invoice_line_keeps_what_the_edit_leaves_out() {
    // Live: changing a line's description and price cleared its unit, and every edit of an
    // existing line was sent with quantity 1.
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "invoices/9",
        Canned::ok(json!({"invoice": {"url": fake.res("invoices/9"), "invoice_items": [
            {"url": fake.res("invoice_items/71"), "description": "Consulting", "item_type": "Hours", "quantity": "7.5", "price": "80.0", "sales_tax_rate": "20.0", "category": fake.res("categories/001")},
            {"url": fake.res("invoice_items/72"), "description": "Old line", "quantity": "1.0", "price": "5.0"}
        ]}})),
    );
    fake.on(
        "PUT",
        "invoices/9",
        Canned::ok(json!({"invoice": {"url": fake.res("invoices/9")}})),
    );
    let s = server(&fake);
    let r = s
        .update_invoice(
            params(json!({"id": "9", "items": [
                {"id": "71", "description": "Consulting, October"},
                {"id": "72", "delete": true},
                {"description": "New line", "price": "10"}
            ]})),
            auth(),
        )
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let body = fake.last("PUT", "invoices/9").body.unwrap();
    let items = body["invoice"]["invoice_items"].as_array().unwrap();
    assert_eq!(items[0]["id"], "71");
    assert_eq!(items[0]["description"], "Consulting, October");
    assert_eq!(items[0]["item_type"], "Hours");
    assert_eq!(items[0]["quantity"], "7.5");
    assert_eq!(items[0]["price"], "80.0");
    assert_eq!(items[0]["sales_tax_rate"], "20.0");
    assert_eq!(items[1], json!({"id": "72", "_destroy": 1}));
    assert_eq!(items[2]["quantity"], "1");
    assert!(items[2].get("id").is_none());

    let r = s
        .update_invoice(
            params(json!({"id": "9", "items": [{"price": "10"}]})),
            auth(),
        )
        .await
        .unwrap();
    assert!(
        error_text(&r).contains("description is required"),
        "{}",
        error_text(&r)
    );
}

#[tokio::test]
async fn estimate_lines_without_a_vat_rate_get_the_companys_rate() {
    // Live: FreeAgent stored -1 as the rate of an estimate line given none, so a 10.00
    // estimate came to 9.90.
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "company",
        Canned::ok(json!({"company": {"url": fake.res("company"), "currency": "GBP", "sales_tax_registration_status": "Registered", "sales_tax_rates": ["20.0", "5.0", "0.0"]}})),
    );
    fake.on(
        "GET",
        "contacts/5",
        Canned::ok(json!({"contact": {"url": fake.res("contacts/5"), "charge_sales_tax": "Auto"}})),
    );
    fake.on(
        "GET",
        "contacts/6",
        Canned::ok(
            json!({"contact": {"url": fake.res("contacts/6"), "charge_sales_tax": "Never"}}),
        ),
    );
    fake.on(
        "POST",
        "estimates",
        Canned::ok(json!({"estimate": {"url": fake.res("estimates/1")}})),
    );
    let s = server(&fake);
    s.create_estimate(
        params(json!({"contact_id": "5", "reference": "EST-1", "items": [
            {"description": "Work", "price": "5", "quantity": "2"},
            {"description": "Zero rated", "price": "1", "sales_tax_rate": "0"}
        ]})),
        auth(),
    )
    .await
    .unwrap();
    let body = fake.last("POST", "estimates").body.unwrap();
    let items = body["estimate"]["estimate_items"].as_array().unwrap();
    assert_eq!(items[0]["sales_tax_rate"], "20.0");
    assert_eq!(items[1]["sales_tax_rate"], "0");
    s.create_estimate(
        params(json!({"contact_id": "6", "reference": "EST-2", "items": [{"description": "Work", "price": "5"}]})),
        auth(),
    )
    .await
    .unwrap();
    let body = fake.last("POST", "estimates").body.unwrap();
    assert_eq!(body["estimate"]["estimate_items"][0]["sales_tax_rate"], "0");
}

#[tokio::test]
async fn mileage_is_claimed_under_the_mileage_category() {
    // Live: FreeAgent refused the claim with "Invalid nominal code Mileage".
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "POST",
        "expenses",
        Canned::ok(json!({"expense": {"url": fake.res("expenses/3")}})),
    );
    let s = server(&fake);
    let r = s
        .create_mileage_claim(
            params(
                json!({"dated_on": "2026-11-01", "description": "Leeds to York", "mileage": "25"}),
            ),
            auth(),
        )
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let body = fake.last("POST", "expenses").body.unwrap();
    assert_eq!(body["expense"]["category"], fake.res("categories/249"));
}

#[tokio::test]
async fn attachments_are_read_from_their_signed_link_without_a_token() {
    use base64::Engine as _;
    let fake = FakeFreeAgent::start().await;
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
    let link = fake.file("receipt.jpg", jpeg.clone());
    fake.on(
        "GET",
        "attachments/9",
        Canned::ok(json!({"attachment": {"url": fake.res("attachments/9"), "content_src": link, "content_type": "image/jpeg", "file_name": "receipt.jpg"}})),
    );
    let pdf = b"%PDF-1.4 receipt".to_vec();
    let pdf_link = fake.file("bill.pdf", pdf.clone());
    fake.on(
        "GET",
        "attachments/10",
        Canned::ok(json!({"attachment": {"url": fake.res("attachments/10"), "content_src": pdf_link, "content_type": "application/x-pdf", "file_name": "bill.pdf"}})),
    );
    fake.on(
        "GET",
        "attachments/11",
        Canned::ok(json!({"attachment": {"url": fake.res("attachments/11"), "content_src": "http://files.example.net/x.jpg", "content_type": "image/jpeg"}})),
    );
    let s = server(&fake);

    let r = serde_json::to_value(
        s.get_attachment(params(json!({"id": fake.res("attachments/9")})), auth())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(r["content"][1]["type"], "image", "{r}");
    assert_eq!(r["content"][1]["mimeType"], "image/jpeg");
    assert_eq!(
        r["content"][1]["data"],
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    );
    let got = fake
        .api_log()
        .into_iter()
        .find(|l| l.path == "/files/receipt.jpg")
        .expect("the file was downloaded");
    assert!(
        got.authorization.is_none(),
        "no FreeAgent token goes to the storage link"
    );

    // A PDF comes as an embedded file, typed as everyone else spells it.
    let r = serde_json::to_value(
        s.get_attachment(params(json!({"id": "10"})), auth())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(r["content"][1]["type"], "resource", "{r}");
    assert_eq!(r["content"][1]["resource"]["mimeType"], "application/pdf");
    assert_eq!(
        r["content"][1]["resource"]["blob"],
        base64::engine::general_purpose::STANDARD.encode(&pdf)
    );

    // Links anywhere but FreeAgent's storage (or the API itself) are not followed.
    let r = s
        .get_attachment(params(json!({"id": "11"})), auth())
        .await
        .unwrap();
    assert!(
        error_text(&r).contains("unexpected host"),
        "{}",
        error_text(&r)
    );
    assert!(fake.api_log().iter().all(|l| !l.path.contains("x.jpg")));
}

#[tokio::test]
async fn attach_file_takes_an_upload_or_base64_but_not_both() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    for args in [
        json!({"target": "expense", "id": "5"}),
        json!({"target": "expense", "id": "5", "upload_url": "https://books.example.com/upload/AAAAAAAAAAAAAAAAAAAAAA", "file_name": "r.pdf", "data_base64": "JVBERg=="}),
    ] {
        let r = s.attach_file(params(args), auth()).await.unwrap();
        assert!(
            error_text(&r).starts_with("validation:"),
            "{}",
            error_text(&r)
        );
    }
    let r = s
        .attach_file(
            params(json!({"target": "expense", "id": "5", "upload_url": "https://books.example.com/upload/AAAAAAAAAAAAAAAAAAAAAA"})),
            auth(),
        )
        .await
        .unwrap();
    assert!(
        error_text(&r).starts_with("not_found:"),
        "{}",
        error_text(&r)
    );
    let r = s
        .create_upload_link(params(json!({"target": "expense"})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).contains("together"), "{}", error_text(&r));
}

#[tokio::test]
async fn the_health_check_flags_duplicates_vat_director_payments_and_journals() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "company",
        Canned::ok(json!({"company": {"annual_accounting_periods": [
            {"starts_on": "2025-10-01", "ends_on": "2026-09-30"},
            {"starts_on": "2026-10-01", "ends_on": "2027-09-30"}
        ]}})),
    );
    fake.on(
        "GET",
        "categories",
        Canned::ok(json!({"admin_expenses_categories": [
            {"url": fake.res("categories/285"), "description": "Computer Software", "nominal_code": "285"},
            {"url": fake.res("categories/363"), "description": "Bank/Finance Charges", "nominal_code": "363"},
            {"url": fake.res("categories/365"), "description": "Insurance", "nominal_code": "365"}
        ]})),
    );
    fake.on(
        "GET",
        "bills",
        Canned::ok(json!({"bills": [
            {"url": fake.res("bills/1"), "contact": fake.res("contacts/4"), "contact_name": "Acme", "total_value": "60.0", "dated_on": "2026-10-01", "reference": "A1"},
            {"url": fake.res("bills/2"), "contact": fake.res("contacts/4"), "contact_name": "Acme", "total_value": "60.00", "dated_on": "2026-10-04", "reference": "A1"},
            {"url": fake.res("bills/3"), "contact": fake.res("contacts/4"), "contact_name": "Acme", "total_value": "60.0", "dated_on": "2026-11-01", "reference": "A2"}
        ]})),
    );
    fake.on(
        "GET",
        "expenses",
        Canned::ok(json!({"expenses": [
            {"url": fake.res("expenses/1"), "user": fake.res("users/7"), "category": fake.res("categories/365"), "dated_on": "2026-10-02", "description": "Van insurance", "gross_value": "-120.00", "sales_tax_value": "-20.00"},
            {"url": fake.res("expenses/2"), "user": fake.res("users/7"), "category": fake.res("categories/285"), "dated_on": "2026-10-05", "description": "Adobe", "gross_value": "-19.99"},
            {"url": fake.res("expenses/3"), "user": fake.res("users/7"), "category": fake.res("categories/285"), "dated_on": "2026-10-06", "description": "Adobe", "gross_value": "-19.99"}
        ]})),
    );
    fake.on(
        "GET",
        "users",
        Canned::ok(json!({"users": [
            {"url": fake.res("users/7"), "first_name": "Ann", "last_name": "Leer", "role": "Director"}
        ]})),
    );
    fake.on(
        "GET",
        "bank_accounts",
        Canned::ok(
            json!({"bank_accounts": [{"url": fake.res("bank_accounts/1"), "name": "Main"}]}),
        ),
    );
    fake.on(
        "GET",
        "bank_transaction_explanations",
        Canned::ok(json!({"bank_transaction_explanations": [
            {"url": fake.res("bank_transaction_explanations/1"), "category": fake.res("categories/285"), "dated_on": "2026-10-07", "description": "A LEER expenses", "gross_value": "-250.00"},
            {"url": fake.res("bank_transaction_explanations/2"), "category": fake.res("categories/363"), "dated_on": "2026-10-08", "description": "Account Balance correction", "gross_value": "-6738.73"},
            {"url": fake.res("bank_transaction_explanations/3"), "paid_user": fake.res("users/7"), "dated_on": "2026-10-09", "description": "A LEER", "gross_value": "-500.00"},
            {"url": fake.res("bank_transaction_explanations/4"), "category": fake.res("categories/363"), "dated_on": "2026-10-10", "description": "Monthly fee", "gross_value": "-5.00"}
        ]})),
    );
    fake.on(
        "GET",
        "journal_sets",
        Canned::ok(json!({"journal_sets": [
            {"url": fake.res("journal_sets/1"), "dated_on": "2026-10-31", "description": "Sale of Car"}
        ]})),
    );
    fake.on(
        "GET",
        "journal_sets/1",
        Canned::ok(json!({"journal_set": {"url": fake.res("journal_sets/1"), "dated_on": "2026-10-31", "description": "Sale of Car", "journal_entries": [
            {"category": fake.res("categories/907-1"), "debit_value": "10000.0"},
            {"category": fake.res("categories/001"), "debit_value": "-10000.0"}
        ]}})),
    );
    let s = server(&fake);
    let v = json(
        &s.books_health_check(params(json!({"to_date": "2026-11-08"})), auth())
            .await
            .unwrap(),
    );
    assert_eq!(
        v["period"],
        json!({"from": "2026-10-01", "to": "2026-11-08"})
    );
    assert_eq!(
        fake.last("GET", "bills").param("from_date"),
        Some("2026-10-01")
    );
    assert_eq!(v["possible_duplicate_bills"]["count"], 1);
    assert_eq!(
        v["possible_duplicate_bills"]["items"][0]["bill_ids"],
        json!(["1", "2"])
    );
    assert_eq!(v["possible_duplicate_expenses"]["count"], 1);
    assert_eq!(v["vat_on_items_without_vat"]["count"], 1);
    assert_eq!(v["vat_on_items_without_vat"]["items"][0]["id"], "1");
    let d = &v["director_payments_as_costs"];
    assert_eq!(d["count"], 1, "a payment explained to the director is fine");
    assert_eq!(d["items"][0]["id"], "1");
    assert!(d["items"][0]["why"].as_str().unwrap().contains("Ann Leer"));
    assert_eq!(v["large_bank_charges"]["count"], 1, "small fees are fine");
    assert_eq!(v["corrections_booked_as_costs"]["items"][0]["id"], "2");
    let j = &v["journals_to_check"]["items"][0];
    assert_eq!(j["total"], "10000.00");
    let why = j["why"].as_str().unwrap();
    assert!(
        why.contains("round amount") && why.contains("director's loan"),
        "{why}"
    );
    assert_eq!(v["clean"], false);
    assert_eq!(
        v["findings"].as_array().unwrap().len(),
        7,
        "{}",
        v["findings"]
    );
    assert!(v.get("unavailable").is_none(), "{v}");
}

#[tokio::test]
async fn explanations_are_suggested_from_how_the_payee_was_explained_before() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "categories",
        Canned::ok(json!({"admin_expenses_categories": [
            {"url": fake.res("categories/285"), "description": "Computer Software", "nominal_code": "285"}
        ]})),
    );
    fake.on(
        "GET",
        "bank_accounts/1",
        Canned::ok(json!({"bank_account": {"url": fake.res("bank_accounts/1"), "name": "Main"}})),
    );
    fake.on(
        "GET",
        "bank_transactions",
        Canned::ok(json!({"bank_transactions": [
            {"url": fake.res("bank_transactions/10"), "dated_on": "2026-11-02", "description": "CARD PAYMENT TO ADOBE SYSTEMS 4410", "amount": "-19.99", "unexplained_amount": "-19.99"},
            {"url": fake.res("bank_transactions/11"), "dated_on": "2026-11-03", "description": "SOMEWHERE NEW", "amount": "-5.00", "unexplained_amount": "-5.00"}
        ]})),
    );
    fake.on(
        "GET",
        "bank_transaction_explanations",
        Canned::ok(json!({"bank_transaction_explanations": [
            {"description": "ADOBE SYSTEMS 1111", "category": fake.res("categories/285"), "sales_tax_rate": "20.0", "dated_on": "2026-08-02", "gross_value": "-19.99"},
            {"description": "ADOBE SYSTEMS 2222", "category": fake.res("categories/285"), "sales_tax_rate": "20.0", "dated_on": "2026-09-02", "gross_value": "-19.99"},
            {"description": "ADOBE SYSTEMS 3333", "category": fake.res("categories/285"), "sales_tax_rate": "20.0", "dated_on": "2026-10-02", "gross_value": "-19.99"},
            {"description": "ADOBE SYSTEMS refund", "category": fake.res("categories/001"), "dated_on": "2026-10-05", "gross_value": "19.99"},
            {"description": "SOMEWHERE NEW", "category": fake.res("categories/285"), "dated_on": "2026-10-05", "gross_value": "-5.00", "marked_for_review": true}
        ]})),
    );
    let s = server(&fake);
    let v = json(
        &s.suggest_explanations(params(json!({"bank_account_id": "1"})), auth())
            .await
            .unwrap(),
    );
    let l = fake.last("GET", "bank_transactions");
    assert_eq!(l.param("view"), Some("unexplained"));
    assert!(
        l.param("bank_account")
            .unwrap()
            .ends_with("/bank_accounts/1")
    );
    assert_eq!(v["with_suggestion"], 1);
    assert_eq!(v["without_history"], 1, "unapproved guesses do not count");
    let t = &v["transactions"][0];
    let sug = &t["suggestion"];
    assert_eq!(
        sug["confidence"], "high",
        "money in is not mixed with money out"
    );
    assert_eq!(sug["category_name"], "Computer Software");
    assert_eq!(
        sug["explain_with"],
        json!({"bank_transaction_id": "10", "category": "285", "sales_tax_rate": "20.0"})
    );
    assert!(v["transactions"][1].get("suggestion").is_none());
}

#[tokio::test]
async fn explain_bank_transactions_carries_on_past_a_failure() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "bank_transactions/10",
        Canned::ok(json!({"bank_transaction": {"url": fake.res("bank_transactions/10"), "dated_on": "2026-11-02", "description": "ADOBE", "amount": "-19.99", "unexplained_amount": "-19.99"}})),
    );
    fake.on(
        "GET",
        "bank_transactions/11",
        Canned::status(404, json!({"errors": {"error": {"message": "not found"}}})),
    );
    fake.on(
        "POST",
        "bank_transaction_explanations",
        Canned::ok(json!({"bank_transaction_explanation": {"url": fake.res("bank_transaction_explanations/55")}})),
    );
    let s = server(&fake);
    let v = json(
        &s.explain_bank_transactions(
            params(json!({"explanations": [
                {"bank_transaction_id": "11", "category": "285"},
                {"bank_transaction_id": "10", "category": "285", "sales_tax_rate": "20.0"}
            ]})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(v["explained"], 1);
    assert_eq!(v["failed"], 1);
    assert_eq!(v["results"][0]["explanation_id"], "55");
    assert!(
        v["failures"][0]["error"]
            .as_str()
            .unwrap()
            .starts_with("not_found")
    );
    let body = fake
        .last("POST", "bank_transaction_explanations")
        .body
        .unwrap();
    let e = &body["bank_transaction_explanation"];
    assert_eq!(e["gross_value"], "-19.99");
    assert!(e["category"].as_str().unwrap().ends_with("/categories/285"));
    let r = s
        .explain_bank_transactions(params(json!({"explanations": []})), auth())
        .await
        .unwrap();
    assert!(error_text(&r).starts_with("validation"));
}

#[tokio::test]
async fn a_receipt_is_matched_to_its_payment_expense_or_bill() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "bank_accounts",
        Canned::ok(
            json!({"bank_accounts": [{"url": fake.res("bank_accounts/1"), "name": "Main"}]}),
        ),
    );
    fake.on(
        "GET",
        "bank_transactions",
        Canned::ok(json!({"bank_transactions": [
            {"url": fake.res("bank_transactions/1"), "dated_on": "2026-10-03", "description": "CARD PAYMENT TO SCREWFIX 22", "amount": "-42.50", "unexplained_amount": "-42.50"},
            {"url": fake.res("bank_transactions/2"), "dated_on": "2026-10-02", "description": "B&Q", "amount": "-42.50", "unexplained_amount": "0.0",
             "bank_transaction_explanations": [{"url": fake.res("bank_transaction_explanations/8"), "category": fake.res("categories/285")}]},
            {"url": fake.res("bank_transactions/3"), "dated_on": "2026-10-02", "description": "OTHER", "amount": "-9.00", "unexplained_amount": "-9.00"}
        ]})),
    );
    fake.on(
        "GET",
        "expenses",
        Canned::ok(json!({"expenses": [
            {"url": fake.res("expenses/4"), "dated_on": "2026-10-04", "description": "Paint", "gross_value": "-42.50", "attachment": {"url": fake.res("attachments/1")}}
        ]})),
    );
    fake.on("GET", "bills", Canned::ok(json!({"bills": []})));
    let s = server(&fake);
    let v = json(
        &s.match_receipt(
            params(json!({"amount": "42.50", "date": "2026-10-02", "supplier": "Screwfix Direct"})),
            auth(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        v["looked_for"],
        json!({"amount": "42.50", "from": "2026-09-27", "to": "2026-10-07"})
    );
    assert_eq!(
        fake.last("GET", "bank_transactions").param("from_date"),
        Some("2026-09-27")
    );
    assert_eq!(v["found"], 3);
    let c = v["candidates"].as_array().unwrap();
    assert_eq!(
        c[0]["bank_transaction_id"], "1",
        "the supplier's name wins over a day"
    );
    assert_eq!(
        c[1]["attach_with"],
        json!({"target": "bank_transaction_explanation", "id": "8"})
    );
    assert_eq!(
        c[2]["kind"], "expense",
        "one with a file already comes last"
    );
    assert_eq!(c[2]["already_has_a_file"], true);
}

#[tokio::test]
async fn several_upload_links_are_made_for_a_pile_of_receipts() {
    let fake = FakeFreeAgent::start().await;
    let s = server(&fake);
    let v = json(
        &s.create_upload_link(params(json!({"count": 3})), auth())
            .await
            .unwrap(),
    );
    let links = v["upload_urls"].as_array().unwrap();
    assert_eq!(links.len(), 3);
    assert!(
        links[0]
            .as_str()
            .unwrap()
            .starts_with("https://books.example.com/")
    );
    assert_ne!(links[0], links[1]);
    for bad in [
        json!({"count": 11}),
        json!({"count": 0}),
        json!({"target": "bill", "id": "5", "count": 2}),
    ] {
        let r = s.create_upload_link(params(bad), auth()).await.unwrap();
        assert!(error_text(&r).starts_with("validation"));
    }
}

#[tokio::test]
async fn the_month_end_check_lists_what_to_finish_before_the_vat_return() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "vat_returns",
        Canned::ok(json!({"vat_returns": [
            {"url": fake.res("vat_returns/2026-09-30"), "period_starts_on": "2026-07-01", "period_ends_on": "2026-09-30", "filing_status": "filed"},
            {"url": fake.res("vat_returns/2026-12-31"), "period_starts_on": "2026-10-01", "period_ends_on": "2026-12-31", "filing_due_on": "2027-02-07", "filing_status": "unfiled"}
        ]})),
    );
    fake.on(
        "GET",
        "bank_accounts",
        Canned::ok(json!({"bank_accounts": [
            {"url": fake.res("bank_accounts/1"), "name": "Main", "unexplained_transaction_count": 0, "marked_for_review_count": 0},
            {"url": fake.res("bank_accounts/2"), "name": "Photography", "unexplained_transaction_count": 8, "marked_for_review_count": 1},
            {"url": fake.res("bank_accounts/3"), "name": "Old", "status": "hidden", "unexplained_transaction_count": 4}
        ]})),
    );
    fake.on(
        "GET",
        "expenses",
        Canned::ok(json!({"expenses": [
            {"url": fake.res("expenses/1"), "dated_on": "2026-10-02", "description": "Lunch", "gross_value": "-12.00", "sales_tax_value": "-2.00"},
            {"url": fake.res("expenses/2"), "dated_on": "2026-10-03", "description": "Train", "gross_value": "-40.00", "attachment": {"url": fake.res("attachments/9")}},
            {"url": fake.res("expenses/3"), "dated_on": "2026-10-04", "description": "Leeds", "gross_value": "-11.25", "mileage": "25.0"}
        ]})),
    );
    // Bills are read twice: dated in the period, then overdue.
    fake.on_seq(
        "GET",
        "bills",
        vec![
            Canned::ok(json!({"bills": [
                {"url": fake.res("bills/5"), "reference": "INV-1", "dated_on": "2026-10-09", "total_value": "120.0"},
                {"url": fake.res("bills/6"), "reference": "INV-2", "dated_on": "2026-10-10", "total_value": "60.0", "attachment": {"url": fake.res("attachments/8")}}
            ]})),
            Canned::ok(json!({"bills": []})),
        ],
    );
    // Invoices: drafts, then overdue.
    fake.on_seq(
        "GET",
        "invoices",
        vec![
            Canned::ok(json!({"invoices": [
                {"url": fake.res("invoices/1"), "reference": "036", "dated_on": "2024-01-27", "total_value": "3.49"},
                {"url": fake.res("invoices/2"), "reference": "280", "dated_on": "2026-11-01", "total_value": "500.0"}
            ]})),
            Canned::ok(json!({"invoices": [{"url": fake.res("invoices/3"), "due_value": "179.76"}]})),
        ],
    );
    fake.on(
        "GET",
        "accounting/trial_balance/summary",
        Canned::ok(json!({"trial_balance_summary": [
            {"nominal_code": "999", "display_nominal_code": "999", "name": "Suspense", "total": "12.50"},
            {"nominal_code": "001", "name": "Sales", "total": "-100.0"}
        ]})),
    );
    fake.on(
        "GET",
        "accounting/balance_sheet",
        Canned::ok(json!({"balance_sheet": {"current_assets": {"accounts": [
            {"name": "Director Loan Account: Ann Lee", "nominal_code": "907-1", "total_debit_value": 1779}
        ]}}})),
    );
    let s = server(&fake);
    let v = json(&s.month_end_check(params(json!({})), auth()).await.unwrap());
    assert_eq!(
        v["period"],
        json!({"from": "2026-10-01", "to": "2026-12-31", "basis": "the next VAT return"})
    );
    assert_eq!(v["vat_return"]["filing_due_on"], "2027-02-07");
    let l = fake.last("GET", "expenses");
    assert_eq!(l.param("from_date"), Some("2026-10-01"));
    assert_eq!(l.param("to_date"), Some("2026-12-31"));
    assert_eq!(v["bank_accounts_to_finish"].as_array().unwrap().len(), 1);
    assert_eq!(
        v["expenses_without_receipts"]["count"], 1,
        "mileage needs no receipt"
    );
    assert_eq!(
        v["expenses_without_receipts"]["vat_reclaimed_without_receipt"],
        "2.00"
    );
    assert_eq!(v["bills_without_invoice"]["count"], 1);
    assert_eq!(v["draft_invoices"]["in_period"][0]["reference"], "280");
    assert_eq!(v["draft_invoices"]["older_than_30_days"]["count"], 1);
    assert_eq!(v["overdue_invoices"], json!({"count": 1, "due": "179.76"}));
    assert_eq!(v["suspense"], "12.50");
    assert_eq!(v["directors_loans_overdrawn"][0]["name"], "Ann Lee");
    assert_eq!(v["ready"], false);
    let todo = v["to_do"].to_string();
    for want in [
        "Explain 8 transaction(s) in Photography",
        "Approve or correct 1 guessed explanation(s) in Photography",
        "Add receipts to 1 expense(s), which reclaim 2.00 of VAT",
        "Attach the supplier's invoice to 1 bill(s)",
        "Chase 1 overdue invoice(s), 179.76 in all",
        "Clear 12.50 from 999 Suspense",
        "director_loan_review",
    ] {
        assert!(todo.contains(want), "missing {want:?} in {todo}");
    }
    assert!(v.get("unavailable").is_none(), "{v}");
    assert!(
        fake.api_log().iter().all(|l| l.method == "GET"),
        "a check changes nothing"
    );
}

#[tokio::test]
async fn the_director_loan_review_estimates_s455_on_what_was_still_owed() {
    let fake = FakeFreeAgent::start().await;
    fake.on(
        "GET",
        "company",
        Canned::ok(json!({"company": {"url": fake.res("company"), "sales_tax_registration_status": "Registered", "annual_accounting_periods": [
            {"starts_on": "2025-01-01", "ends_on": "2025-12-31"},
            {"starts_on": "2026-01-01", "ends_on": "2026-12-31"}
        ]}})),
    );
    let sheet = |loan: i64, expenses: i64| {
        Canned::ok(json!({"balance_sheet": {
            "current_assets": {"accounts": [{"name": "Director Loan Account: Ann Lee", "nominal_code": "907-1", "total_debit_value": loan}]},
            "current_liabilities": {"accounts": [{"name": "Expense Account: Ann Lee", "nominal_code": "905-1", "total_debit_value": expenses}]}
        }}))
    };
    // Read today, then at the year end, then at the s455 deadline.
    fake.on_seq(
        "GET",
        "accounting/balance_sheet",
        vec![sheet(1779, -2301), sheet(11142, -8064), sheet(1779, -900)],
    );
    let s = server(&fake);
    let v = json(
        &s.director_loan_review(params(json!({"as_at_date": "2026-11-08"})), auth())
            .await
            .unwrap(),
    );
    let dates: Vec<String> = fake
        .api_log()
        .into_iter()
        .filter(|l| l.path == "/v2/accounting/balance_sheet")
        .filter_map(|l| l.param("as_at_date").map(String::from))
        .collect();
    assert_eq!(dates, ["2026-11-08", "2025-12-31", "2026-09-30"]);
    let d = &v["directors"][0];
    assert_eq!(d["name"], "Ann Lee");
    assert_eq!(d["loan_account"], "1779.00");
    assert_eq!(d["loan_position"], "owes the company");
    assert_eq!(d["expenses_owed_to_them"], "2301.00");
    assert_eq!(d["net"], "-522.00");
    assert_eq!(d["net_position"], "owed by the company");
    assert_eq!(d["at_year_end"]["loan_account"], "11142.00");
    assert_eq!(
        d["s455"],
        json!({"year_end": "2025-12-31", "repay_by": "2026-09-30", "deadline_passed": true, "owed_at_year_end": "11142.00", "still_owed": "1779.00", "estimated_charge": "600.41"})
    );
    let flags = d["flags"].to_string();
    assert!(flags.contains("s455 tax of about 600.41 is due"), "{flags}");
    assert!(flags.contains("set against the loan"), "{flags}");
    assert!(!flags.contains("benefit in kind"), "{flags}");
}
