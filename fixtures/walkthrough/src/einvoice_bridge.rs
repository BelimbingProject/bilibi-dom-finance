//! The e-invoice ledger bridge section of the walkthrough (slice NE4), run
//! inside the clearance section while its stand-in authority and the real
//! submit and poll jobs are still serving. Company 002 keeps its book in USD,
//! so its MYR validations failed to post and wait on the reconciliation read.
//! Company 001 keeps its book in MYR: a validation posts its one owned entry
//! once the posting accounts are set (a retry posts what failed before); a
//! cancellation reverses it; a self-billed purchase posts to the payable side;
//! a month's sales no buyer asked an invoice for are consolidated, refused,
//! released and consolidated again; a document's PDF is generated once per
//! state and read back with its validation link's QR code; documents for the
//! transactions Receivables and Payables already posted link those entries
//! and never post themselves. The evidence datasets close it. Every printed
//! line is the facade's, the jobs' or the database's answer.
use crate::clearance::{
    buyer, d, draft, jobs_settle, save, show, shown, text, Outcome, CLIENT_ID, CLIENT_SECRET,
    COMPANY,
};
use bilibi_database::{Db, Error};
use bilibi_finance_einvoice::{
    qr_code_svg, CodeOverrideChange, ConsolidationRequest, DocumentQuery, DocumentView, Draft,
    DraftLine, DraftParty, DraftSource, Einvoice, EinvoiceError, FailedDocuments, Reconciliation,
    SERVICE,
};
use bilibi_finance_ledger::{
    Entry, EntryOwner, EntryQuery, Ledger, LedgerError, Money, NewEntry, NewLine, PostOptions,
};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::Target;
use bilibi_tenancy::Scope;
use jiff::civil::date;
use jiff::Timestamp;
use rust_decimal::Decimal;
use serde_json::json;

/// The company whose MYR book the documents post to, and whose Receivables
/// and Payables entries the linked documents name.
const LINKED_COMPANY: i64 = 1;

fn entry_line(entry: &Entry) -> String {
    let lines: Vec<String> = entry
        .lines
        .iter()
        .map(|line| {
            if line.debit.is_zero() {
                format!("account {} Cr {}", line.account_id, line.credit)
            } else {
                format!("account {} Dr {}", line.account_id, line.debit)
            }
        })
        .collect();
    format!(
        "entry #{} {} {:?} reference {:?} owner {} reverses {:?} [{}]",
        entry.entry_number,
        entry.entry_date,
        entry.description,
        entry.reference,
        entry
            .owner
            .as_ref()
            .map_or("none".into(), |o| format!("{} {}", o.module, o.record)),
        entry.reverses_entry_id,
        lines.join(", ")
    )
}

