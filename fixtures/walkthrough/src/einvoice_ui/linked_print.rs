use super::{draft, mount, pdf_text, Outcome};
use bilibi_authz::grants::{Direct, Grants};
use bilibi_database::Error;
use bilibi_finance_einvoice::{DraftSource, Einvoice};
use bilibi_finance_ledger::{AccountType, Ledger, NewAccount};
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
    let einvoice = services.get::<Einvoice>(bilibi_finance_einvoice::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let downloads = services.get::<DownloadService<Scope, Error>>("finance/einvoice.files")?;
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
    let id = einvoice
        .issue_document(manager, 1, &fiscal)
        .await?
        .document
        .id;
    let named = einvoice.generate_pdf(manager, 1, id).await?.artifact_id;
    let file = einvoice.read_pdf(manager, 1, &named).await?;
    let text = pdf_text(&file.bytes)?;
    assert!(text.contains(FISCAL) && text.contains(SOURCE), "{text}");
    assert!(
        text.contains("1 Sep 2026") && text.contains("30 Sep 2026"),
        "{text}"
    );
    let named_path = format!("/finance/einvoice/documents/{id}/pdf/{named}");
    assert!(detail(services, manager, id).await?.contains(&named_path));

    grants
        .set_direct(admin, 1, SOURCE_VIEW, Some(Direct::Deny))
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
        text.contains(FISCAL) && !text.contains(SOURCE) && !text.contains("document:"),
        "{text}"
    );
    assert!(detail(services, manager, id)
        .await?
        .contains(&format!("/finance/einvoice/documents/{id}/pdf/{masked}")));

    grants.set_direct(admin, 1, SOURCE_VIEW, None).await?;
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
