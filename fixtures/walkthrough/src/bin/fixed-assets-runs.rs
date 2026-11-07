//! NX2 running proof, mirroring Bilimbi.Finance.FixedAssets runs and corrections.
//! Run only against a fresh disposable database; business writes use facades.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Db};
use bilibi_finance_fixed_assets::{
    AcquisitionInput, Assumptions, DisposalInput, EventInput, FixedAssets, FixedAssetsError as F,
    Method, RegisterReport, ReportPins, RevisionInput,
};
use bilibi_finance_ledger::{
    AccountType, Ledger, MinorUnits, Money, NewAccount, NewEntry, NewLine, PostOptions, Reversal,
    TotalsQuery, YearClose,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_tenancy::Scope;
use jiff::civil::{date, Date};
use std::fmt::Debug;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
inventory::submit! { Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.fixture.finance.fixed-assets-runs",Provenance {module:"finance/fixed_assets",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],
    roles:&[
        bilibi_authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_fixed_assets::CAPABILITIES},
        bilibi_authz::RoleDefinition {code:"staff",name:"staff",description:"Walkthrough fixture role",grant_all:false,capabilities:&[bilibi_finance_fixed_assets::VIEW]},
    ],
}) }
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
fn m(v: &str) -> Money {
    Money::parse(v, MinorUnits::new(2).unwrap()).unwrap()
}
fn event(d: Date) -> EventInput {
    EventInput {
        effective_date: d,
        reason: "NX2 independently checked correction".into(),
    }
}
fn refused<T: Debug>(r: Result<T, F>, expected: impl FnOnce(&F) -> bool) {
    match r {
        Err(e) if expected(&e) => println!("Expected refusal: {e}"),
        other => panic!("Unexpected result: {other:?}"),
    }
}
fn tied(r: &RegisterReport) {
    assert!(!r.reconciliation.is_empty());
    for a in &r.reconciliation {
        assert_eq!(
            a.difference,
            Money::ZERO,
            "account {} {:?}",
            a.account_id,
            a.kind
        );
    }
}
fn snapshot(r: &RegisterReport) -> String {
    format!("{:?}|{:?}|{:?}", r.pins(), r.rows, r.reconciliation)
}
async fn report(f: &FixedAssets, s: &Scope, d: Date) -> Outcome<RegisterReport> {
    Ok(f.register_report(s, COMPANY, d, &ReportPins::default())
        .await?)
}
async fn acquire(
    f: &FixedAssets,
    l: &Ledger,
    s: &Scope,
    revision: i64,
    source_accounts: (i64, i64),
    amount: &str,
    service: Date,
) -> Outcome<bilibi_finance_fixed_assets::Asset> {
    let e = l
        .post_entry(
            s,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 1, 17),
                description: "NX2 acquisition".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(source_accounts.0, m(amount)),
                    NewLine::credit(source_accounts.1, m(amount)),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    Ok(f.acquire_from_entry(
        s,
        COMPANY,
        e.id,
        1,
        &AcquisitionInput {
            category_revision_id: revision,
            description: format!("NX2 {amount}"),
            in_service_date: service,
            residual_value: None,
            method: None,
            life_months: None,
            annual_rate: None,
        },
    )
    .await?)
}
async fn reverse_source(l: &Ledger, s: &Scope, id: i64, d: Date) -> Outcome<()> {
    l.reverse_entry(
        s,
        COMPANY,
        id,
        &Reversal {
            entry_date: d,
            description: "NX2 acquisition correction".into(),
            reference: None,
        },
    )
    .await?;
    Ok(())
}
async fn sql_refuses(c: &tokio_postgres::Client, sql: &str, message: &str) -> Outcome<()> {
    c.batch_execute("BEGIN").await?;
    let result = c.batch_execute(sql).await;
    c.batch_execute("ROLLBACK").await?;
    assert!(
        result
            .as_ref()
            .err()
            .and_then(|e| e.as_db_error())
            .is_some_and(|e| e.message().contains(message)),
        "{sql}: {result:?}"
    );
    Ok(())
}
#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_module_registry::verify_composition(
        &modules,
        bilibi_domains::MODULES,
        bilibi_extensions::MODULES,
    )?;
    bilibi_database::migrate(&db, &modules).await?;
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    install(&db, &services).await?;
    run(&db, &services).await?;
    println!("NX2: runs, corrections, reports, evidence, audit and database fences passed");
    Ok(())
}
async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let f = services.get::<FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    let l = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let s = sign_in(db, services, 1, 1).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let foreign = sign_in(db, services, 2, 5).await?;
    let chart = l.list_accounts(&s, COMPANY).await?;
    let capital = chart.iter().find(|a| a.code == "3000").unwrap().id;
    let mut accounts = Vec::new();
    for (code, kind) in [
        ("NX-COST", AccountType::Asset),
        ("NX-ACC", AccountType::Asset),
        ("NX-EXP", AccountType::Expense),
        ("NX-GAIN", AccountType::Income),
        ("NX-LOSS", AccountType::Expense),
        ("NX-CASH", AccountType::Asset),
    ] {
        accounts.push(
            l.create_account(&s, COMPANY, &NewAccount::new(code, code, kind))
                .await?
                .id,
        );
    }
    let defaults = RevisionInput {
        cost_account_id: accounts[0],
        accumulated_account_id: accounts[1],
        expense_account_id: accounts[2],
        gain_account_id: accounts[3],
        loss_account_id: accounts[4],
        assumptions: Assumptions {
            method: Method::StraightLine,
            life_months: 12,
            annual_rate: None,
        },
        reason: "NX2 twelve equal months".into(),
    };
    let category = f
        .create_category(&s, COMPANY, "NX2 equipment", &defaults)
        .await?;
    let a = acquire(
        &f,
        &l,
        &s,
        category.id,
        (accounts[0], capital),
        "1200",
        date(2026, 1, 17),
    )
    .await?;
    let b = acquire(
        &f,
        &l,
        &s,
        category.id,
        (accounts[0], capital),
        "600",
        date(2026, 1, 17),
    )
    .await?;
    refused(f.depreciate(&staff, COMPANY, date(2026, 1, 1)).await, |e| {
        matches!(e, F::Unauthorized)
    });
    refused(f.depreciate(&s, COMPANY, date(2026, 1, 2)).await, |e| {
        matches!(e, F::InvalidPeriod)
    });
    // The owner grant alone cannot bypass Ledger's posting authority, and a
    // refused posting must not consume the month or leave business records.
    let grants = services.get::<Grants>("base/authz.grants")?;
    let pin = l
        .account_totals(&s, COMPANY, &TotalsQuery::default())
        .await?
        .through_entry_number;
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::POST_ENTRIES, Some(false))
        .await?;
    refused(f.depreciate(&s, COMPANY, date(2026, 1, 1)).await, |e| {
        matches!(
            e,
            F::Ledger(bilibi_finance_ledger::LedgerError::Unauthorized)
        )
    });
    assert!(f.list_runs(&s, COMPANY).await?.is_empty());
    for dataset in ["run", "run_item", "run_exception", "event"] {
        assert!(f
            .evidence_page(&s, COMPANY, dataset, None, None)
            .await?
            .rows
            .is_empty());
    }
    assert_eq!(
        l.account_totals(&s, COMPANY, &TotalsQuery::default())
            .await?
            .through_entry_number,
        pin
    );
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::POST_ENTRIES, None)
        .await?;
    // Race creation, rather than only racing retries after a run already exists.
    let (first, second) = tokio::join!(
        f.depreciate(&s, COMPANY, date(2026, 1, 1)),
        f.depreciate(&s, COMPANY, date(2026, 1, 1))
    );
    let jan = first?;
    assert_eq!(second?.id, jan.id);
    assert_eq!(f.list_runs(&s, COMPANY).await?.len(), 1);
    assert_eq!(
        l.account_totals(&s, COMPANY, &TotalsQuery::default())
            .await?
            .through_entry_number,
        pin + 1
    );
    assert_eq!(jan.total, m("150"));
    assert_eq!(jan.items.len(), 2);
    assert_eq!(
        jan.items
            .iter()
            .find(|i| i.asset_id == a.id)
            .unwrap()
            .amount,
        m("100")
    );
    assert_eq!(
        jan.items
            .iter()
            .find(|i| i.asset_id == b.id)
            .unwrap()
            .amount,
        m("50")
    );
    let (r1, r2) = tokio::join!(
        f.depreciate(&s, COMPANY, date(2026, 1, 1)),
        f.depreciate(&s, COMPANY, date(2026, 1, 1))
    );
    assert_eq!(r1?.id, jan.id);
    assert_eq!(r2?.id, jan.id);
    let historical = report(&f, &s, date(2026, 1, 31)).await?;
    tied(&historical);
    reverse_source(&l, &s, a.entry_id, date(2026, 1, 31)).await?;
    assert!(report(&f, &s, date(2026, 1, 31))
        .await?
        .reconciliation
        .iter()
        .any(|r| r.difference != Money::ZERO));
    let void = f
        .void_acquisition(&s, COMPANY, a.id, &event(date(2026, 1, 31)))
        .await?;
    assert_eq!(void.accumulated, Some(m("100")));
    assert!(void.entry_id.is_some());
    tied(&report(&f, &s, date(2026, 1, 31)).await?);
    let partial = f
        .reverse_run(&s, COMPANY, jan.id, &event(date(2026, 1, 31)))
        .await?;
    let pe = l.get_entry(&s, COMPANY, partial.entry_id.unwrap()).await?;
    assert_eq!(pe.lines.iter().map(|x| x.debit).sum::<Money>(), m("50"));
    assert!(l
        .get_entry(&s, COMPANY, jan.entry_id.unwrap())
        .await?
        .reversed_by_entry_id
        .is_none());
    tied(&report(&f, &s, date(2026, 1, 31)).await?);
    let feb = f.depreciate(&s, COMPANY, date(2026, 2, 1)).await?;
    assert_eq!(feb.total, m("100"));
    tied(&report(&f, &s, date(2026, 2, 28)).await?);
    // One surviving asset exercises profit, loss, break-even and nil proceeds;
    // every disposal is reversed before trying the next, restoring the register.
    for (proceeds, gain) in [("550", "50"), ("450", "-50"), ("500", "0"), ("0", "-500")] {
        let disposed = f
            .dispose(
                &s,
                COMPANY,
                b.id,
                &DisposalInput {
                    event: event(date(2026, 2, 28)),
                    proceeds: m(proceeds),
                    settlement_account_id: Some(accounts[5]),
                },
            )
            .await?;
        assert_eq!(disposed.accumulated, Some(m("100")));
        let r = report(&f, &s, date(2026, 2, 28)).await?;
        tied(&r);
        let row = r.rows.iter().find(|r| r.asset.id == b.id).unwrap();
        assert!(row.disposed);
        assert_eq!(row.disposal_gain, m(gain));
        assert_eq!(row.carrying_value, Money::ZERO);
        refused(
            f.reverse_run(&s, COMPANY, feb.id, &event(date(2026, 2, 28)))
                .await,
            |e| matches!(e, F::DependentPostings),
        );
        let reversed = f
            .reverse_disposal(&s, COMPANY, b.id, &event(date(2026, 2, 28)))
            .await?;
        assert_eq!(reversed.reverses_event_id, Some(disposed.id));
        assert_eq!(
            l.get_entry(&s, COMPANY, disposed.entry_id.unwrap())
                .await?
                .reversed_by_entry_id,
            reversed.entry_id
        );
        tied(&report(&f, &s, date(2026, 2, 28)).await?);
    }
    // A bad source is an exception, not a refusal of the live asset's charge.
    let bad_source = acquire(
        &f,
        &l,
        &s,
        category.id,
        (accounts[0], capital),
        "300",
        date(2026, 1, 17),
    )
    .await?;
    reverse_source(&l, &s, bad_source.entry_id, date(2026, 3, 31)).await?;
    let march = f.depreciate(&s, COMPANY, date(2026, 3, 1)).await?;
    assert_eq!(march.total, m("50"));
    assert_eq!(march.items.len(), 1);
    assert_eq!(march.exceptions.len(), 1);
    assert_eq!(march.exceptions[0].asset_id, bad_source.id);
    assert_eq!(march.exceptions[0].reason, "acquisition_reversed");
    f.void_acquisition(&s, COMPANY, bad_source.id, &event(date(2026, 3, 31)))
        .await?;
    refused(
        f.reverse_run(&s, COMPANY, feb.id, &event(date(2026, 3, 31)))
            .await,
        |e| matches!(e, F::DependentPostings),
    );
    // A standing disposal of an entirely different asset still blocks a run
    // correction company-wide, even though that asset has no run items.
    let unrelated = acquire(
        &f,
        &l,
        &s,
        category.id,
        (accounts[0], capital),
        "120",
        date(2027, 1, 1),
    )
    .await?;
    assert!(!march.items.iter().any(|i| i.asset_id == unrelated.id));
    f.dispose(
        &s,
        COMPANY,
        unrelated.id,
        &DisposalInput {
            event: event(date(2026, 3, 31)),
            proceeds: m("120"),
            settlement_account_id: Some(accounts[5]),
        },
    )
    .await?;
    tied(&report(&f, &s, date(2026, 3, 31)).await?);
    refused(
        f.reverse_run(&s, COMPANY, march.id, &event(date(2026, 3, 31)))
            .await,
        |e| matches!(e, F::DependentPostings),
    );
    f.reverse_disposal(&s, COMPANY, unrelated.id, &event(date(2026, 3, 31)))
        .await?;
    reverse_source(&l, &s, unrelated.entry_id, date(2026, 3, 31)).await?;
    f.void_acquisition(&s, COMPANY, unrelated.id, &event(date(2026, 3, 31)))
        .await?;
    let whole = f
        .reverse_run(&s, COMPANY, march.id, &event(date(2026, 3, 31)))
        .await?;
    assert_eq!(
        l.get_entry(&s, COMPANY, march.entry_id.unwrap())
            .await?
            .reversed_by_entry_id,
        whole.entry_id
    );
    assert_eq!(
        f.depreciate(&s, COMPANY, date(2026, 3, 1)).await?.id,
        march.id
    );
    tied(&report(&f, &s, date(2026, 3, 31)).await?);
    // Backdated registration and new revision must not change any historical pin.
    let late = acquire(
        &f,
        &l,
        &s,
        category.id,
        (accounts[0], capital),
        "120",
        date(2027, 1, 1),
    )
    .await?;
    let mut revised = defaults.clone();
    revised.assumptions.life_months = 6;
    revised.reason = "NX2 later revision".into();
    f.revise_category(&s, COMPANY, category.category_id, &revised)
        .await?;
    reverse_source(&l, &s, late.entry_id, date(2026, 3, 31)).await?;
    let before_entryless = report(&f, &s, date(2026, 3, 31)).await?;
    let entryless = f
        .void_acquisition(&s, COMPANY, late.id, &event(date(2026, 3, 31)))
        .await?;
    assert!(entryless.entry_id.is_none());
    assert_eq!(entryless.accumulated, Some(Money::ZERO));
    let reproduced = f
        .register_report(&s, COMPANY, date(2026, 3, 31), &before_entryless.pins())
        .await?;
    assert_eq!(snapshot(&before_entryless), snapshot(&reproduced));
    let reproduced = f
        .depreciation_schedule_report(&s, COMPANY, date(2026, 1, 31), &historical.pins())
        .await?;
    assert_eq!(snapshot(&historical), snapshot(&reproduced));
    // A gain or depreciation account the company also uses elsewhere (sales,
    // a manual journal) ties out: only this module's postings are compared,
    // and the rest is shown beside the row instead of as a difference.
    l.post_entry(
        &s,
        COMPANY,
        &NewEntry {
            entry_date: date(2026, 3, 31),
            description: "NX2 unrelated postings on shared accounts".into(),
            reference: None,
            lines: vec![
                NewLine::debit(accounts[5], m("75")),
                NewLine::credit(accounts[3], m("75")),
                NewLine::debit(accounts[2], m("20")),
                NewLine::credit(accounts[5], m("20")),
            ],
        },
        &PostOptions::default(),
    )
    .await?;
    let shared = report(&f, &s, date(2026, 12, 31)).await?;
    tied(&shared);
    let other = |id| {
        shared
            .reconciliation
            .iter()
            .find(|r| r.account_id == id)
            .unwrap()
            .other_postings
    };
    assert_eq!(other(accounts[3]), Money::ZERO - m("75"));
    assert_eq!(other(accounts[2]), m("20"));
    assert_eq!(other(accounts[0]), Money::ZERO);
    println!("Shared gain and expense accounts tie out; their other postings are shown apart");
    // Closing entries zero expense in the ledger, but register reconciliation
    // reads activity without either a closing entry or its later reversal.
    let before_close = report(&f, &s, date(2026, 12, 31)).await?;
    tied(&before_close);
    let year = l.list_fiscal_years(&s, COMPANY).await?.remove(0);
    let closed = l
        .close_fiscal_year(&s, COMPANY, year.id, &YearClose::default())
        .await?;
    assert!(closed.closing_entry_id.is_some());
    let closed_report = report(&f, &s, date(2026, 12, 31)).await?;
    tied(&closed_report);
    assert_eq!(before_close.reconciliation, closed_report.reconciliation);
    assert_eq!(
        snapshot(&before_close),
        snapshot(
            &f.register_report(&s, COMPANY, date(2026, 12, 31), &before_close.pins())
                .await?
        )
    );
    l.reopen_fiscal_year(&s, COMPANY, year.id, "NX2 closing correction")
        .await?;
    assert_eq!(
        closed_report.reconciliation,
        report(&f, &s, date(2026, 12, 31)).await?.reconciliation
    );
    for p in year.periods.iter().rev().skip(1) {
        l.reopen_period(&s, COMPANY, p.id, "NX2 restore open periods")
            .await?;
    }
    // Reverse the remaining source: April consumes an exception-only month.
    reverse_source(&l, &s, b.entry_id, date(2026, 3, 31)).await?;
    let skipped = f.depreciate(&s, COMPANY, date(2026, 4, 1)).await?;
    assert_eq!(skipped.total, Money::ZERO);
    assert!(skipped.entry_id.is_none());
    assert_eq!(skipped.exceptions.len(), 1);
    f.void_acquisition(&s, COMPANY, b.id, &event(date(2026, 4, 30)))
        .await?;
    assert_eq!(
        f.depreciate(&s, COMPANY, date(2026, 4, 1)).await?.id,
        skipped.id
    );
    let empty = f.depreciate(&s, COMPANY, date(2026, 5, 1)).await?;
    assert!(empty.items.is_empty() && empty.exceptions.is_empty() && empty.entry_id.is_none());
    assert_eq!(
        f.depreciate(&s, COMPANY, date(2026, 5, 1)).await?.id,
        empty.id
    );
    refused(
        f.reverse_run(&s, COMPANY, empty.id, &event(date(2026, 5, 31)))
            .await,
        |e| matches!(e, F::EmptyRun),
    );
    tied(&report(&f, &s, date(2026, 5, 31)).await?);
    let zero = acquire(
        &f,
        &l,
        &s,
        category.id,
        (accounts[0], capital),
        "12",
        date(2027, 1, 1),
    )
    .await?;
    reverse_source(&l, &s, zero.entry_id, date(2026, 5, 31)).await?;
    let multi = acquire(
        &f,
        &l,
        &s,
        category.id,
        (accounts[0], capital),
        "120",
        date(2026, 6, 17),
    )
    .await?;
    // Close in sequence through May: even an entryless void needs an open month.
    let years = l.list_fiscal_years(&s, COMPANY).await?;
    for p in years
        .iter()
        .flat_map(|y| &y.periods)
        .filter(|p| p.start_date <= date(2026, 5, 1))
    {
        l.close_period(&s, COMPANY, p.id, Some("NX2 closed period proof"))
            .await?;
    }
    let consumed = f.depreciate(&s, COMPANY, date(2026, 1, 1)).await?;
    assert_eq!(consumed.id, jan.id);
    assert!(consumed.reversed);
    refused(
        f.void_acquisition(&s, COMPANY, zero.id, &event(date(2026, 5, 31)))
            .await,
        |e| matches!(e, F::PeriodClosed),
    );
    f.void_acquisition(&s, COMPANY, zero.id, &event(date(2026, 6, 1)))
        .await?;
    for month in [date(2026, 6, 1), date(2026, 7, 1)] {
        assert_eq!(f.depreciate(&s, COMPANY, month).await?.total, m("10"));
    }
    reverse_source(&l, &s, multi.entry_id, date(2026, 7, 31)).await?;
    let void = f
        .void_acquisition(&s, COMPANY, multi.id, &event(date(2026, 7, 31)))
        .await?;
    assert_eq!(void.accumulated, Some(m("20")));
    let mirror = l.get_entry(&s, COMPANY, void.entry_id.unwrap()).await?;
    assert_eq!(mirror.lines.len(), 4);
    assert_eq!(
        mirror.lines.iter().map(|line| line.debit).sum::<Money>(),
        m("20")
    );
    tied(&report(&f, &s, date(2026, 7, 31)).await?);
    refused(f.list_runs(&foreign, COMPANY).await, |e| {
        matches!(e, F::Unauthorized | F::CompanyNotFound)
    });
    refused(f.list_events(&admin, 2, b.id).await, |e| {
        matches!(e, F::NotFound)
    });
    services
        .get::<bilibi_company::CompanyService>(bilibi_company::SERVICE)?
        .archive_company(&admin, 3, Some("NX2 archival proof"))
        .await?;
    refused(f.depreciate(&admin, 3, date(2026, 1, 1)).await, |e| {
        matches!(e, F::CompanyArchived)
    });
    assert_eq!(f.evidence_datasets().len(), 11);
    for dataset in f.evidence_datasets() {
        let mut after = None;
        let mut last = 0;
        let mut count = 0;
        loop {
            let page = f
                .evidence_page(&s, COMPANY, dataset, Some(1), after)
                .await?;
            assert!(page.rows.len() <= 1);
            for row in &page.rows {
                let id = row["id"].as_i64().unwrap();
                assert!(id > last);
                last = id;
                assert_eq!(row["tenant_id"], 1);
                assert_eq!(row["company_id"], COMPANY);
                count += 1;
            }
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        assert!(
            count > 0 || dataset.starts_with("opening_"),
            "empty evidence {dataset}"
        );
        println!("{dataset}: {count} scoped ordered rows");
    }
    refused(
        f.evidence_page(&s, COMPANY, "run", Some(501), None).await,
        |e| matches!(e, F::InvalidOption),
    );
    let c = db.get().await?;
    for (table, kind) in [
        ("categories", "category"),
        ("category_revisions", "category_revision"),
        ("assets", "asset"),
        ("runs", "run"),
        ("run_items", "run_item"),
        ("run_exceptions", "run_exception"),
        ("events", "event"),
    ] {
        let missing:i64=c.query_one(&format!("SELECT count(*) FROM finance_fixed_assets_{table} r WHERE r.tenant_id=1 AND r.company_id=$1 AND (SELECT count(*) FROM base_audit_mutations a WHERE a.auditable_type=$2 AND a.auditable_id=r.id::text AND a.tenant_id=r.tenant_id AND a.new_values->>'company_id'=r.company_id::text)<>1"),&[&COMPANY,&format!("finance.fixed-assets.{kind}")]).await?.get(0);
        assert_eq!(missing, 0, "audit {table}");
        for sql in [
            format!(
                "UPDATE finance_fixed_assets_{table} SET recorded_by_user_id=1 WHERE tenant_id=1 AND company_id={COMPANY}"
            ),
            format!(
                "DELETE FROM finance_fixed_assets_{table} WHERE tenant_id=1 AND company_id={COMPANY}"
            ),
            format!("TRUNCATE finance_fixed_assets_{table} CASCADE"),
        ] {
            sql_refuses(&c, &sql, "fixed assets retain every record permanently").await?;
        }
    }
    for table in ["run_items", "run_exceptions"] {
        sql_refuses(&c,&format!("INSERT INTO finance_fixed_assets_{table} SELECT (jsonb_populate_record(NULL::finance_fixed_assets_{table},(SELECT to_jsonb(r)||jsonb_build_object('id',999999) FROM finance_fixed_assets_{table} r WHERE tenant_id=1 AND company_id={COMPANY} LIMIT 1))).*"),"fixed asset run already recorded").await?;
    }
    sql_refuses(&c,&format!("INSERT INTO finance_fixed_assets_runs SELECT (jsonb_populate_record(NULL::finance_fixed_assets_runs,(SELECT to_jsonb(r)||jsonb_build_object('id',999999,'period','2026-12-01','total',999) FROM finance_fixed_assets_runs r WHERE id={} AND tenant_id=1 AND company_id={COMPANY}))).*; SET CONSTRAINTS ALL IMMEDIATE",jan.id),"fixed asset run total differs from items").await?;
    // Forcing the parent check early must not permit a later child to change
    // the immutable run's sum without another check at commit.
    sql_refuses(&c,&format!("INSERT INTO finance_fixed_assets_runs SELECT (jsonb_populate_record(NULL::finance_fixed_assets_runs,(SELECT to_jsonb(r)||jsonb_build_object('id',999998,'period','2026-12-01') FROM finance_fixed_assets_runs r WHERE id={}))).*; SET CONSTRAINTS finance_fixed_assets_runs_total IMMEDIATE; INSERT INTO finance_fixed_assets_run_items SELECT (jsonb_populate_record(NULL::finance_fixed_assets_run_items,(SELECT to_jsonb(r)||jsonb_build_object('id',999998,'run_id',999998) FROM finance_fixed_assets_run_items r WHERE run_id={} LIMIT 1))).*; SET CONSTRAINTS ALL IMMEDIATE",empty.id,jan.id),"fixed asset run total differs from items").await?;
    Ok(())
}
