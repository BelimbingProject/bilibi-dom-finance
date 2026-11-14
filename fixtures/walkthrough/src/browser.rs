//! NU1 browser records use the local authority without changing security grants.
use crate::clearance::{self, StandIn, CLIENT_ID, CLIENT_SECRET};
use bilibi_database::Db;
use bilibi_finance_einvoice::Einvoice;
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{Config, Executor};
use bilibi_settings::Target;
use serde_json::json;
use std::sync::Arc;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

pub async fn prepare_einvoice(
    db: &Db,
    services: &Services,
    manager: &bilibi_tenancy::Scope,
    admin: &bilibi_tenancy::Scope,
    company: i64,
) -> Outcome<tokio::sync::watch::Sender<bool>> {
    let target = Target::Company(OrganisationId::new(company)?);
    let url = clearance::serve(Arc::new(StandIn::default())).await?;
    clearance::save(
        db,
        services,
        admin,
        Target::Global,
        "finance.einvoice.sandbox.api_url",
        json!(url),
    )
    .await?;
    for (key, value) in [
        ("environment", "sandbox"),
        ("document_version", "1.0"),
        ("sandbox.client_id", CLIENT_ID),
        ("sandbox.client_secret", CLIENT_SECRET),
        ("supplier.tax_id", "C2584563222"),
        ("supplier.registration_number", "201901234567"),
        ("supplier.industry_code", "47111"),
        ("supplier.business_activity", "Retail"),
        ("supplier.phone", "+60123456789"),
        ("supplier.address_line1", "2 Jalan Contoh"),
        ("supplier.city", "Kuala Lumpur"),
        ("supplier.state_code", "14"),
        ("supplier.country_code", "MYS"),
    ] {
        clearance::save(
            db,
            services,
            manager,
            target,
            &format!("finance.einvoice.{key}"),
            json!(value),
        )
        .await?;
    }
    let facade = services.get::<Einvoice>(bilibi_finance_einvoice::SERVICE)?;
    let executor = Executor::from_services(services, Config::default())?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    tokio::spawn(executor.run(shutdown));
    if facade
        .list_documents(
            manager,
            company,
            &bilibi_finance_einvoice::DocumentQuery::default(),
        )
        .await?
        .total_entries
        > 0
    {
        return Ok(stop);
    }
    let mut last_document = None;
    for n in 1..=6 {
        let code = match n {
            4 => "BROWSER-E-EXPIRED".to_owned(),
            5 => "BROWSER-E-TIMEOUT".to_owned(),
            6 => "BROWSER-E-REJECTION".to_owned(),
            _ => format!("BROWSER-E-{n}"),
        };
        let document = facade
            .issue_document(manager, company, &clearance::draft(&code, None))
            .await?;
        if n == 3 {
            last_document = Some(document.document.id);
        }
        if matches!(n, 1 | 4 | 5 | 6) {
            facade
                .submit_document(manager, company, document.document.id)
                .await?;
            let client = db.get().await?;
            clearance::jobs_settle(&client).await?;
        }
        println!(
            "E-invoice: /finance/einvoice/documents/{}",
            document.document.id
        );
    }
    prove_document_messages(services, manager, last_document.unwrap()).await?;
    prove_invoice_einvoice(db, services, manager, company).await?;
    prove_report_messages(services, manager).await?;
    prove_payroll_messages(services, manager, company).await?;
    Ok(stop)
}

