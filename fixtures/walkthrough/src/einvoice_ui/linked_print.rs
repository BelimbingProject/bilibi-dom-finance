use super::{draft, mount, pdf_text, Outcome};
use bilibi_authz::grants::{Direct, Grants};
use bilibi_database::{Db, Error};
use bilibi_finance_einvoice::{Draft, DraftSource, Einvoice};
use bilibi_finance_ledger::{AccountType, Ledger, Money, NewAccount};
use bilibi_finance_payables::{DocumentLineInput, NewDocument, Payables, Settings, SettingsInput};
use bilibi_finance_receivables::Receivables;
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use bilibi_ui::{download::*, page::*};
use serde_json::json;
use std::collections::HashMap;

pub(super) async fn prove(services: &Services, manager: &Scope, admin: &Scope) -> Outcome<()> {
    const FISCAL: &str = "NE5-LINKED-FISCAL";
    const SOURCE: &str = "NE5-SOURCE-001";
    const SOURCE_VIEW: &str = "finance.receivables.view";
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let control = ledger
        .create_account(
            admin,
            1,
            &NewAccount::new("NE5-control", "Receivables", AccountType::Asset),
        )
        .await?;
    let income = ledger
        .create_account(
            admin,
            1,
            &NewAccount::new("NE5-income", "Sales", AccountType::Income),
        )
        .await?;
    ar.configure(
        admin,
        1,
        &json!({"receivable_account_id": control.id, "income_account_id": income.id}),
    )
    .await?;
    let series = ar.create_series(admin, 1, &json!({
        "code":"NE5-SOURCE", "name":"NE5 source", "kind":"invoice", "prefix":"NE5-SOURCE-", "number_width":3,
    })).await?;
    let customer = ar
        .create_customer(admin, 1, &json!({"record_kind":"company","record_id":20}))
        .await?;
    let invoice = ar.create_draft(admin, 1, &json!({
        "kind":"invoice", "series_id":series.id, "customer_id":customer.id, "issue_date":"2026-09-01",
        "lines":[{"description":"Source sale","quantity":"1","unit_price":"275.60"}],
    })).await?;
    let invoice = ar.issue_document(admin, 1, invoice.id).await?;
    assert_eq!(invoice.document_reference.as_deref(), Some(SOURCE));
    let mut fiscal = draft(FISCAL, None);
    fiscal.billing_period_start = Some("2026-09-01".parse()?);
    fiscal.billing_period_end = Some("2026-09-30".parse()?);
    fiscal.source = Some(DraftSource {
        module: Some("finance/receivables".into()),
        record: Some(format!("document:{}", invoice.id)),
        ledger_entry_id: invoice.ledger_entry_id,
    });
    prove_link(services, manager, admin, &fiscal, SOURCE, SOURCE_VIEW).await
}

pub(super) async fn prove_payables(
    db: &Db,
    services: &Services,
    manager: &Scope,
    admin: &Scope,
) -> Outcome<()> {
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let payables = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let vendor = crate::vendors::vendor(
        db,
        services,
        manager,
        1,
        "ne5-payables",
        "Distinct Supplier",
    )
    .await?;
    let chart = ledger.list_accounts(manager, 1).await?;
    let account = |code: &str| {
        chart
            .iter()
            .find(|account| account.code == code)
            .map(|account| account.id)
            .ok_or(Error::NotFound)
    };
    let units = ledger.get_book(manager, 1).await?.minor_units;
    payables
        .put_settings(
            manager,
            1,
            &SettingsInput {
                settings: Settings {
                    payable_account_id: account("2000")?,
                    quantity_tolerance: None,
                    price_tolerance: None,
                },
                reason: Some("Fixture posting accounts".into()),
            },
        )
        .await?;
    let bill = payables
        .create_bill(
            manager,
            1,
            &NewDocument {
                vendor_id: Some(vendor),
                party_document_number: Some("NE5-SUPPLIER-REF".into()),
                document_date: "2026-09-01".parse()?,
                due_date: None,
                description: None,
                lines: vec![DocumentLineInput {
                    description: "Self-billed supplies".into(),
                    account_id: account("5000")?,
                    amount: Money::parse("275.60", units)?,
                    tax_code_id: None,
                    external_order_line_id: None,
                    order_line_id: None,
                    quantity: None,
                }],
                tax_lines: None,
            },
        )
        .await?;
    let bill = crate::payables::approve_then_post(&payables, admin, manager, 1, bill.id).await?;
    assert_eq!(bill.document_number, Some(1));
    let mut fiscal = draft("NE5-LINKED-SELF", None);
    fiscal.document_type = Some("11".into());
    let mut supplier = fiscal.buyer.take().ok_or("missing fixture supplier")?;
    supplier.name = Some("Distinct Supplier".into());
    supplier.industry_code = Some("47111".into());
    supplier.business_activity = Some("Retail".into());
    fiscal.supplier = Some(supplier);
    fiscal.billing_period_start = Some("2026-09-01".parse()?);
    fiscal.billing_period_end = Some("2026-09-30".parse()?);
    fiscal.source = Some(DraftSource {
        module: Some(bilibi_finance_payables::MODULE.into()),
        record: Some(format!("document:{}", bill.id)),
        ledger_entry_id: bill.entry_id,
    });
    prove_link(
        services,
        manager,
        admin,
        &fiscal,
        "Bill 1",
        bilibi_finance_payables::VIEW,
    )
    .await
}