fn ledger_show<T: std::fmt::Debug>(step: &str, result: Result<T, LedgerError>) {
    match result {
        Ok(value) => println!("{step}: {value:?}"),
        Err(LedgerError::Invalid(errors)) => println!(
            "{step}: refused ({})",
            errors
                .iter()
                .map(|e| format!("{} {}", e.field, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}

fn ledger_side(view: &DocumentView) -> String {
    format!(
        "{} status={} ledger_entry={:?} reversal_entry={:?} linked={} attention={:?}",
        view.document.code_number,
        view.status,
        view.ledger_entry_id,
        view.reversal_entry_id,
        view.recognised_by_source,
        view.attention
    )
}

fn print_reconciliation(step: &str, result: Result<Reconciliation, EinvoiceError>) {
    match result {
        Ok(reconciliation) => {
            println!(
                "{step}: {} unposted, {} unreversed",
                reconciliation.unposted.len(),
                reconciliation.unreversed.len()
            );
            for (kind, rows) in [
                ("unposted", &reconciliation.unposted),
                ("unreversed", &reconciliation.unreversed),
            ] {
                for row in rows {
                    println!(
                        "  {kind} {} ({}, {}) entry={:?} reversal={:?} reason={:?}",
                        row.document.code_number,
                        row.document.status,
                        row.document.payable_amount,
                        row.ledger_entry_id,
                        row.reversal_entry_id,
                        row.reason
                    );
                }
            }
        }
        Err(error) => show::<()>(step, Err(error)),
    }
}

async fn print_entry(ledger: &Ledger, scope: &Scope, company: i64, id: Option<i64>) {
    if let Some(id) = id {
        match ledger.get_entry(scope, company, id).await {
            Ok(entry) => println!("  {}", entry_line(&entry)),
            Err(error) => println!("  entry {id}: {error:?}"),
        }
    }
}

/// A one-line document of `total` at no tax, so its total is what a source
/// entry recognised.
fn untaxed(code: &str, document_type: &str, total: Decimal) -> Draft {
    let mut draft = draft(code, None);
    draft.document_type = text(document_type);
    draft.lines = vec![DraftLine {
        tax_type: text("06"),
        tax_rate: Some(d("0")),
        unit_price: Some(total),
        quantity: Some(d("1")),
        ..draft.lines[0].clone()
    }];
    draft
}

/// A self-billed invoice: the counterparty is the supplier, the company's
/// own profile the buyer.
fn self_billed(mut draft: Draft) -> Draft {
    draft.document_type = text("11");
    draft.supplier = Some(DraftParty {
        name: text("Supplier Three Sdn Bhd"),
        industry_code: text("47111"),
        business_activity: text("Retail"),
        ..buyer()
    });
    draft.buyer = None;
    draft
}

fn issued_on(mut draft: Draft, at: &str) -> Draft {
    draft.issued_at = Some(at.parse().expect("a timestamp"));
    draft
}

async fn issue(einvoice: &Einvoice, scope: &Scope, company: i64, draft: &Draft) -> Outcome<i64> {
    Ok(einvoice
        .issue_document(scope, company, draft)
        .await?
        .document
        .id)
}

/// Submits a document and waits for its jobs.
async fn clear(
    einvoice: &Einvoice,
    db: &Db,
    scope: &Scope,
    company: i64,
    id: i64,
) -> Outcome<DocumentView> {
    einvoice.submit_document(scope, company, id).await?;
    jobs_settle(&**db.get().await?).await?;
    Ok(einvoice.get_document(scope, company, id).await?)
}

/// Finance home's failed counts and the documents list's "Still to act on"
/// rows in the same environment.
async fn still_to_act(einvoice: &Einvoice, scope: &Scope) -> Outcome<(FailedDocuments, Vec<i64>)> {
    let failed = einvoice.failed_documents(scope, LINKED_COMPANY).await?;
    let ids = einvoice
        .list_documents(
            scope,
            LINKED_COMPANY,
            &DocumentQuery {
                environment: failed.environment.clone(),
                needs_action: true,
                page_size: 300,
                ..DocumentQuery::default()
            },
        )
        .await?
        .entries
        .iter()
        .map(|d| d.id)
        .collect::<Vec<_>>();
    assert_eq!((failed.refused + failed.invalid) as usize, ids.len());
    Ok((failed, ids))
}

pub(crate) async fn run(
    db: &Db,
    services: &Services,
    manager: &Scope,
    viewer: &Scope,
) -> Outcome<()> {
    println!("\n== E-invoice ledger bridge (slice NE4)");
    let einvoice = services.get::<Einvoice>(SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;

    println!(
        "\n-- Company 002 keeps its book in USD: every MYR validation's posting failed and waits"
    );
    print_reconciliation(
        "reconciliation",
        einvoice.reconciliation(manager, COMPANY).await,
    );
    let first = einvoice
        .reconciliation(manager, COMPANY)
        .await?
        .unposted
        .last()
        .map(|row| row.document.id)
        .ok_or(Error::Internal)?;
    show(
        "viewer retries a posting",
        einvoice
            .retry_posting(viewer, COMPANY, first)
            .await
            .map(|view| ledger_side(&view)),
    );
    show(
        "manager retries it",
        einvoice
            .retry_posting(manager, COMPANY, first)
            .await
            .map(|view| ledger_side(&view)),
    );
    let linked = Target::Company(OrganisationId::new(LINKED_COMPANY)?);
    for (key, value) in [
        ("finance.einvoice.environment", json!("sandbox")),
        ("finance.einvoice.document_version", json!("1.0")),
        ("finance.einvoice.supplier.tax_id", json!("C2584563222")),
        (
            "finance.einvoice.supplier.registration_number",
            json!("201901234567"),
        ),
        ("finance.einvoice.supplier.industry_code", json!("47111")),
        (
            "finance.einvoice.supplier.business_activity",
            json!("Retail"),
        ),
        ("finance.einvoice.supplier.phone", json!("+60123456789")),
        (
            "finance.einvoice.supplier.address_line1",
            json!("1 Jalan Contoh"),
        ),
        ("finance.einvoice.supplier.city", json!("Kuala Lumpur")),
        ("finance.einvoice.supplier.state_code", json!("14")),
        ("finance.einvoice.supplier.country_code", json!("MYS")),
        ("finance.einvoice.sandbox.client_id", json!(CLIENT_ID)),
        (
            "finance.einvoice.sandbox.client_secret",
            json!(CLIENT_SECRET),
        ),
    ] {
        save(db, services, manager, linked, key, value).await?;
    }
    println!("manager saved Company 001's sandbox profile, credentials and version 1.0");

    println!(
        "\n-- Company 001 keeps its book in MYR: a validation posts once its accounts are set"
    );
    let book = ledger.get_book(manager, LINKED_COMPANY).await?;
    let chart = ledger.list_accounts(manager, LINKED_COMPANY).await?;
    println!(
        "Company 001's book: {} {}; chart {}",
        book.currency_code,
        book.minor_units,
        chart
            .iter()
            .map(|account| format!("{} {}", account.id, account.code))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "manager clears the receivable account: {}",
        save(
            db,
            services,
            manager,
            linked,
            "finance.einvoice.posting.receivable_account",
            serde_json::Value::Null,
        )
        .await?
    );
    let account = |code: &str| {
        chart
            .iter()
            .find(|account| account.code == code)
            .map_or(0, |account| account.id)
    };
    let first = issue(
        &einvoice,
        manager,
        LINKED_COMPANY,
        &draft("NE4-INV-0", None),
    )
    .await?;
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, first).await?;
    println!("NE4-INV-0 after its jobs: {}", ledger_side(&view));
    println!(
        "  its last event: {:?}",
        view.events
            .last()
            .map(|e| (e.event.clone(), e.reason.clone()))
    );
    save(
        db,
        services,
        manager,
        linked,
        "finance.einvoice.posting.receivable_account",
        json!("9999"),
    )
    .await?;
    show(
        "manager retries with a receivable account code the chart lacks",
        einvoice
            .retry_posting(manager, LINKED_COMPANY, first)
            .await
            .map(|view| ledger_side(&view)),
    );
    for (field, code) in [
        ("receivable_account", "1100"),
        ("revenue_account", "4000"),
        ("tax_account", "2100"),
        ("settlement_account", "1000"),
        ("payable_account", "2000"),
        ("expense_account", "5000"),
        ("purchase_tax_account", "2100"),
    ] {
        save(
            db,
            services,
            manager,
            linked,
            &format!("finance.einvoice.posting.{field}"),
            json!(code),
        )
        .await?;
    }
    println!(
        "manager set the posting accounts: receivable 1100, revenue 4000, tax 2100, settlement \
         1000, payable 2000, expense 5000, purchase tax 2100"
    );
    for row in einvoice
        .reconciliation(manager, LINKED_COMPANY)
        .await?
        .unposted
    {
        let result = einvoice
            .retry_posting(manager, LINKED_COMPANY, row.document.id)
            .await;
        let entry = result.as_ref().ok().and_then(|view| view.ledger_entry_id);
        show(
            &format!("retry posting {}", row.document.code_number),
            result.map(|view| ledger_side(&view)),
        );
        print_entry(&ledger, manager, LINKED_COMPANY, entry).await;
    }
    let again = einvoice.retry_posting(manager, LINKED_COMPANY, first).await;
    show(
        "retry the first one again (posts once)",
        again.map(|view| ledger_side(&view)),
    );
    show(
        "retry a reversal of a valid document",
        einvoice
            .retry_reversal(manager, LINKED_COMPANY, first)
            .await
            .map(|view| ledger_side(&view)),
    );
    let posted = einvoice
        .get_document(manager, LINKED_COMPANY, first)
        .await?;
    ledger_show(
        "the ledger refuses a second entry for the same document",
        ledger
            .post_entry(
                manager,
                LINKED_COMPANY,
                &NewEntry {
                    entry_date: date(2026, 10, 1),
                    description: "A second entry for the document".into(),
                    reference: None,
                    lines: vec![
                        NewLine::debit(account("1100"), Money::parse("1.00", book.minor_units)?),
                        NewLine::credit(account("4000"), Money::parse("1.00", book.minor_units)?),
                    ],
                },
                &PostOptions {
                    owner: Some(EntryOwner {
                        module: "finance/einvoice".into(),
                        record: format!("document:{first}"),
                    }),
                    settlement_account_ids: Vec::new(),
                },
            )
            .await
            .map(|entry| entry.entry_number),
    );
    ledger_show(
        "and a reversal of its entry by anyone but its owner",
        ledger
            .reverse_entry(
                manager,
                LINKED_COMPANY,
                posted.ledger_entry_id.unwrap_or_default(),
                &bilibi_finance_ledger::Reversal {
                    entry_date: date(2026, 10, 2),
                    description: "Not the owner's reversal".into(),
                    reference: None,
                },
            )
            .await
            .map(|entry| entry.entry_number),
    );
    print_reconciliation(
        "reconciliation after the retries",
        einvoice.reconciliation(manager, LINKED_COMPANY).await,
    );

    println!("\n-- A validation posts at once; a cancellation reverses");
    let sale = issue(
        &einvoice,
        manager,
        LINKED_COMPANY,
        &draft("NE4-INV-1", None),
    )
    .await?;
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, sale).await?;
    shown("NE4-INV-1 after its jobs", Ok(view.clone()));
    println!("  {}", ledger_side(&view));
    print_entry(&ledger, manager, LINKED_COMPANY, view.ledger_entry_id).await;
    let view = einvoice
        .cancel_document(manager, LINKED_COMPANY, sale, "Issued in error")
        .await?;
    shown("manager cancels NE4-INV-1", Ok(view.clone()));
    println!("  {}", ledger_side(&view));
    print_entry(&ledger, manager, LINKED_COMPANY, view.reversal_entry_id).await;
    show(
        "retry its reversal (reverses once)",
        einvoice
            .retry_reversal(manager, LINKED_COMPANY, sale)
            .await
            .map(|view| ledger_side(&view)),
    );
    show(
        "retry a posting of the cancelled document",
        einvoice
            .retry_posting(manager, LINKED_COMPANY, sale)
            .await
            .map(|view| ledger_side(&view)),
    );
    for (label, sql) in [
        (
            "INSERT a second posted event for NE4-INV-1",
            format!(
                "INSERT INTO finance_einvoice_status_events (tenant_id, company_id, document_id, \
                 event, actor_type, actor_user_id, ledger_entry_id) \
                 VALUES (1, {LINKED_COMPANY}, {sale}, 'posted', 'user', 1, 1)"
            ),
        ),
        (
            "INSERT a second posting_reversed event for NE4-INV-1",
            format!(
                "INSERT INTO finance_einvoice_status_events (tenant_id, company_id, document_id, \
                 event, actor_type, actor_user_id, ledger_entry_id) \
                 VALUES (1, {LINKED_COMPANY}, {sale}, 'posting_reversed', 'user', 1, 1)"
            ),
        ),
    ] {
        match db.get().await?.batch_execute(&sql).await {
            Ok(()) => println!("{label}: accepted"),
            Err(error) => println!(
                "{label}: refused by database — {}",
                error.as_db_error().map_or("unavailable", |e| e.message())
            ),
        }
    }

    println!("\n-- A self-billed purchase posts to the payable side");
    let purchase = issue(
        &einvoice,
        manager,
        LINKED_COMPANY,
        &self_billed(draft("NE4-SB-1", None)),
    )
    .await?;
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, purchase).await?;
    println!(
        "NE4-SB-1 (self-billed {}, counterparty {:?}): {}",
        view.self_billed,
        view.counterparty_name,
        ledger_side(&view)
    );
    print_entry(&ledger, manager, LINKED_COMPANY, view.ledger_entry_id).await;

    println!("\n-- The printable document and its validation link's QR code");
    let pdf = einvoice
        .generate_pdf(manager, LINKED_COMPANY, purchase)
        .await;
    match &pdf {
        Ok(stored) => println!("generate NE4-SB-1's PDF: {stored:?}"),
        Err(error) => println!("generate NE4-SB-1's PDF: refused ({error:?}: {error})"),
    }
    show(
        "generate it again (unchanged since)",
        einvoice
            .generate_pdf(manager, LINKED_COMPANY, purchase)
            .await,
    );
    if let Ok(pdf) = &pdf {
        show(
            "viewer reads it with a session from before joining Company 001",
            einvoice
                .read_pdf(viewer, LINKED_COMPANY, &pdf.artifact_id)
                .await
                .map(|file| file.filename),
        );
        let viewer = crate::sign_in(db, services, 1, 2).await?;
        match einvoice
            .read_pdf(&viewer, LINKED_COMPANY, &pdf.artifact_id)
            .await
        {
            Ok(file) => println!(
                "viewer signs in again and reads it: {} ({} bytes, starts {:?})",
                file.filename,
                file.bytes.len(),
                String::from_utf8_lossy(&file.bytes[..8.min(file.bytes.len())])
            ),
            Err(error) => show::<()>("viewer reads it", Err(error)),
        }
        show(
            "read a PDF id that is not stored",
            einvoice
                .read_pdf(manager, LINKED_COMPANY, "not-an-artifact")
                .await
                .map(|file| file.filename),
        );
    }
    let refreshed = einvoice
        .refresh_status(manager, LINKED_COMPANY, purchase)
        .await?;
    println!(
        "after a refresh adds {:?}, generate again: {:?}",
        refreshed.events.last().map(|event| event.event.clone()),
        einvoice
            .generate_pdf(manager, LINKED_COMPANY, purchase)
            .await
            .map(|pdf| pdf.artifact_id)
    );
    let view = einvoice
        .get_document(manager, LINKED_COMPANY, purchase)
        .await?;
    println!("the view's pdf_artifact_id: {:?}", view.pdf_artifact_id);
    match view
        .validation_url
        .as_deref()
        .and_then(|url| qr_code_svg(url, 160))
    {
        Some(svg) => println!(
            "QR code of {:?}: an SVG of {} bytes, script-free {}, starts {:?}",
            view.validation_url,
            svg.len(),
            !svg.contains("<script"),
            &svg[..svg.find('>').map_or(0, |end| end + 1)]
        ),
        None => println!("no QR code: the document has no validation link"),
    }
    println!("QR code of a blank text: {:?}", qr_code_svg("", 160));

    println!("\n-- A month's consolidated document (September 2026, judged at chosen instants)");
    let mut large = draft("NE4-SEP-LARGE", None);
    large.lines.truncate(1);
    large.lines[0].quantity = Some(d("100"));
    large.lines[0].unit_price = Some(d("200.00"));
    for code_draft in [
        issued_on(draft("NE4-SEP-1", None), "2026-09-05T02:00:00Z"),
        issued_on(draft("NE4-SEP-2", None), "2026-09-18T09:30:00Z"),
        issued_on(
            untaxed("NE4-SEP-3", "01", d("80.00")),
            "2026-09-30T15:59:59Z",
        ),
        issued_on(large, "2026-09-20T03:00:00Z"),
        issued_on(
            self_billed(draft("NE4-SEP-SB", None)),
            "2026-09-21T03:00:00Z",
        ),
        issued_on(draft("NE4-OCT-1", None), "2026-09-30T16:00:00Z"),
    ] {
        issue(&einvoice, manager, LINKED_COMPANY, &code_draft).await?;
    }
    let inside: Timestamp = "2026-10-03T00:00:00Z".parse()?;
    let month = date(2026, 9, 1);
    match einvoice
        .consolidation_candidates(manager, LINKED_COMPANY, month, inside)
        .await
    {
        Ok(candidates) => {
            println!(
                "candidates for {}: from {} until {} closes {} open {} totals {} + {} = {}",
                candidates.month,
                candidates.from,
                candidates.until,
                candidates.closes_at,
                candidates.open,
                candidates.line_extension_amount,
                candidates.tax_amount,
                candidates.payable_amount
            );
            for document in &candidates.documents {
                println!(
                    "  eligible {} issued {} {}",
                    document.code_number, document.issued_at, document.payable_amount
                );
            }
            for refused in &candidates.refused {
                println!(
                    "  refused {} {}",
                    refused.document.code_number, refused.reason
                );
            }
        }
        Err(error) => show::<()>("candidates", Err(error)),
    }
    let request = |code: &str, ids: Option<Vec<i64>>| ConsolidationRequest {
        month,
        code_number: text(code),
        document_ids: ids,
    };
    for (step, at, ids) in [
        ("issue on 29 September", "2026-09-29T00:00:00Z", None),
        (
            "issue on 8 October, after the window",
            "2026-10-08T00:00:00Z",
            None,
        ),
        (
            "issue naming a document that is not eligible",
            "2026-10-03T00:00:00Z",
            Some(vec![first]),
        ),
        (
            "issue naming no document",
            "2026-10-03T00:00:00Z",
            Some(vec![]),
        ),
    ] {
        show(
            step,
            einvoice
                .issue_consolidated_document(
                    manager,
                    LINKED_COMPANY,
                    &request("NE4-CONS-X", ids),
                    at.parse()?,
                )
                .await
                .map(|view| view.document.code_number),
        );
    }
    show(
        "viewer issues one",
        einvoice
            .issue_consolidated_document(
                viewer,
                LINKED_COMPANY,
                &request("NE4-CONS-X", None),
                inside,
            )
            .await
            .map(|view| view.document.code_number),
    );
    show(
        "manager issues it while Company 001 has retired state code 17 (Not Applicable)",
        einvoice
            .issue_consolidated_document(
                manager,
                LINKED_COMPANY,
                &request("NE4-CONS-REFUSE-1", None),
                inside,
            )
            .await
            .map(|view| view.document.code_number),
    );
    show(
        "manager reinstates state code 17",
        einvoice
            .override_code(
                manager,
                LINKED_COMPANY,
                "state",
                "17",
                &CodeOverrideChange {
                    description: None,
                    active: Some(true),
                },
            )
            .await,
    );
    let refused = shown(
        "manager issues NE4-CONS-REFUSE-1 for September",
        einvoice
            .issue_consolidated_document(
                manager,
                LINKED_COMPANY,
                &request("NE4-CONS-REFUSE-1", None),
                inside,
            )
            .await,
    )
    .ok_or(Error::Internal)?;
    for line in &refused.document.lines {
        println!(
            "  line {} {:?} {} tax {} {:?} = {}",
            line.line_number,
            line.description,
            line.line_extension_amount,
            line.tax_type,
            line.tax_rate,
            line.tax_amount
        );
    }
    println!(
        "  totals {} + {} = {}; sources {:?}",
        refused.document.line_extension_amount,
        refused.document.tax_amount,
        refused.document.payable_amount,
        refused
            .sources
            .iter()
            .map(|source| format!(
                "{} lines {:?} ({})",
                source.document.code_number, source.line_numbers, source.document.status
            ))
            .collect::<Vec<_>>()
    );
    let source = refused.sources[0].document.id;
    let covered = einvoice
        .get_document(manager, LINKED_COMPANY, source)
        .await?;
    println!(
        "source {} is {} and held by {:?}",
        covered.document.code_number,
        covered.status,
        covered
            .consolidated_by
            .as_ref()
            .map(|holder| holder.code_number.clone())
    );
    show(
        "submit the held source on its own",
        einvoice
            .submit_document(manager, LINKED_COMPANY, source)
            .await,
    );
    show(
        "issue a second consolidated document for the month",
        einvoice
            .issue_consolidated_document(
                manager,
                LINKED_COMPANY,
                &request("NE4-CONS-X", None),
                inside,
            )
            .await
            .map(|view| view.document.code_number),
    );
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, refused.document.id).await?;
    shown("NE4-CONS-REFUSE-1 after its jobs", Ok(view.clone()));
    let covered = einvoice
        .get_document(manager, LINKED_COMPANY, source)
        .await?;
    println!(
        "source {} is {} and held by {:?}",
        covered.document.code_number,
        covered.status,
        covered
            .consolidated_by
            .as_ref()
            .map(|holder| holder.code_number.clone())
    );
    show(
        "submit the refused consolidated document again",
        einvoice
            .submit_document(manager, LINKED_COMPANY, refused.document.id)
            .await,
    );
    let consolidated = einvoice
        .issue_consolidated_document(
            manager,
            LINKED_COMPANY,
            &request("NE4-CONS-1", None),
            inside,
        )
        .await?;
    let view = clear(
        &einvoice,
        db,
        manager,
        LINKED_COMPANY,
        consolidated.document.id,
    )
    .await?;
    shown(
        "NE4-CONS-1 covers the released sources again; after its jobs",
        Ok(view.clone()),
    );
    println!("  {}", ledger_side(&view));
    print_entry(&ledger, manager, LINKED_COMPANY, view.ledger_entry_id).await;

    println!("\n-- Company 001: documents for transactions Receivables and Payables recognised");
    let entries = ledger
        .list_entries(
            manager,
            LINKED_COMPANY,
            &EntryQuery {
                limit: Some(500),
                ..EntryQuery::default()
            },
        )
        .await?;
    let owned = |module: &str| {
        entries
            .iter()
            .find(|entry| {
                entry.owner.as_ref().is_some_and(|owner| {
                    owner.module == module && owner.record.starts_with("document:")
                }) && entry.reverses_entry_id.is_none()
                    && entry.reversed_by_entry_id.is_none()
            })
            .cloned()
    };
    let (Some(receivable), Some(payable)) =
        (owned("finance/receivables"), owned("finance/payables"))
    else {
        println!("no Receivables or Payables document entry to link");
        return Ok(());
    };
    let recognised =
        |entry: &Entry| -> Decimal { entry.lines.iter().map(|l| l.debit.amount()).sum() };
    let owner_of = |entry: &Entry| entry.owner.clone().expect("owned");
    println!("Receivables' {}", entry_line(&receivable));
    println!("Payables' {}", entry_line(&payable));
    let ar = owner_of(&receivable);
    let source = |module: &str, record: &str, entry: i64| {
        Some(DraftSource {
            module: text(module),
            record: text(record),
            ledger_entry_id: Some(entry),
        })
    };
    let total = recognised(&receivable);
    let attempts: Vec<(&str, Draft)> = vec![
        (
            "a source without a ledger entry",
            Draft {
                source: Some(DraftSource {
                    module: text(&ar.module),
                    record: text(&ar.record),
                    ledger_entry_id: None,
                }),
                ..untaxed("NE4-AR-X", "01", total)
            },
        ),
        (
            "a source module of another shape",
            Draft {
                source: source("Receivables", &ar.record, receivable.id),
                ..untaxed("NE4-AR-X", "01", total)
            },
        ),
        (
            "an entry the ledger does not hold",
            Draft {
                source: source(&ar.module, &ar.record, 999_999),
                ..untaxed("NE4-AR-X", "01", total)
            },
        ),
        (
            "Payables' entry named as a Receivables document",
            Draft {
                source: source(&ar.module, &ar.record, payable.id),
                ..untaxed("NE4-AR-X", "01", total)
            },
        ),
        (
            "an amount the entry does not recognise",
            Draft {
                source: source(&ar.module, &ar.record, receivable.id),
                ..untaxed("NE4-AR-X", "01", total + d("1.00"))
            },
        ),
    ];
    for (step, attempt) in attempts {
        show(
            step,
            einvoice
                .issue_document(manager, LINKED_COMPANY, &attempt)
                .await
                .map(|view| view.document.code_number),
        );
    }
    if let Some(reversed) = entries
        .iter()
        .find(|entry| entry.reversed_by_entry_id.is_some() && entry.owner.is_some())
    {
        let owner = owner_of(reversed);
        show(
            "an entry that was reversed",
            einvoice
                .issue_document(
                    manager,
                    LINKED_COMPANY,
                    &Draft {
                        source: source(&owner.module, &owner.record, reversed.id),
                        ..untaxed("NE4-AR-X", "01", recognised(reversed))
                    },
                )
                .await
                .map(|view| view.document.code_number),
        );
    }
    let linked_sale = Draft {
        source: source(&ar.module, &ar.record, receivable.id),
        ..untaxed("NE4-AR-1", "01", total)
    };
    let view = shown(
        "manager issues NE4-AR-1 for Receivables' document",
        einvoice
            .issue_document(manager, LINKED_COMPANY, &linked_sale)
            .await,
    )
    .ok_or(Error::Internal)?;
    println!(
        "  source links {:?}; {}",
        view.source_links,
        ledger_side(&view)
    );
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, view.document.id).await?;
    println!("NE4-AR-1 after its jobs: {}", ledger_side(&view));
    println!(
        "  its events: {:?}",
        view.events
            .iter()
            .map(|e| e.event.as_str())
            .collect::<Vec<_>>()
    );
    show(
        "retry its posting (the source module's accounting)",
        einvoice
            .retry_posting(manager, LINKED_COMPANY, view.document.id)
            .await
            .map(|view| ledger_side(&view)),
    );
    show(
        "a second document for the same source while NE4-AR-1 stands",
        einvoice
            .issue_document(
                manager,
                LINKED_COMPANY,
                &Draft {
                    code_number: text("NE4-AR-2"),
                    ..linked_sale.clone()
                },
            )
            .await
            .map(|view| view.document.code_number),
    );
    let mut note = untaxed("NE4-AR-CN-1", "02", d("10.00"));
    note.original_document_id = Some(view.document.id);
    show(
        "a credit note against NE4-AR-1 without its own source",
        einvoice
            .issue_document(manager, LINKED_COMPANY, &note)
            .await
            .map(|view| view.document.code_number),
    );
    let ap = owner_of(&payable);
    let view = shown(
        "manager issues self-billed NE4-AP-1 for Payables' bill",
        einvoice
            .issue_document(
                manager,
                LINKED_COMPANY,
                &self_billed(Draft {
                    source: source(&ap.module, &ap.record, payable.id),
                    ..untaxed("NE4-AP-1", "11", recognised(&payable))
                }),
            )
            .await,
    );
    if let Some(view) = view {
        println!(
            "  source links {:?}; {}",
            view.source_links,
            ledger_side(&view)
        );
    }
    println!("\n-- Failed documents still to act on (Finance home)");
    let (before, ids) = still_to_act(&einvoice, manager).await?;
    for consolidated in einvoice
        .list_documents(
            manager,
            LINKED_COMPANY,
            &DocumentQuery {
                search: "NE4-CONS-REFUSE-1".into(),
                ..DocumentQuery::default()
            },
        )
        .await?
        .entries
    {
        assert!(
            !ids.contains(&consolidated.id),
            "a consolidated document is counted"
        );
    }
    let bill = |code: &str| {
        self_billed(Draft {
            source: source(&ap.module, &ap.record, payable.id),
            ..untaxed(code, "11", recognised(&payable))
        })
    };
    let first = issue(&einvoice, manager, LINKED_COMPANY, &bill("NE4-AP-REFUSE-1")).await?;
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, first).await?;
    assert_eq!(view.status, "refused");
    let (failed, ids) = still_to_act(&einvoice, manager).await?;
    assert_eq!(failed.refused, before.refused + 1);
    assert!(ids.contains(&first));
    let second = issue(&einvoice, manager, LINKED_COMPANY, &bill("NE4-AP-REFUSE-2")).await?;
    let (failed, ids) = still_to_act(&einvoice, manager).await?;
    assert_eq!(failed.refused, before.refused + 1);
    assert!(ids.contains(&first) && !ids.contains(&second));
    println!("a newer issued document for the bill leaves the refused one to act on");
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, second).await?;
    assert_eq!(view.status, "refused");
    let (failed, ids) = still_to_act(&einvoice, manager).await?;
    assert_eq!(failed.refused, before.refused + 1);
    assert!(!ids.contains(&first) && ids.contains(&second));
    println!("a newer refused document for the bill is the one to act on");
    let third = issue(&einvoice, manager, LINKED_COMPANY, &bill("NE4-AP-2")).await?;
    let view = clear(&einvoice, db, manager, LINKED_COMPANY, third).await?;
    assert_eq!(view.status, "valid");
    let (failed, ids) = still_to_act(&einvoice, manager).await?;
    assert_eq!(failed.refused, before.refused);
    assert!(!ids.contains(&first) && !ids.contains(&second));
    println!("once a newer document for the bill stands, neither refused one is left to act on");
    print_reconciliation(
        "Company 001's reconciliation (linked documents never post)",
        einvoice.reconciliation(manager, LINKED_COMPANY).await,
    );

    println!("\n-- Evidence for the auditor pack");
    println!("datasets: {:?}", Einvoice::evidence_datasets());
    let viewer = &crate::sign_in(db, services, 1, 2).await?;
    for (scope, company_id, dataset, limit) in [
        (manager, LINKED_COMPANY, "status_event", Some(3)),
        (viewer, LINKED_COMPANY, "source_link", None),
        (manager, LINKED_COMPANY, "consolidated_source", None),
    ] {
        match einvoice
            .evidence_page(scope, company_id, dataset, limit, None)
            .await
        {
            Ok(page) => println!(
                "{dataset} of Company 00{company_id}: {} fields, {} rows, next {:?}; first {}",
                page.fields.len(),
                page.rows.len(),
                page.next,
                page.rows
                    .first()
                    .map_or("none".into(), |row| row.to_string())
            ),
            Err(error) => show::<()>(dataset, Err(error)),
        }
    }
    for (step, dataset, limit) in [
        ("an unknown dataset", "credentials", None),
        ("a limit of 501", "document", Some(501)),
    ] {
        show(
            step,
            einvoice
                .evidence_page(manager, LINKED_COMPANY, dataset, limit, None)
                .await
                .map(|page| page.rows.len()),
        );
    }
    let secrets: i64 = db.get().await?
        .query_one(
            "SELECT count(*) FROM finance_einvoice_submissions WHERE convert_from(payload, 'UTF8') \
             LIKE '%' || $1 || '%'",
            &[&CLIENT_SECRET],
        )
        .await?
        .get(0);
    println!("submission payloads carrying the client secret: {secrets}");
    Ok(())
}