/// Exercise the mounted pages against real facade records, without mocks.
pub async fn prove_messages(
    services: &Services,
    scope: &bilibi_tenancy::Scope,
    asset: i64,
) -> Outcome<()> {
    use bilibi_database::Error;
    use bilibi_tenancy::Scope;
    use bilibi_ui::page::{LiveInput, LiveRequest, PageRequest, PageService};
    use std::collections::HashMap;
    let pages = services.get::<PageService<Scope, Error>>("finance/fixed_assets.pages")?;
    // A refusal stays in its dialog, beside its fields, until it is closed.
    for (operation, path, open, refusal, marker, close) in [
        (
            "categories",
            HashMap::new(),
            json!({"type":"new"}),
            json!({"type":"create_category"}),
            "category-modal",
            json!({"type":"close_category"}),
        ),
        (
            "asset",
            HashMap::from([("id".into(), asset.to_string())]),
            json!({"type":"open_dispose"}),
            json!({"type":"dispose"}),
            "asset-dispose-modal",
            json!({"type":"close_dialog"}),
        ),
    ] {
        let mut state = pages
            .0
            .mount(PageRequest {
                operation,
                scope: Some(scope),
                path: &path,
                query: &HashMap::new(),
                body: &[],
                framed: false,
            })
            .await?
            .unwrap();
        state
            .update(LiveRequest {
                scope,
                credential: "",
                input: LiveInput::Event(&open),
            })
            .await?;
        let body = state
            .update(LiveRequest {
                scope,
                credential: "",
                input: LiveInput::Event(&refusal),
            })
            .await?;
        let refused = |html: &str| {
            html.contains(&format!("{marker}-form-errors"))
                || html.contains(&format!("{marker}-alert"))
        };
        assert!(
            refused(&body.html),
            "{operation}: the refusal shows in its dialog"
        );
        let refreshed = state
            .update(LiveRequest {
                scope,
                credential: "",
                input: LiveInput::Refresh,
            })
            .await?;
        assert_eq!(
            body.html, refreshed.html,
            "refresh retains refusal and draft"
        );
        let stale = json!({"type":"clear-flash","key":"success"});
        let unchanged = state
            .update(LiveRequest {
                scope,
                credential: "",
                input: LiveInput::Event(&stale),
            })
            .await?;
        assert_eq!(
            body.html, unchanged.html,
            "old success dismissal retains refusal"
        );
        let closed = state
            .update(LiveRequest {
                scope,
                credential: "",
                input: LiveInput::Event(&close),
            })
            .await?;
        assert!(!closed.html.contains(marker));
        assert!(!refused(&closed.html));
    }
    println!(
        "NU1: Fixed Assets list/detail dialog refusals survive refresh and stale success dismissal; Cancel closes them"
    );
    Ok(())
}

