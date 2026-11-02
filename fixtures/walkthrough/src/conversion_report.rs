//! Real contracts and immutable evidence, exercised around the existing
//! opening-register corrections and backdated settlement lifecycle.
use bilibi_agent_api::plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans};
use bilibi_database::Db;
use bilibi_finance_attachments::NewFile;
use bilibi_finance_ledger::{Ledger, Money, NewEntry, NewLine};
use bilibi_finance_reconciliation::{NewMatch, NewReconciliation, Reconciliation};
use bilibi_finance_reports::Reports;

use bilibi_module_registry::Services;
use bilibi_tenancy::{Authentication, ProofFuture};
use jiff::civil::date;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::Transaction;
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
struct Authority {
    authentication: Arc<Authentication>,
    user: i64,
}
impl PlanAuthority for Authority {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            Ok(PlanIdentity {
                scope: self
                    .authentication
                    .authenticate_session(tx, 1, self.user, None, Some(COMPANY), String::new())
                    .await?,
                credential_binding: format!("conversion-fixture:{}", self.user),
            })
        })
    }
}
pub struct Kept {
    pub id: i64,
    pub bank_id: i64,
    pub opening_item_id: i64,
    pub content: Value,
}
const COMPANY: i64 = 3;
const VIEWS: &[&str] = &[
    bilibi_finance_ledger::VIEW,
    bilibi_finance_receivables::VIEW,
    bilibi_finance_payables::VIEW,
    bilibi_finance_reconciliation::VIEW,
    bilibi_finance_payroll::VIEW,
    bilibi_finance_sst::VIEW,
    bilibi_finance_fixed_assets::VIEW,
    bilibi_finance_reports::SIGN_OFF,
    "admin.company.tenant-wide.manage",
];
fn issue() -> PlannedOperation {
    PlannedOperation {
        key: "finance.reports.conversion.issue".into(),
        input: json!({"company_id":COMPANY,"as_of":"2026-08-01","reviewer_reference":"Source closing pack CONVERSION-3; reviewer workpaper C-109","note":"Review unresolved control postings and the external payroll and tax schedules. No historical completeness attestation."}),
    }
}
fn opening_import(
    bank: i64,
    reference: &str,
    statement_balance: &str,
    csv: &str,
    replaces: Option<i64>,
) -> PlannedOperation {
    let mut input = json!({
        "company_id": COMPANY,
        "bank_account_id": bank,
        "source_reference": reference,
        "statement_source": "Source bank statement at conversion",
        "statement_balance": statement_balance,
        "currency_code": "MYR",
        "csv": csv,
    });
    if let Some(replaces) = replaces {
        input["replaces_bridge_id"] = replaces.into();
        input["reason"] = "Corrected outstanding deposit amount from source statement".into();
    }
    PlannedOperation {
        key: "finance.reconciliation.opening.import".into(),
        input,
    }
}
pub async fn before(db: &Db, services: &Services) -> Outcome<Kept> {
    let s = crate::in_company(db, services, 4, COMPANY).await?;
    let auth = services.get::<Authentication>("base/tenancy.authentication")?;
    let authority = Authority {
        authentication: auth,
        user: 4,
    };
    let plans = services.get::<Plans>("base/agent_api.plans")?;
    let reports = services.get::<Reports>(bilibi_finance_reports::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let reconciliation = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let bank_account_id = ledger
        .list_accounts(&s, COMPANY)
        .await?
        .into_iter()
        .find(|account| account.code == "1000")
        .ok_or("bank ledger account")?
        .id;
    let bank = reconciliation
        .list_bank_accounts(&s, COMPANY)
        .await?
        .into_iter()
        .find(|bank| bank.ledger_account_id == bank_account_id)
        .ok_or("conversion bank account")?;
    let registry = services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let first = [opening_import(
        bank.id,
        "BANK-OPENING-V1",
        "9400",
        "source_id,item_date,description,amount\ndeposit-in-transit,2026-06-30,Deposit in transit,500\nlate-clearing,2026-06-30,Later clearing item,100\n",
        None,
    )];
    let review = plans.preview(&authority, &first).await?;
    assert_eq!(review["valid"], true, "{review}");
    plans.commit(&s, &authority, &first).await?;
    let first_bridge = registry
        .read(
            db,
            &s,
            "finance.reconciliation.opening.current",
            &json!({"company_id":COMPANY,"bank_account_id":bank.id}),
        )
        .await?["result"]["bridge_id"]
        .as_i64()
        .ok_or("first opening bank bridge")?;
    let corrected = [opening_import(
        bank.id,
        "BANK-OPENING-V2",
        "9300",
        "source_id,item_date,description,amount\ndeposit-in-transit,2026-06-30,Corrected deposit in transit,600\nlate-clearing,2026-06-30,Later clearing item,100\n",
        Some(first_bridge),
    )];
    let review = plans.preview(&authority, &corrected).await?;
    assert_eq!(review["valid"], true, "{review}");
    plans.commit(&s, &authority, &corrected).await?;
    let opening = registry
        .read(
            db,
            &s,
            "finance.reconciliation.opening.context",
            &json!({"company_id":COMPANY,"bank_account_id":bank.id}),
        )
        .await?["result"]
        .clone();
    let opening_item = opening["items"][0]["id"]
        .as_i64()
        .ok_or("corrected opening bank item")?;
    let later_opening_item = opening["items"][1]["id"]
        .as_i64()
        .ok_or("later opening bank item")?;
    let grants = services.get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    for cap in VIEWS {
        grants.set_direct(&s, 2, cap, Some(true)).await?;
    }
    grants
        .set_direct(
            &s,
            2,
            bilibi_finance_reconciliation::IMPORT_STATEMENTS,
            Some(true),
        )
        .await?;
    grants
        .set_direct(&s, 2, bilibi_finance_attachments::CREATE, Some(true))
        .await?;
    for unrelated in [
        bilibi_finance_attachments::VIEW,
        bilibi_finance_einvoice::VIEW,
    ] {
        grants.set_direct(&s, 2, unrelated, Some(false)).await?;
    }
    let viewer = crate::in_company(db, services, 2, COMPANY).await?;
    let statement = reconciliation
        .import_statement(
            &viewer,
            COMPANY,
            bank.id,
            &NewFile {
                file_name: "conversion-opening-statement.csv".into(),
                content: b"Date,Details,Ref,Amount,Balance\n2026-07-02,Outstanding deposit,OPENING-DEPOSIT,600,9900\n".to_vec(),
                content_type: Some("text/csv".into()),
            },
        )
        .await?;
    let opening_match = [PlannedOperation {
        key: "finance.reconciliation.opening.match".into(),
        input: json!({
            "company_id": COMPANY,
            "bank_account_id": bank.id,
            "opening_item_id": opening_item,
            "statement_line_id": statement.statement.lines[0].id,
        }),
    }];
    let review = plans.preview(&authority, &opening_match).await?;
    assert_eq!(review["valid"], true, "{review}");
    plans.commit(&s, &authority, &opening_match).await?;
    let opened = reconciliation
        .open_reconciliation(
            &s,
            COMPANY,
            bank.id,
            &NewReconciliation {
                end_date: Some(date(2026, 8, 1)),
                statement_balance: "9900".into(),
            },
        )
        .await?;
    for capability in [
        bilibi_finance_ledger::VIEW,
        bilibi_finance_reconciliation::VIEW,
        bilibi_finance_reconciliation::FINALISE_RECONCILIATIONS,
    ] {
        grants.set_direct(&s, 1, capability, Some(true)).await?;
    }
    let manager = crate::in_company(db, services, 1, COMPANY).await?;
    reconciliation
        .finalise_reconciliation(&manager, COMPANY, opened.id)
        .await?;
    let preview = registry
        .read(
            db,
            &s,
            "finance.reports.conversion.preview",
            &json!({"company_id":COMPANY,"as_of":"2026-08-01"}),
        )
        .await?["result"]
        .clone();
    assert!(preview["postings_by_owner"]["rows"]
        .as_array()
        .is_some_and(|a| !a.is_empty()));
    assert!(!preview["sources"]["receivables"]["opening_batch"]["rows"]
        .as_array()
        .unwrap()
        .is_empty());
    let schedules = preview["sources"]["reports"]["source_period"]["rows"]
        .as_array()
        .ok_or("retained source-period schedules")?;
    assert_eq!(schedules.len(), 4);
    for kind in ["comparative", "movement", "sst", "payroll"] {
        let row = schedules
            .iter()
            .find(|row| row["kind"] == kind)
            .ok_or("retained schedule kind")?;
        assert_eq!(row["source_reference"], format!("HISTORY-{kind}"));
        assert_eq!(row["recorded_by_user_id"], s.user());
        assert!(!row["facts"]
            .as_array()
            .ok_or("source schedule facts")?
            .is_empty());
    }
    let opening_bridges = preview["sources"]["bank"]["opening_bridge"]["rows"]
        .as_array()
        .ok_or("retained opening bank bridges")?;
    assert_eq!(opening_bridges.len(), 2);
    assert_eq!(opening_bridges[0]["source_reference"], "BANK-OPENING-V1");
    assert_eq!(opening_bridges[1]["source_reference"], "BANK-OPENING-V2");
    assert_eq!(
        opening_bridges[1]["statement_source"],
        "Source bank statement at conversion"
    );
    assert_eq!(opening_bridges[1]["statement_balance"], "9300.0000");
    assert_eq!(opening_bridges[1]["book_balance"], "10000.0000");
    assert_eq!(opening_bridges[1]["replaces_bridge_id"], first_bridge);
    assert_eq!(
        opening_bridges[1]["reason"],
        "Corrected outstanding deposit amount from source statement"
    );
    assert_eq!(
        opening_bridges[1]["review"]["source_reference"],
        "BANK-OPENING-V2"
    );
    assert_eq!(
        opening_bridges[1]["source_csv"],
        "source_id,item_date,description,amount\ndeposit-in-transit,2026-06-30,Corrected deposit in transit,600\nlate-clearing,2026-06-30,Later clearing item,100\n"
    );
    let opening_items = preview["sources"]["bank"]["opening_item"]["rows"]
        .as_array()
        .ok_or("retained opening bank items")?;
    assert_eq!(opening_items.len(), 4);
    assert_eq!(opening_items[2]["source_id"], "deposit-in-transit");
    assert_eq!(opening_items[2]["amount"], "600.0000");
    assert_eq!(opening_items[3]["source_id"], "late-clearing");
    assert_eq!(opening_items[3]["amount"], "100.0000");
    assert_eq!(
        preview["sources"]["bank"]["match_opening_item"]["rows"][0]["opening_item_id"],
        opening_item
    );
    let bank_account_changes = preview["sources"]["bank"]["bank_account_change"]["rows"]
        .as_array()
        .ok_or("retained bank account changes")?;
    assert!(!bank_account_changes.is_empty());
    assert!(bank_account_changes
        .iter()
        .all(|row| { row["bank_account_id"] == bank.id && row["changed_by_user_id"] == s.user() }));
    for (area, dataset, actor) in [
        ("receivables", "setup_change", "changed_by_user_id"),
        ("payables", "settings_change", "changed_by_user_id"),
        ("fixed_assets", "category_revision", "recorded_by_user_id"),
    ] {
        let rows = preview["sources"][area][dataset]["rows"]
            .as_array()
            .ok_or("retained source configuration")?;
        assert!(!rows.is_empty(), "missing {area} {dataset} provenance");
        assert!(rows.iter().all(|row| row[actor] == s.user()));
    }
    assert_eq!(
        preview["bank_bridges"][0]["source_activity_after_finalisation"],
        false
    );
    let op = [issue()];
    let review = plans.preview(&authority, &op).await?;
    assert_eq!(review["valid"], true, "{review}");
    let done = plans.commit(&s, &authority, &op).await?;
    let id = done["results"][0]["result"]["result"]["id"]
        .as_i64()
        .ok_or_else(|| format!("missing issued result: {done}"))?;
    let kept = reports.get_issued_report(&s, COMPANY, id).await?;
    assert_eq!(kept.kind, "conversion");
    assert_eq!(
        kept.content["source_trial_balance"]["rows"],
        preview["source_trial_balance"]["rows"]
    );
    assert!(kept.content.get("accounts").is_none());
    let second_review = plans.preview(&authority, &op).await?;
    assert_eq!(second_review["valid"], true, "{second_review}");
    let second = plans.commit(&s, &authority, &op).await?;
    let second_id = second["results"][0]["result"]["result"]["id"]
        .as_i64()
        .ok_or("second issued conversion report")?;
    grants
        .set_direct(&s, 2, bilibi_finance_reports::ISSUE, Some(false))
        .await?;
    assert_eq!(
        registry
            .read(
                db,
                &viewer,
                "finance.reports.conversion.preview",
                &json!({"company_id":COMPANY,"as_of":"2026-08-01"}),
            )
            .await?["result"]["version"],
        3
    );
    assert_eq!(
        reports
            .get_issued_report(&viewer, COMPANY, id)
            .await?
            .content,
        kept.content
    );
    let mut after = None;
    let mut issued_ids = Vec::new();
    loop {
        let page = reports
            .evidence_page(&viewer, COMPANY, "issued", Some(500), after)
            .await?;
        assert!(page.rows.len() <= 1, "issued bodies were not page-bounded");
        issued_ids.extend(page.rows.iter().filter_map(|row| row["id"].as_i64()));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert!(issued_ids.contains(&id));
    assert!(issued_ids.contains(&second_id));
    grants
        .set_direct(&s, 2, bilibi_finance_payroll::VIEW, Some(false))
        .await?;
    assert!(reports
        .get_issued_report(&viewer, COMPANY, id)
        .await
        .is_err());
    assert!(reports
        .evidence_page(&viewer, COMPANY, "issued", Some(500), None)
        .await
        .is_err());
    grants
        .set_direct(&s, 2, bilibi_finance_payroll::VIEW, Some(true))
        .await?;
    let c = db.get().await?;
    for sql in [
        "UPDATE finance_reports_issued SET content='{}' WHERE id=$1",
        "DELETE FROM finance_reports_issued WHERE id=$1",
    ] {
        assert!(c.execute(sql, &[&id]).await.is_err());
    }
    println!("PASS conversion report: shared contract issuance, exact source/target facts, source-grant revocation and immutable issued copy");
    Ok(Kept {
        id,
        bank_id: bank.id,
        opening_item_id: later_opening_item,
        content: kept.content,
    })
}
pub async fn after(db: &Db, services: &Services, kept: Kept) -> Outcome<()> {
    let s = crate::in_company(db, services, 4, COMPANY).await?;
    let reports = services.get::<Reports>(bilibi_finance_reports::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let reconciliation = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let issuer = Authority {
        authentication: authentication.clone(),
        user: 4,
    };
    let plans = services.get::<Plans>("base/agent_api.plans")?;
    let before = reports.get_issued_report(&s, COMPANY, kept.id).await?;
    assert_eq!(
        before.content, kept.content,
        "register source corrections changed issued content"
    );
    let units = ledger.get_book(&s, COMPANY).await?.minor_units;
    let chart = ledger.list_accounts(&s, COMPANY).await?;
    let account = |code: &str| chart.iter().find(|a| a.code == code).unwrap().id;
    ledger
        .post_entry(
            &s,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 7, 1),
                description: "Unrelated backdated control posting after conversion issuance".into(),
                reference: None,
                lines: vec![
                    NewLine {
                        account_id: account("1100"),
                        debit: Money::parse("1", units)?,
                        credit: Money::ZERO,
                        description: None,
                    },
                    NewLine {
                        account_id: account("3000"),
                        debit: Money::ZERO,
                        credit: Money::parse("1", units)?,
                        description: None,
                    },
                ],
            },
            &Default::default(),
        )
        .await?;
    assert_eq!(
        reports
            .get_issued_report(&s, COMPANY, kept.id)
            .await?
            .content,
        kept.content
    );
    let deposit = ledger
        .post_entry(
            &s,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 7, 2),
                description: "Late bank deposit".into(),
                reference: Some("LATE-DEPOSIT".into()),
                lines: vec![
                    NewLine {
                        account_id: account("1000"),
                        debit: Money::parse("1", units)?,
                        credit: Money::ZERO,
                        description: None,
                    },
                    NewLine {
                        account_id: account("3000"),
                        debit: Money::ZERO,
                        credit: Money::parse("1", units)?,
                        description: None,
                    },
                ],
            },
            &Default::default(),
        )
        .await?;
    let withdrawal = ledger
        .post_entry(
            &s,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 7, 3),
                description: "Late bank withdrawal".into(),
                reference: Some("LATE-WITHDRAWAL".into()),
                lines: vec![
                    NewLine {
                        account_id: account("3000"),
                        debit: Money::parse("1", units)?,
                        credit: Money::ZERO,
                        description: None,
                    },
                    NewLine {
                        account_id: account("1000"),
                        debit: Money::ZERO,
                        credit: Money::parse("1", units)?,
                        description: None,
                    },
                ],
            },
            &Default::default(),
        )
        .await?;
    let entry_only = reports
        .conversion_report(&s, COMPANY, date(2026, 8, 1), &Default::default())
        .await?;
    let entry_only_bridge = entry_only["bank_bridges"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["account"]["id"] == kept.bank_id))
        .ok_or("conversion bank bridge after entries")?;
    assert!(Money::parse(
        entry_only_bridge["change_since_bridge"]
            .as_str()
            .ok_or("bank bridge entry-only change")?,
        units,
    )?
    .is_zero());
    assert_eq!(
        entry_only_bridge["source_activity_after_finalisation"],
        true
    );
    assert!(
        entry_only_bridge["source_boundary"]["through_entry_number"]
            .as_i64()
            .unwrap()
            > entry_only_bridge["finalised_boundary"]["through_entry_number"]
                .as_i64()
                .unwrap()
    );
    for axis in ["through_statement_number", "through_match_event_number"] {
        assert_eq!(
            entry_only_bridge["source_boundary"][axis],
            entry_only_bridge["finalised_boundary"][axis]
        );
    }
    let importer = crate::in_company(db, services, 2, COMPANY).await?;
    let imported = reconciliation
        .import_statement(
            &importer,
            COMPANY,
            kept.bank_id,
            &NewFile {
                file_name: "conversion-late-statement.csv".into(),
                content: b"Date,Details,Ref,Amount,Balance\n2026-07-02,Late bank deposit,LATE-DEPOSIT,1,10001\n2026-07-03,Late bank withdrawal,LATE-WITHDRAWAL,-1,10000\n".to_vec(),
                content_type: Some("text/csv".into()),
            },
        )
        .await?;
    for (line, entry) in imported
        .statement
        .lines
        .iter()
        .zip([deposit.id, withdrawal.id])
    {
        reconciliation
            .record_match(
                &s,
                COMPANY,
                kept.bank_id,
                &NewMatch {
                    statement_line_ids: vec![line.id],
                    entry_ids: vec![entry],
                    note: Some("Late source activity after bridge finalisation".into()),
                },
            )
            .await?;
    }
    let before_opening_match = reports
        .conversion_report(&s, COMPANY, date(2026, 8, 1), &Default::default())
        .await?;
    let before_opening_match = before_opening_match["bank_bridges"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["account"]["id"] == kept.bank_id))
        .ok_or("conversion bank bridge before later opening match")?["source_boundary"]
        .clone();
    let later_statement = reconciliation
        .import_statement(
            &importer,
            COMPANY,
            kept.bank_id,
            &NewFile {
                file_name: "conversion-later-opening-clearance.csv".into(),
                content: b"Date,Details,Ref,Amount,Balance\n2026-08-02,Later opening clearance,OPENING-LATER,100,10100\n".to_vec(),
                content_type: Some("text/csv".into()),
            },
        )
        .await?;
    let opening_match = [PlannedOperation {
        key: "finance.reconciliation.opening.match".into(),
        input: json!({
            "company_id": COMPANY,
            "bank_account_id": kept.bank_id,
            "opening_item_id": kept.opening_item_id,
            "statement_line_id": later_statement.statement.lines[0].id,
        }),
    }];
    let review = plans.preview(&issuer, &opening_match).await?;
    assert_eq!(review["valid"], true, "{review}");
    plans.commit(&s, &issuer, &opening_match).await?;
    let current = reports
        .conversion_report(&s, COMPANY, date(2026, 8, 1), &Default::default())
        .await?;
    assert_ne!(
        current["through_entry_number"],
        kept.content["through_entry_number"]
    );
    assert_ne!(
        current["receivables"]["control"],
        kept.content["receivables"]["control"]
    );
    assert!(
        current["sources"]["receivables"]["opening_item"]["rows"]
            .as_array()
            .unwrap()
            .len()
            > 500,
        "evidence paging lost register rows"
    );
    let bridge = current["bank_bridges"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["account"]["id"] == kept.bank_id))
        .ok_or("conversion bank bridge")?;
    assert!(Money::parse(
        bridge["change_since_bridge"]
            .as_str()
            .ok_or("bank bridge change")?,
        units,
    )?
    .is_zero());
    assert_eq!(bridge["source_activity_after_finalisation"], true);
    assert_eq!(
        bridge["source_boundary"]["through_statement_number"],
        before_opening_match["through_statement_number"],
        "a statement containing only a later-dated line crossed the report boundary"
    );
    assert!(
        bridge["source_boundary"]["through_match_event_number"]
            .as_i64()
            .unwrap()
            > before_opening_match["through_match_event_number"]
                .as_i64()
                .unwrap(),
        "an opening-item match did not advance the report-date activity boundary"
    );
    assert!(
        bridge["source_boundary"]["through_statement_number"]
            .as_i64()
            .unwrap()
            > bridge["finalised_boundary"]["through_statement_number"]
                .as_i64()
                .unwrap()
    );
    assert!(
        bridge["source_boundary"]["through_match_event_number"]
            .as_i64()
            .unwrap()
            > bridge["finalised_boundary"]["through_match_event_number"]
                .as_i64()
                .unwrap()
    );
    assert!(current["unresolved_differences"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| {
            row["kind"] == "bank_bridge_stale" && row["bank_account_id"] == kept.bank_id
        })));
    for dataset in [
        "statement",
        "statement_line",
        "match",
        "match_entry",
        "match_event",
        "match_line",
        "match_opening_item",
    ] {
        assert!(
            current["sources"]["bank"][dataset]["rows"]
                .as_array()
                .unwrap()
                .len()
                > kept.content["sources"]["bank"][dataset]["rows"]
                    .as_array()
                    .unwrap()
                    .len()
        );
    }
    let sign = [PlannedOperation {
        key: "finance.reports.conversion.sign".into(),
        input: json!({"company_id":COMPANY,"report_id":kept.id,"reviewer_reference":"Independent accountant review C-109/S","note":"Qualified: the retained report's differences and excluded history remain; external tax/payroll schedules require separate review."}),
    }];
    assert_eq!(
        plans.preview(&issuer, &sign).await?["valid"],
        false,
        "issuer signed own work"
    );
    let signer = Authority {
        authentication: authentication.clone(),
        user: 2,
    };
    assert_eq!(
        plans.preview(&signer, &sign).await?["valid"],
        false,
        "statement importer signed retained source work"
    );
    let grants = services.get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    for cap in VIEWS {
        grants.set_direct(&s, 3, cap, Some(true)).await?;
    }
    for unrelated in [
        bilibi_finance_attachments::VIEW,
        bilibi_finance_einvoice::VIEW,
    ] {
        grants.set_direct(&s, 3, unrelated, Some(false)).await?;
    }
    let signer = Authority {
        authentication,
        user: 3,
    };
    let scope = crate::in_company(db, services, 3, COMPANY).await?;
    let preview = plans.preview(&signer, &sign).await?;
    assert_eq!(preview["valid"], true, "{preview}");
    let attestation = preview["results"][0]["preview"]["result"]["attestation"]
        .as_str()
        .ok_or("missing reviewed sign-off attestation")?
        .to_owned();
    let done = plans.commit(&scope, &signer, &sign).await?;
    let sign_id = done["results"][0]["result"]["result"]["id"]
        .as_i64()
        .ok_or("missing signoff")?;
    assert_eq!(
        done["results"][0]["result"]["result"]["attestation"],
        attestation
    );
    assert_eq!(
        plans.preview(&signer, &sign).await?["valid"],
        false,
        "second signoff accepted"
    );
    let evidence = reports
        .evidence_page(&scope, COMPANY, "conversion_signoff", Some(1), None)
        .await?;
    assert_eq!(evidence.rows[0]["report_id"], kept.id);
    assert_eq!(evidence.rows[0]["attestation"], attestation);
    let registry = services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let read = registry
        .read(
            db,
            &scope,
            "finance.reports.conversion.get",
            &json!({"company_id":COMPANY,"report_id":kept.id}),
        )
        .await?;
    assert_eq!(read["result"]["report"]["content"], kept.content);
    assert_eq!(read["result"]["signoffs"][0]["signed_by_user_id"], 3);
    assert_eq!(read["result"]["signoffs"][0]["attestation"], attestation);
    let list = registry
        .read(
            db,
            &scope,
            "finance.reports.conversion.list",
            &json!({"company_id":COMPANY}),
        )
        .await?;
    assert_eq!(list["result"]["rows"][0]["signed"], true);
    let c = db.get().await?;
    assert!(c
        .execute(
            "DELETE FROM finance_reports_conversion_signoffs WHERE id=$1",
            &[&sign_id]
        )
        .await
        .is_err());
    assert!(c
        .batch_execute("TRUNCATE finance_reports_conversion_signoffs")
        .await
        .is_err());
    println!("PASS conversion report: original snapshot survives 10,000-row corrections and a backdated control posting; later reports retain differences and full evidence; stale review refused; independent qualified sign-off audited and immutable");
    Ok(())
}