async fn prove_link(
    services: &Services,
    manager: &Scope,
    admin: &Scope,
    fiscal: &Draft,
    source: &str,
    source_view: &str,
) -> Outcome<()> {
    let reference = fiscal
        .code_number
        .as_deref()
        .ok_or("missing fixture fiscal reference")?;
    let einvoice = services.get::<Einvoice>(bilibi_finance_einvoice::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let downloads = services.get::<DownloadService<Scope, Error>>("finance/einvoice.files")?;
    let id = einvoice
        .issue_document(manager, 1, fiscal)
        .await?
        .document
        .id;
    let named = einvoice.generate_pdf(manager, 1, id).await?.artifact_id;
    let file = einvoice.read_pdf(manager, 1, &named).await?;
    let text = pdf_text(&file.bytes)?;
    assert!(
        text.contains(reference),
        "missing fiscal reference {reference}: {text}"
    );
    assert!(
        text.contains(source),
        "missing posted source reference {source}: {text}"
    );
    assert!(
        text.contains("1 Sep 2026") && text.contains("30 Sep 2026"),
        "{text}"
    );
    let named_path = format!("/finance/einvoice/documents/{id}/pdf/{named}");
    assert!(detail(services, manager, id).await?.contains(&named_path));

    grants
        .set_direct(admin, 1, source_view, Some(Direct::Deny))
        .await?;
    assert!(!detail(services, manager, id)
        .await?
        .contains("einvoice-document-download-pdf"));
    let path = HashMap::from([
        ("id".into(), id.to_string()),
        ("artifact_id".into(), named.clone()),
    ]);
    assert!(matches!(
        downloads
            .0
            .serve(DownloadRequest {
                operation: "pdf",
                scope: manager,
                path: &path,
                query: &HashMap::new(),
            })
            .await,
        Err(Error::Forbidden)
    ));
    let masked = einvoice.generate_pdf(manager, 1, id).await?.artifact_id;
    assert_ne!(named, masked);
    let file = einvoice.read_pdf(manager, 1, &masked).await?;
    let text = pdf_text(&file.bytes)?;
    assert!(
        text.contains(reference) && !text.contains(source) && !text.contains("document:"),
        "{text}"
    );
    assert!(detail(services, manager, id)
        .await?
        .contains(&format!("/finance/einvoice/documents/{id}/pdf/{masked}")));

    grants.set_direct(admin, 1, source_view, None).await?;
    assert!(detail(services, manager, id).await?.contains(&named_path));
    assert_eq!(
        einvoice.generate_pdf(manager, 1, id).await?.artifact_id,
        named
    );
    Ok(())
}

async fn detail(services: &Services, scope: &Scope, id: i64) -> Outcome<String> {
    let mut page = mount(services, scope, Some(id)).await?;
    Ok(page
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?
        .html)
}