/// A person starts the e-invoice of an issued invoice from the invoice
/// itself: the Invoices list's contract offers the address, that address
/// opens New e-invoice with what the invoice says, a refusal stays beside its
/// field, and the recorded e-invoice is sent and shows on the invoice.
async fn prove_invoice_einvoice(
    db: &Db,
    services: &Services,
    scope: &bilibi_tenancy::Scope,
    company: i64,
) -> Outcome<()> {
    use bilibi_agent_api::page::{PageContracts, SERVICE};
    use bilibi_database::Error;
    use bilibi_tenancy::Scope;
    use bilibi_ui::page::{LiveInput, LiveRequest, PageRequest, PageService};
    use std::collections::HashMap;
    let contracts = services.get::<PageContracts>(SERVICE)?;
    let listed = contracts
        .read(
            scope,
            "finance.receivables.document.page",
            &json!({"company_id":company,"kind":"invoice","status":"issued","page_size":100}),
        )
        .await?["result"]
        .clone();
    let Some(invoice) = listed["documents"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["fiscal"]["start"].is_string())
    else {
        println!("NU1: no issued invoice without an e-invoice; the invoice proof is skipped");
        return Ok(());
    };
    assert_eq!(listed["fiscal_documents"], json!(true));
    let id = invoice["id"].as_i64().ok_or("invoice id")?;
    let number = invoice["document_reference"]
        .as_str()
        .ok_or("invoice number")?;
    let start = invoice["fiscal"]["start"].as_str().ok_or("start")?;
    assert!(start.starts_with("/finance/einvoice/documents?"), "{start}");
    let query = HashMap::from([
        ("source".to_owned(), "finance/receivables".to_owned()),
        ("record".to_owned(), format!("document:{id}")),
    ]);
    let pages = services.get::<PageService<Scope, Error>>("finance/einvoice.pages")?;
    let mut state = pages
        .0
        .mount(PageRequest {
            operation: "index",
            scope: Some(scope),
            path: &HashMap::new(),
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .unwrap();
    let opened = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    for text in ["einvoice-new-modal", number, "Submit e-invoice"] {
        assert!(opened.html.contains(text), "the dialog shows {text}");
    }
    // The invoice holds no authority codes: what is missing is said beside
    // its field and nothing is recorded.
    let bare = json!({"type":"create_einvoice"});
    let refused = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&bare),
        })
        .await?;
    assert!(refused.redirect.is_none());
    assert!(
        refused.html.contains("einvoice-new-modal") && refused.html.contains("be blank"),
        "a refusal stays in the dialog: {}",
        refused.html
    );
    let filled = json!({
        "type":"create_einvoice",
        "einvoice[tax_id]":"C29702635060",
        "einvoice[registration_type]":"BRN",
        "einvoice[phone]":"+60355550100",
        "einvoice[state_code]":"14",
        "einvoice[country_code]":"MYS",
        "einvoice[classification_0]":"003",
        "einvoice[tax_type_0]":"02",
        "einvoice[classification_1]":"003",
    });
    let sent = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&filled),
        })
        .await?;
    let Some(href) = sent.redirect.as_deref() else {
        return Err(format!("the e-invoice was not recorded: {}", sent.html).into());
    };
    assert!(href.starts_with("/finance/einvoice/documents/"), "{href}");
    let client = db.get().await?;
    clearance::jobs_settle(&client).await?;
    let read = contracts
        .read(
            scope,
            "finance.receivables.document.get",
            &json!({"company_id":company,"document_id":id}),
        )
        .await?["result"]
        .clone();
    assert_eq!(read["fiscal"]["document"]["status"], json!("valid"), "{read}");
    assert_eq!(read["fiscal"]["document"]["href"], json!(href));
    assert!(read["fiscal"]["start"].is_null(), "a standing e-invoice is not started twice");
    // The address is a request, never proof: asking again opens only a notice.
    let mut again = pages
        .0
        .mount(PageRequest {
            operation: "index",
            scope: Some(scope),
            path: &HashMap::new(),
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .unwrap();
    let again = again
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    assert!(again.html.contains("already has e-invoice"));
    assert!(!again.html.contains("einvoice-new-submit"));
    println!(
        "NU1: invoice {number} offers Submit e-invoice; the dialog refuses missing codes beside their fields, records and sends it; the invoice reads its e-invoice as valid at {href} and offers no second one"
    );
    Ok(())
}

async fn prove_document_messages(
    services: &Services,
    scope: &bilibi_tenancy::Scope,
    document: i64,
) -> Outcome<()> {
    use bilibi_database::Error;
    use bilibi_tenancy::Scope;
    use bilibi_ui::page::{LiveInput, LiveRequest, PageRequest, PageService};
    use std::collections::HashMap;
    let pages = services.get::<PageService<Scope, Error>>("finance/einvoice.pages")?;
    let path = HashMap::from([("id".into(), document.to_string())]);
    let mut state = pages
        .0
        .mount(PageRequest {
            operation: "detail",
            scope: Some(scope),
            path: &path,
            query: &HashMap::new(),
            body: &[],
            framed: false,
        })
        .await?
        .unwrap();
    let refused = json!({"type":"refresh"});
    let body = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&refused),
        })
        .await?;
    assert!(body
        .html
        .contains("The authority has not given this document a UUID yet"));
    let stale = json!({"type":"clear-flash","key":"success"});
    for input in [LiveInput::Refresh, LiveInput::Event(&stale)] {
        let kept = state
            .update(LiveRequest {
                scope,
                credential: "",
                input,
            })
            .await?;
        assert_eq!(
            body.html, kept.html,
            "e-invoice refusal survives refresh and stale success dismissal"
        );
    }
    let dismiss = json!({"type":"clear-flash","key":"error"});
    let cleared = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&dismiss),
        })
        .await?;
    assert!(!cleared.html.contains("id=\"flash-error\""));
    println!(
        "NU1: E-invoice refusal survives refresh and stale success dismissal; matching dismissal clears without an error"
    );
    Ok(())
}

