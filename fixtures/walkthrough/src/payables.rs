//! Running proof of Bilimbi.Finance.Payables NP1 through the real facades.
use crate::{sign_in, COMPANY};
use bilibi_authz::grants::Grants;
use bilibi_database::{Db, Error};
use bilibi_finance_attachments::{AttachmentError, Attachments, NewFile};
use bilibi_finance_ledger::{
    AccountChange, AccountType, ApprovalExceptionQuery, EntryQuery, Ledger, LedgerError,
    MinorUnits, Money, NewAccount, Reversal,
};
use bilibi_finance_payables::{
    DocumentLineInput, DocumentPageQuery, DocumentQuery, NewDocument, NewTaxCode, Payables,
    PayablesError, Settings, SettingsInput, TaxAmountInput, TaxCodeDetails, Tolerance,
    ToleranceKind, DOCUMENT_SOURCE,
};
use bilibi_module_registry::Services;
use bilibi_tenancy::{Authentication, Scope};
use jiff::civil::date;
use rust_decimal::Decimal;
use std::fmt::Debug;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
#[macro_export]
macro_rules! walkthrough_payables_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY,"walkthrough.fixture.finance.payables",$crate::registry::Provenance {module:"finance/payables",source:file!(),line:line!()},&$crate::authz::VocabularyContribution {
            domains:&[],verbs:&[],capabilities:&[],system_principals:&[],
            roles:&[
                $crate::authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_payables::CAPABILITIES},
                $crate::authz::RoleDefinition {code:"staff",name:"staff",description:"Walkthrough fixture role",grant_all:false,capabilities:&["finance.payables.view"]},
            ],
        }) }
    };
}
fn refused<T: Debug>(
    label: &str,
    r: Result<T, PayablesError>,
    expected: impl FnOnce(&PayablesError) -> bool,
) -> Outcome<()> {
    match r {
        Err(e) if expected(&e) => {
            println!("{label}: refused ({e:?})");
            Ok(())
        }
        other => Err(format!("{label}: unexpected {other:?}").into()),
    }
}
async fn db_refuses(c: &tokio_postgres::Client, label: &str, sql: &str) -> Outcome<()> {
    let result = c.batch_execute(sql).await;
    let _ = c.batch_execute("ROLLBACK").await;
    match result {
        Err(e) => {
            println!(
                "{label}: refused ({})",
                e.as_db_error()
                    .map_or_else(|| e.to_string(), |d| d.message().into())
            );
            Ok(())
        }
        Ok(()) => Err(format!("{label}: database accepted forbidden write").into()),
    }
}
fn draft(vendor: i64, number: &str, expense: i64, tax: i64, units: MinorUnits) -> NewDocument {
    NewDocument {
        vendor_id: Some(vendor),
        party_document_number: Some(number.into()),
        document_date: date(2026, 12, 15),
        due_date: None,
        description: Some("Fixture bill".into()),
        lines: vec![DocumentLineInput {
            description: "Supplies".into(),
            account_id: expense,
            amount: Money::parse("100.00", units).expect("fixture money"),
            tax_code_id: Some(tax),
            external_order_line_id: Some("caller-order-17".into()),
            order_line_id: None,
            quantity: None,
        }],
        tax_lines: None,
    }
}
async fn approve(p: &Payables, s: &Scope, id: i64) -> Outcome<()> {
    p.approve_document(s, COMPANY, id).await?;
    Ok(())
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    println!("\n== Payables NP1: configuration, documents and owned postings");
    let p = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let attachments = services.get::<Attachments>(bilibi_finance_attachments::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let manager = sign_in(db, services, 1, 1).await?;
    let vendor = crate::vendors::vendor(
        db,
        services,
        &manager,
        COMPANY,
        "supplier-001",
        "Fixture supplier",
    )
    .await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let other = sign_in(db, services, 2, 5).await?;
    let c = db.get().await?;
    let book = ledger.get_book(&manager, COMPANY).await?;
    let units = book.minor_units;
    let money = |s| Money::parse(s, units).expect("fixture money");
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    let find = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    let expense = find("5000")?;
    let payable = find("2000")?;
    let tax_account = find("2100")?;
    let second = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("2200", "Other payables", AccountType::Liability),
        )
        .await?;
    refused(
        "unconfigured",
        p.get_settings(&manager, COMPANY).await,
        |e| matches!(e, PayablesError::PayablesNotConfigured),
    )?;
    let config = SettingsInput {
        settings: Settings {
            payable_account_id: payable,
            quantity_tolerance: Some(Tolerance {
                kind: ToleranceKind::Percent,
                value: Decimal::from(2),
            }),
            price_tolerance: None,
        },
        reason: Some("Initial configuration".into()),
    };
    refused(
        "reader cannot configure",
        p.put_settings(&staff, COMPANY, &config).await,
        |e| matches!(e, PayablesError::Unauthorized),
    )?;
    let mut bad = config.clone();
    bad.settings.payable_account_id = expense;
    refused(
        "payable must be liability",
        p.put_settings(&manager, COMPANY, &bad).await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    p.put_settings(&manager, COMPANY, &config).await?;
    p.put_settings(&manager, COMPANY, &config).await?;
    assert_eq!(
        p.list_settings_changes(&manager, COMPANY, 0, 100)
            .await?
            .rows
            .len(),
        1
    );
    let tax = NewTaxCode {
        code: "INPUT".into(),
        details: TaxCodeDetails {
            name: "Input tax".into(),
            rate: "6".into(),
            account_id: tax_account,
            active: true,
            reason: Some("Initial tax code".into()),
        },
    };
    let code = p.create_tax_code(&manager, COMPANY, &tax).await?;
    refused(
        "duplicate tax code",
        p.create_tax_code(&manager, COMPANY, &tax).await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    let mut fine_tax = tax.clone();
    fine_tax.code = "FINE".into();
    fine_tax.details.rate = "1.00001".into();
    refused(
        "too fine tax rate",
        p.create_tax_code(&manager, COMPANY, &fine_tax).await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    let input = draft(vendor, "SUP-001", expense, code.id, units);
    refused(
        "reader cannot record",
        p.create_bill(&staff, COMPANY, &input).await,
        |e| matches!(e, PayablesError::Unauthorized),
    )?;
    refused("cross tenant", p.get_settings(&other, COMPANY).await, |e| {
        matches!(
            e,
            PayablesError::CompanyNotFound | PayablesError::Unauthorized
        )
    })?;
    refused(
        "company missing",
        p.get_settings(&admin, 99999).await,
        |e| matches!(e, PayablesError::CompanyNotFound),
    )?;
    refused(
        "archived company",
        p.create_bill(&admin, 3, &input).await,
        |e| matches!(e, PayablesError::CompanyArchived),
    )?;
    let mut bad = input.clone();
    bad.lines[0].amount = Money::parse("1.001", MinorUnits::new(4).ok_or(Error::Invalid)?)?;
    refused(
        "book scale",
        p.create_bill(&manager, COMPANY, &bad).await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    bad = input.clone();
    bad.lines[0].order_line_id = Some(1);
    bad.lines[0].quantity = Some(Decimal::ONE);
    refused(
        "absent order projection",
        p.create_bill(&manager, COMPANY, &bad).await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    bad = input.clone();
    bad.tax_lines = Some(vec![TaxAmountInput {
        tax_code_id: code.id + 999,
        tax_amount: money("1"),
    }]);
    refused(
        "unused explicit tax code",
        p.create_bill(&manager, COMPANY, &bad).await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    let bill = p.create_bill(&manager, COMPANY, &input).await?;
    assert_eq!(bill.total, money("106"));
    assert_eq!(bill.due_date, bill.document_date);
    assert!(bill.document_number.is_none());
    println!(
        "recorded: {} tax={} total={} due={} actor={}",
        bill.label(),
        bill.total_tax,
        bill.total,
        bill.due_date,
        bill.created_by_user_id
    );
    refused(
        "duplicate supplier number",
        p.create_bill(&manager, COMPANY, &input).await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    let mut details = tax.details.clone();
    details.rate = "8".into();
    details.reason = Some("New rate".into());
    p.update_tax_code(&manager, COMPANY, code.id, &details)
        .await?;
    p.update_tax_code(&manager, COMPANY, code.id, &details)
        .await?;
    assert_eq!(
        p.list_tax_code_changes(&manager, COMPANY, 0, 100)
            .await?
            .rows
            .len(),
        2
    );
    assert_eq!(
        p.get_document(&manager, COMPANY, bill.id).await?.tax_lines[0].rate,
        Decimal::from(6)
    );
    println!("tax snapshot kept at 6 after code changed to 8; no-op history unchanged");
    refused(
        "same-person approval",
        p.approve_document(&manager, COMPANY, bill.id).await,
        |e| matches!(e, PayablesError::Ledger(LedgerError::SamePerson)),
    )?;
    let auth = services.get::<Authentication>("base/tenancy.authentication")?;
    let as_manager = auth
        .authenticate_session(&**c, 1, 1, Some(4), None, String::new())
        .await?;
    refused(
        "impersonation approval",
        p.approve_document(&as_manager, COMPANY, bill.id).await,
        |e| matches!(e, PayablesError::Ledger(LedgerError::SamePerson)),
    )?;
    let file = NewFile {
        file_name: "bill.pdf".into(),
        content: b"%PDF-1.4 payables fixture".to_vec(),
        content_type: Some("application/pdf".into()),
    };
    let attachment = attachments
        .attach(&manager, COMPANY, DOCUMENT_SOURCE, bill.id, &file)
        .await?;
    let attachment = attachments
        .replace(&manager, COMPANY, attachment.id, &file)
        .await?;
    approve(&p, &admin, bill.id).await?;
    assert!(matches!(
        attachments
            .remove(&manager, COMPANY, attachment.id, "Cannot remove approved")
            .await,
        Err(AttachmentError::DocumentFinal)
    ));
    println!("attachment replace accepted while draft; remove refused after approval");
    // Fail the owner's update AFTER the entry was inserted. Both ledger
    // and audit must roll back and no document number may be consumed.
    let entries_before = ledger
        .list_entries(
            &manager,
            COMPANY,
            &EntryQuery {
                limit: Some(500),
                ..Default::default()
            },
        )
        .await?
        .len();
    let audits_before: i64 = c
        .query_one("SELECT count(*) FROM base_audit_mutations", &[])
        .await?
        .get(0);
    c.batch_execute(&format!("CREATE FUNCTION np1_refuse_post() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'NP1 injected document update failure' USING ERRCODE='check_violation'; END $$; CREATE TRIGGER np1_refuse_post BEFORE UPDATE ON finance_payables_documents FOR EACH ROW WHEN (NEW.id={} AND NEW.status='posted') EXECUTE FUNCTION np1_refuse_post()",bill.id)).await?;
    let failed = p.post_document(&manager, COMPANY, bill.id).await;
    c.batch_execute("DROP TRIGGER np1_refuse_post ON finance_payables_documents; DROP FUNCTION np1_refuse_post()").await?;
    refused("owner update failure", failed, |e| {
        matches!(e, PayablesError::Failed(_))
    })?;
    assert_eq!(
        ledger
            .list_entries(
                &manager,
                COMPANY,
                &EntryQuery {
                    limit: Some(500),
                    ..Default::default()
                }
            )
            .await?
            .len(),
        entries_before
    );
    assert_eq!(
        c.query_one("SELECT count(*) FROM base_audit_mutations", &[])
            .await?
            .get::<_, i64>(0),
        audits_before
    );
    assert_eq!(
        p.get_document(&manager, COMPANY, bill.id).await?.status,
        "approved"
    );
    let posted = p.post_document(&manager, COMPANY, bill.id).await?;
    assert_eq!(posted.document_number, Some(1));
    let entry = ledger
        .get_entry(&manager, COMPANY, posted.entry_id.ok_or(Error::NotFound)?)
        .await?;
    assert_eq!(
        entry.owner.as_ref().map(|o| o.record.as_str()),
        Some(format!("document:{}", bill.id).as_str())
    );
    println!(
        "posted {} atomically with ledger entry {} owned by {:?}",
        posted.label(),
        entry.entry_number,
        entry.owner
    );
    assert!(matches!(
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                entry.id,
                &Reversal {
                    entry_date: date(2026, 12, 16),
                    description: "Wrong correction path".into(),
                    reference: None
                }
            )
            .await,
        Err(LedgerError::OwnedBy(_))
    ));
    refused(
        "repeat posting",
        p.post_document(&manager, COMPANY, bill.id).await,
        |e| matches!(e, PayablesError::NotApproved),
    )?;
    refused(
        "posted void",
        p.void_document(&manager, COMPANY, bill.id, "Wrong").await,
        |e| matches!(e, PayablesError::NotVoidable),
    )?;
    let mut note = draft(vendor, "NOTE-001", expense, code.id, units);
    note.vendor_id = Some(i64::MAX); // A note always retains its original bill's vendor.
    note.lines[0].amount = money("10");
    let note = p
        .create_debit_note(&manager, COMPANY, bill.id, &note)
        .await?;
    assert_eq!(note.party.name, bill.party.name);
    approve(&p, &admin, note.id).await?;
    let mut config2 = config.clone();
    config2.settings.payable_account_id = second.id;
    config2.reason = Some("Changed control account".into());
    p.put_settings(&manager, COMPANY, &config2).await?;
    let note = p.post_document(&manager, COMPANY, note.id).await?;
    assert_eq!(note.payable_account_id, Some(payable));
    assert_eq!(note.document_number, Some(1));
    assert_eq!(
        p.get_document(&manager, COMPANY, bill.id)
            .await?
            .outstanding,
        Some(money("95.20"))
    );
    let mut excessive = draft(vendor, "EXCESS", expense, code.id, units);
    refused(
        "debit note too large",
        p.create_debit_note(&manager, COMPANY, bill.id, &excessive)
            .await,
        |e| matches!(e, PayablesError::ExceedsOutstanding),
    )?;
    excessive.lines[0].amount = money("80");
    let n1 = p
        .create_debit_note(&manager, COMPANY, bill.id, &excessive)
        .await?;
    let n2 = p
        .create_debit_note(&manager, COMPANY, bill.id, &excessive)
        .await?;
    approve(&p, &admin, n1.id).await?;
    approve(&p, &admin, n2.id).await?;
    p.post_document(&manager, COMPANY, n1.id).await?;
    refused(
        "outstanding checked again at posting",
        p.post_document(&manager, COMPANY, n2.id).await,
        |e| matches!(e, PayablesError::ExceedsOutstanding),
    )?;
    let voided = p
        .create_bill(
            &manager,
            COMPANY,
            &draft(vendor, "VOID-001", expense, code.id, units),
        )
        .await?;
    refused(
        "void needs reason",
        p.void_document(&admin, COMPANY, voided.id, " ").await,
        |e| matches!(e, PayablesError::Invalid(_)),
    )?;
    p.void_document(&admin, COMPANY, voided.id, "Entered twice")
        .await?;
    let replacement = p
        .create_bill(
            &manager,
            COMPANY,
            &draft(vendor, "VOID-001", expense, code.id, units),
        )
        .await?;
    assert_ne!(replacement.id, voided.id);
    ledger
        .set_duty_separation(&manager, COMPANY, false, Some("Fixture small team"))
        .await?;
    approve(&p, &manager, replacement.id).await?;
    assert!(ledger
        .list_approval_exceptions(&manager, COMPANY, &ApprovalExceptionQuery::default())
        .await?
        .iter()
        .any(|e| e.source == "finance/payables" && e.subject_id == replacement.id));
    ledger
        .set_duty_separation(&manager, COMPANY, true, None)
        .await?;
    println!("same-person approval while duties disabled recorded an approval exception");
    let stale = p
        .create_bill(
            &manager,
            COMPANY,
            &draft(vendor, "STALE-001", expense, code.id, units),
        )
        .await?;
    approve(&p, &admin, stale.id).await?;
    grants
        .set_direct(
            &admin,
            1,
            bilibi_finance_payables::POST_DOCUMENTS,
            Some(false),
        )
        .await?;
    refused(
        "revoked capability on stale scope",
        p.post_document(&manager, COMPANY, stale.id).await,
        |e| matches!(e, PayablesError::Unauthorized),
    )?;
    grants
        .set_direct(&admin, 1, bilibi_finance_payables::POST_DOCUMENTS, None)
        .await?;
    let (a, b) = tokio::join!(
        p.post_document(&manager, COMPANY, stale.id),
        p.post_document(&admin, COMPANY, stale.id)
    );
    assert!(matches!(
        (&a, &b),
        (Ok(_), Err(PayablesError::NotApproved)) | (Err(PayablesError::NotApproved), Ok(_))
    ));
    println!(
        "concurrent posting: one committed, one refused ({:?}, {:?})",
        a.as_ref().map(|d| d.document_number),
        b.as_ref().map(|d| d.document_number)
    );
    let retired = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("5800", "Inactive-after-draft expense", AccountType::Expense),
        )
        .await?;
    let inactive = p
        .create_bill(
            &manager,
            COMPANY,
            &draft(vendor, "INACTIVE", retired.id, code.id, units),
        )
        .await?;
    approve(&p, &admin, inactive.id).await?;
    ledger
        .update_account(
            &manager,
            COMPANY,
            retired.id,
            &AccountChange {
                active: Some(false),
                ..Default::default()
            },
        )
        .await?;
    refused(
        "account deactivated after recording",
        p.post_document(&manager, COMPANY, inactive.id).await,
        |e| matches!(e, PayablesError::Ledger(LedgerError::Invalid(_))),
    )?;
    let mut early = draft(vendor, "NO-PERIOD", expense, code.id, units);
    early.document_date = date(2027, 1, 1);
    let early = p.create_bill(&manager, COMPANY, &early).await?;
    approve(&p, &admin, early.id).await?;
    refused(
        "actual document date outside fiscal periods",
        p.post_document(&manager, COMPANY, early.id).await,
        |e| matches!(e, PayablesError::Ledger(LedgerError::Invalid(_))),
    )?;
    let page = p
        .page_documents(
            &manager,
            COMPANY,
            &DocumentPageQuery {
                search: "fixture supplier".into(),
                ..Default::default()
            },
        )
        .await?;
    assert!(page.total > 0);
    assert!(page
        .documents
        .windows(2)
        .all(|d| (d[0].document_date.as_str(), d[0].id) > (d[1].document_date.as_str(), d[1].id)));
    let first = p
        .list_documents(
            &manager,
            COMPANY,
            &DocumentQuery {
                limit: 1,
                ..Default::default()
            },
        )
        .await?;
    let last = first.last().ok_or(Error::NotFound)?;
    let following = p
        .list_documents(
            &manager,
            COMPANY,
            &DocumentQuery {
                after: Some((last.document_date, last.id)),
                limit: 1,
                ..Default::default()
            },
        )
        .await?;
    assert!(following.iter().all(|d| d.id != last.id));
    println!(
        "paged reads: newest-first search returned {} documents; keyset cursor advanced",
        page.total
    );
    db_refuses(
        &c,
        "UPDATE recorded line",
        &format!(
            "UPDATE finance_payables_document_lines SET description='Changed' WHERE document_id={}",
            bill.id
        ),
    )
    .await?;
    db_refuses(
        &c,
        "DELETE document",
        &format!(
            "DELETE FROM finance_payables_documents WHERE id={}",
            bill.id
        ),
    )
    .await?;
    db_refuses(
        &c,
        "TRUNCATE history",
        "TRUNCATE finance_payables_settings_changes",
    )
    .await?;
    db_refuses(
        &c,
        "edit immutable header",
        &format!(
            "UPDATE finance_payables_documents SET description='Changed' WHERE id={}",
            replacement.id
        ),
    )
    .await?;
    db_refuses(&c,"append line in later transaction",&format!("INSERT INTO finance_payables_document_lines(tenant_id,company_id,document_id,line_number,description,account_id,amount) VALUES(1,1,{},2,'Late',{},1)",inactive.id,expense)).await?;
    db_refuses(
        &c,
        "tax code identity",
        &format!(
            "UPDATE finance_payables_tax_codes SET code='Changed' WHERE id={}",
            code.id
        ),
    )
    .await?;
    let mut rounded = draft(vendor, "ROUND-HALF", expense, code.id, units);
    let rounding_code = p
        .create_tax_code(
            &manager,
            COMPANY,
            &NewTaxCode {
                code: "HALF".into(),
                details: TaxCodeDetails {
                    name: "Rounding proof".into(),
                    rate: "10".into(),
                    account_id: tax_account,
                    active: true,
                    reason: None,
                },
            },
        )
        .await?;
    rounded.lines[0].tax_code_id = Some(rounding_code.id);
    rounded.lines[0].amount = money("0.05");
    let rounded = p.create_bill(&manager, COMPANY, &rounded).await?;
    assert_eq!(rounded.total_tax, money("0.01"));
    let mut stated = draft(vendor, "STATED-TAX", expense, code.id, units);
    stated.tax_lines = Some(vec![TaxAmountInput {
        tax_code_id: code.id,
        tax_amount: money("7.99"),
    }]);
    let stated = p.create_bill(&manager, COMPANY, &stated).await?;
    assert_eq!(stated.total_tax, money("7.99"));
    grants
        .set_direct(&admin, 3, bilibi_finance_ledger::VIEW, Some(false))
        .await?;
    assert_eq!(p.get_document(&staff, COMPANY, bill.id).await?.id, bill.id);
    grants
        .set_direct(&admin, 3, bilibi_finance_ledger::VIEW, None)
        .await?;
    println!(
        "tax half rounds up, party tax overrides computed tax, payables-only reader needs no ledger grant"
    );
    db_refuses(&c,"append to recorded draft",&format!("INSERT INTO finance_payables_document_lines(tenant_id,company_id,document_id,line_number,description,account_id,amount) VALUES(1,1,{},2,'Late draft line',{},1)",stated.id,expense)).await?;
    let header = |company: i64, number: &str| {
        format!(
            "INSERT INTO finance_payables_documents(tenant_id,company_id,document_type,status,party_key,party_name,party_document_number,document_date,due_date,currency_code,total_net,total_tax,total,created_by_user_id) VALUES(1,{company},'bill','draft','raw-party','Raw party','{number}','2026-12-15','2026-12-15','MYR',1,0,1,1)"
        )
    };
    db_refuses(
        &c,
        "deferred document without lines",
        &format!("BEGIN; {}; COMMIT", header(1, "RAW-NO-LINES")),
    )
    .await?;
    db_refuses(&c,"deferred totals mismatch",&format!("BEGIN; WITH d AS ({} RETURNING id) INSERT INTO finance_payables_document_lines(tenant_id,company_id,document_id,line_number,description,account_id,amount) SELECT 1,1,id,1,'Wrong total',{},0.5 FROM d; COMMIT",header(1,"RAW-TOTALS"),expense)).await?;
    db_refuses(&c,"cross-company tax code",&format!("BEGIN; WITH d AS ({} RETURNING id) INSERT INTO finance_payables_document_lines(tenant_id,company_id,document_id,line_number,description,account_id,amount,tax_code_id) SELECT 1,2,id,1,'Foreign tax',{},1,{} FROM d; COMMIT",header(2,"RAW-FOREIGN-TAX"),expense,code.id)).await?;
    let history = p.list_settings_changes(&manager, COMPANY, 0, 1).await?;
    assert_eq!(history.rows.len(), 1);
    assert!(history.next_after.is_some());
    assert_eq!(
        p.list_settings_changes(
            &manager,
            COMPANY,
            history.next_after.ok_or(Error::NotFound)?,
            1
        )
        .await?
        .rows
        .len(),
        1
    );
    println!("Payables NP1 walkthrough complete");
    Ok(())
}