async fn prove_report_messages(services: &Services, scope: &bilibi_tenancy::Scope) -> Outcome<()> {
    use bilibi_database::Error;
    use bilibi_tenancy::Scope;
    use bilibi_ui::page::{LiveInput, LiveRequest, PageRequest, PageService};
    use std::collections::HashMap;
    let pages = services.get::<PageService<Scope, Error>>("finance/reports.pages")?;
    let mut state = pages
        .0
        .mount(PageRequest {
            operation: "reports",
            scope: Some(scope),
            path: &HashMap::new(),
            query: &HashMap::new(),
            body: &[],
            framed: false,
        })
        .await?
        .unwrap();
    state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    let export = json!({"type":"export_auditor_pack"});
    let body = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&export),
        })
        .await?;
    assert!(body.html.contains("Auditor pack generated."));
    let wrong = json!({"type":"clear-flash","key":"error"});
    let kept = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&wrong),
        })
        .await?;
    assert_eq!(body.html, kept.html);
    let dismiss = json!({"type":"clear-flash","key":"success"});
    let cleared = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&dismiss),
        })
        .await?;
    assert!(!cleared.html.contains("id=\"flash-success\""));
    let params = HashMap::from([("view".into(), "profit-and-loss".into())]);
    let changed = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Params(&params),
        })
        .await?;
    assert!(!changed.html.contains("id=\"flash-success\""));
    println!(
        "NU1: real auditor pack success clears by kind and stays dismissed after changing reports"
    );
    Ok(())
}

async fn prove_payroll_messages(
    services: &Services,
    scope: &bilibi_tenancy::Scope,
    company: i64,
) -> Outcome<()> {
    use bilibi_database::Error;
    use bilibi_tenancy::Scope;
    use bilibi_ui::page::{LiveInput, LiveRequest, PageRequest, PageService};
    use std::collections::HashMap;
    let ledger = services.get::<bilibi_finance_ledger::Ledger>(bilibi_finance_ledger::SERVICE)?;
    let accounts = ledger.list_accounts(scope, company).await?;
    let id = |code: &str| {
        accounts
            .iter()
            .find(|a| a.code == code)
            .unwrap()
            .id
            .to_string()
    };
    let pages = services.get::<PageService<Scope, Error>>("finance/payroll.pages")?;
    let mut state = pages
        .0
        .mount(PageRequest {
            operation: "setup",
            scope: Some(scope),
            path: &HashMap::new(),
            query: &HashMap::new(),
            body: &[],
            framed: false,
        })
        .await?
        .unwrap();
    let payroll =
        services.get::<bilibi_finance_payroll::Payroll>(bilibi_finance_payroll::SERVICE)?;
    // Before the setup, the first account chosen waits on the page; the
    // second saves both together.
    let first =
        json!({"type":"save_account","field":"net_pay_account_id","id":"","value":id("2000")});
    let waiting = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&first),
        })
        .await?;
    assert!(waiting.html.contains("they are saved together"));
    assert!(payroll.get_settings(scope, company).await.is_err());
    let second = json!({"type":"save_account","field":"claims_payable_account_id","id":"","value":id("2000")});
    let saved = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&second),
        })
        .await?;
    assert!(!saved.html.contains("they are saved together"));
    assert!(payroll.get_settings(scope, company).await.is_ok());
    // A refusal in the "+ Mapping" dialog stays in the dialog, not the page.
    let open = json!({"type":"new_mapping"});
    state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&open),
        })
        .await?;
    let invalid = json!({"type":"save_mapping","mapping[kind]":"component","mapping[code]":"","mapping[direction]":"earning","mapping[expense_account_id]":id("5000")});
    let refused = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&invalid),
        })
        .await?;
    assert!(refused.html.contains("id=\"payroll-mapping-modal\""));
    assert!(refused.html.contains("payroll-mapping-code-error-0"));
    assert!(!refused.html.contains("id=\"flash-error\""));
    let stale = json!({"type":"clear-flash","key":"success"});
    let kept = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&stale),
        })
        .await?;
    assert_eq!(refused.html, kept.html);
    let close = json!({"type":"close_mapping"});
    let closed = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&close),
        })
        .await?;
    assert!(!closed.html.contains("id=\"payroll-mapping-modal\""));
    println!(
        "NU1: payroll accounts chosen in place save together; the mapping dialog keeps its refusal and closes"
    );
    Ok(())
}
