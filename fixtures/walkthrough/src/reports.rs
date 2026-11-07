//! The reports walkthrough: Company 001's management reports after every
//! other walkthrough has posted. It configures the company's report sections
//! (refused shapes first), reads profit and loss, the balance sheet, the cash
//! position and the overview with their comparative columns, follows a figure
//! to the entries behind it, reproduces a report from its pinned entry number
//! after a later post that a posting subscription hears, issues immutable
//! snapshots, and ends with the refusals of the facade and the database. Every
//! line is the facade's actual answer; nothing here is a test double.
use crate::{sign_in, COMPANY};
use bilibi_database::{Db, MutationBoundary};
use bilibi_finance_ledger::{
    Ledger, MinorUnits, Money, NewAccount, NewEntry, NewLine, PeriodStatus, PostOptions,
    SERVICE as LEDGER,
};
use bilibi_finance_reports::{
    previous_period, ActivityQuery, BalanceSheet, CashPosition, Group, Issue, Overview, Period,
    PostingEvent, ProfitAndLoss, Reports, ReportsError, Section, SECTIONS, SERVICE,
};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use jiff::civil::{date, Date};
use serde_json::{json, Value};
use std::{fmt::Debug, time::Duration};

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn show<T: Debug>(step: &str, result: Result<T, ReportsError>) {
    match result {
        Ok(value) => println!("{step}: {value:?}"),
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}

fn amounts(values: &[Money], units: MinorUnits) -> String {
    values
        .iter()
        .map(|amount| format!("{:>10}", amount.shown(units)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn sections(indent: &str, sections: &[Section], units: MinorUnits) {
    for section in sections {
        println!(
            "{indent}{}: {}",
            section.name.as_deref().unwrap_or("(no section)"),
            amounts(&section.totals, units)
        );
        for row in &section.rows {
            println!(
                "{indent}  {} {:<22} {}",
                row.code,
                row.name,
                amounts(&row.amounts, units)
            );
        }
    }
}

fn groups(groups: &[Group], units: MinorUnits) {
    for group in groups {
        println!(
            "  {}: {}",
            group.account_type.as_str(),
            amounts(&group.totals, units)
        );
        sections("    ", &group.sections, units);
    }
}

fn profit_and_loss(step: &str, report: &ProfitAndLoss) {
    let units = report.header.minor_units;
    println!(
        "{step}: through entry {}, columns {}, unmatched codes {:?}",
        report.header.through_entry_number,
        serde_json::to_string(&report.header.columns).unwrap_or_default(),
        report.unmatched_codes
    );
    groups(&report.groups, units);
    println!("  net income: {}", amounts(&report.net_income, units));
}

fn balance_sheet(step: &str, report: &BalanceSheet) {
    let units = report.header.minor_units;
    println!(
        "{step}: through entry {}, columns {}",
        report.header.through_entry_number,
        serde_json::to_string(&report.header.columns).unwrap_or_default()
    );
    groups(&report.groups, units);
    println!(
        "  earnings (in equity): {}",
        amounts(&report.earnings, units)
    );
    println!(
        "  total assets {} = total liabilities and equity {}: {}",
        amounts(&report.total_assets, units),
        amounts(&report.total_liabilities_and_equity, units),
        report.total_assets == report.total_liabilities_and_equity
    );
}

fn cash_position(step: &str, report: &CashPosition) {
    let units = report.header.minor_units;
    println!(
        "{step}: through entry {}, unmatched codes {:?}",
        report.header.through_entry_number, report.unmatched_codes
    );
    sections("  ", &report.sections, units);
    println!("  total: {}", amounts(&report.totals, units));
}

fn overview(step: &str, report: &Overview) {
    let units = report.header.minor_units;
    println!(
        "{step}: through entry {}, columns {}",
        report.header.through_entry_number,
        serde_json::to_string(&report.header.columns).unwrap_or_default()
    );
    for (label, values) in [
        ("income", &report.income),
        ("expense", &report.expense),
        ("net income", &report.net_income),
        ("assets", &report.assets),
        ("liabilities", &report.liabilities),
        ("equity", &report.equity),
        ("cash", &report.cash),
    ] {
        println!("  {label:<12} {}", amounts(values, units));
    }
}

/// Saves the company's sections as the Settings screen would.
async fn save_sections(
    db: &Db,
    services: &Services,
    scope: &Scope,
    value: Value,
) -> Outcome<String> {
    let mutations = services.get::<MutationBoundary>("base/database.mutations")?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let mut client = db.get().await?;
    let mut tx = mutations
        .begin(
            &mut client,
            scope.mutation_context("finance.reports.walkthrough.sections.save"),
        )
        .await?;
    let target = Target::Company(OrganisationId::new(COMPANY)?);
    match settings.put(&mut tx, scope, target, SECTIONS, value).await {
        Ok(()) => {
            tx.commit().await?;
            Ok("stored".into())
        }
        Err(error) => {
            tx.rollback().await?;
            Ok(format!("refused ({error:?})"))
        }
    }
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let reports = services.get::<Reports>(SERVICE)?;
    let ledger = services.get::<Ledger>(LEDGER)?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let other = sign_in(db, services, 2, 5).await?;
    let contractor = sign_in(db, services, 1, 7).await?;
    let book = ledger.get_book(&manager, COMPANY).await?;
    let money = |text: &str| Money::parse(text, book.minor_units).expect("fixture amount");

    println!("\n== Report sections (company 1)");
    // A group over the bank and cash accounts, so a listed group brings both.
    let mut accounts = ledger.list_accounts(&manager, COMPANY).await?;
    let id = |accounts: &[bilibi_finance_ledger::Account], code: &str| {
        accounts
            .iter()
            .find(|account| account.code == code)
            .map(|account| account.id)
            .ok_or("fixture account")
    };
    if id(&accounts, "1800").is_err() {
        ledger
            .create_account_group(
                &manager,
                COMPANY,
                &bilibi_finance_ledger::NewAccountGroup::new(
                    "1800",
                    "Cash and bank",
                    bilibi_finance_ledger::AccountType::Asset,
                ),
            )
            .await?;
        accounts = ledger.list_accounts(&manager, COMPANY).await?;
        let group = id(&accounts, "1800")?;
        ledger
            .update_account(
                &manager,
                COMPANY,
                id(&accounts, "1000")?,
                &bilibi_finance_ledger::AccountChange {
                    parent_id: Some(Some(group)),
                    ..Default::default()
                },
            )
            .await?;
        let mut on_hand = NewAccount::new(
            "1020",
            "Cash on hand",
            bilibi_finance_ledger::AccountType::Asset,
        );
        on_hand.parent_id = Some(group);
        ledger.create_account(&manager, COMPANY, &on_hand).await?;
        accounts = ledger.list_accounts(&manager, COMPANY).await?;
    }
    let (cash, on_hand, sales, costs) = (
        id(&accounts, "1000")?,
        id(&accounts, "1020")?,
        id(&accounts, "4000")?,
        id(&accounts, "5000")?,
    );
    for (step, value) in [
        (
            "an object, not a list",
            json!({"report": "balance_sheet", "name": "Cash", "accounts": ["1000"]}),
        ),
        (
            "an unknown report",
            json!([{"report": "cash_flow", "name": "Cash", "accounts": ["1000"]}]),
        ),
        (
            "a blank heading",
            json!([{"report": "balance_sheet", "name": "  ", "accounts": ["1000"]}]),
        ),
        (
            "a fourth key",
            json!([{"report": "balance_sheet", "name": "Cash", "accounts": ["1000"], "order": 1}]),
        ),
        (
            "a numeric account code",
            json!([{"report": "balance_sheet", "name": "Cash", "accounts": [1000]}]),
        ),
        (
            "a heading of 101 characters",
            json!([{"report": "balance_sheet", "name": "x".repeat(101), "accounts": []}]),
        ),
    ] {
        println!(
            "manager saves sections with {step}: {}",
            save_sections(db, services, &manager, value).await?
        );
    }
    let layout = json!([
        {"report": "profit_and_loss", "name": "Revenue", "accounts": ["4000"]},
        {"report": "profit_and_loss", "name": "Direct costs", "accounts": ["5000"]},
        {"report": "balance_sheet", "name": "Cash and receivables", "accounts": ["1800", "1100"]},
        {"report": "balance_sheet", "name": "Owed to others", "accounts": ["2000", "2100"]},
        {"report": "balance_sheet", "name": "Cash, listed twice", "accounts": ["1800"]},
        {"report": "cash_position", "name": "Bank and cash on hand", "accounts": [" 1800 ", "9999"]},
    ]);
    println!(
        "manager saves six sections: {}",
        save_sections(db, services, &manager, layout).await?
    );

    // Postings in the first open period, so the figures move this year.
    let year = ledger
        .list_fiscal_years(&manager, COMPANY)
        .await?
        .into_iter()
        .find(|year| year.start_date == date(2026, 1, 1))
        .ok_or("company 1's 2026 fiscal year")?;
    let open = year
        .periods
        .iter()
        .find(|period| period.status == PeriodStatus::Open)
        .ok_or("an open period")?
        .start_date;
    for (description, lines) in [
        (
            "Cash drawn to hand",
            vec![
                NewLine::debit(on_hand, money("80.00")),
                NewLine::credit(cash, money("80.00")),
            ],
        ),
        (
            "Goods bought for resale",
            vec![
                NewLine::debit(costs, money("120.00")),
                NewLine::credit(cash, money("120.00")),
            ],
        ),
    ] {
        let entry = ledger
            .post_entry(
                &manager,
                COMPANY,
                &NewEntry {
                    entry_date: open,
                    description: description.into(),
                    reference: None,
                    lines,
                },
                &PostOptions::default(),
            )
            .await?;
        println!(
            "manager posts entry {} dated {}: {description}",
            entry.entry_number, entry.entry_date
        );
    }

    println!("\n== Figures (company 1)");
    let year_2026 = Period {
        from: date(2026, 1, 1),
        to: date(2026, 12, 31),
    };
    let first_half = Period {
        from: date(2026, 1, 1),
        to: date(2026, 6, 30),
    };
    let pl = reports
        .profit_and_loss(&manager, COMPANY, &[year_2026, first_half], None)
        .await?;
    profit_and_loss(
        "manager reads profit and loss for 2026 and its first half",
        &pl,
    );
    let pinned = pl.header.through_entry_number;
    balance_sheet(
        "manager reads the balance sheet as of 2026-12-31 and 2025-12-31",
        &reports
            .balance_sheet(
                &manager,
                COMPANY,
                &[date(2026, 12, 31), date(2025, 12, 31)],
                None,
            )
            .await?,
    );
    cash_position(
        "manager reads the cash position as of 2026-12-31 and the day before the first open period",
        &reports
            .cash_position(
                &manager,
                COMPANY,
                &[date(2026, 12, 31), open.yesterday()?],
                None,
            )
            .await?,
    );
    let quarter = Period {
        from: date(2026, 4, 1),
        to: date(2026, 6, 30),
    };
    overview(
        "manager reads the overview of 2026-04-01 to 2026-06-30",
        &reports.overview(&manager, COMPANY, quarter, None).await?,
    );
    for period in [
        quarter,
        year_2026,
        Period {
            from: date(2026, 3, 1),
            to: date(2026, 3, 31),
        },
        Period {
            from: date(2026, 3, 10),
            to: date(2026, 3, 19),
        },
        Period {
            from: date(2026, 2, 15),
            to: date(2026, 3, 14),
        },
    ] {
        show(
            &format!("previous period of {} to {}", period.from, period.to),
            previous_period(period).map(|p| format!("{} to {}", p.from, p.to)),
        );
    }

    println!("\n== Behind a figure (company 1)");
    let activity = reports
        .account_activity(
            &manager,
            COMPANY,
            costs,
            &ActivityQuery {
                from: Some(year_2026.from),
                to: Some(year_2026.to),
                through_entry_number: Some(pinned),
                exclude_closing: true,
                limit: Some(2),
                after: None,
            },
        )
        .await?;
    println!(
        "manager opens 5000's 2026 figure at entry {}: debits {} credits {}, {} entries, more {}",
        activity.through_entry_number,
        activity.debits,
        activity.credits,
        activity.entries.len(),
        activity.more
    );
    let mut own = (Money::ZERO, Money::ZERO);
    let mut after = None;
    loop {
        let page = reports
            .account_activity(
                &manager,
                COMPANY,
                costs,
                &ActivityQuery {
                    from: Some(year_2026.from),
                    to: Some(year_2026.to),
                    through_entry_number: Some(pinned),
                    exclude_closing: true,
                    limit: Some(2),
                    after,
                },
            )
            .await?;
        for entry in &page.entries {
            println!(
                "  #{} {} {:?} posted by {} (impersonator {:?}): debit {} credit {}, {} lines",
                entry.entry.entry_number,
                entry.entry.entry_date,
                entry.entry.description,
                entry.entry.posted_by_user_id,
                entry.entry.posted_by_impersonator_id,
                entry.debit,
                entry.credit,
                entry.entry.lines.len()
            );
            own = (own.0 + entry.debit, own.1 + entry.credit);
        }
        match page.entries.last() {
            Some(last) if page.more => after = Some((last.entry.entry_date, last.entry.id)),
            _ => break,
        }
    }
    let figure = pl
        .groups
        .iter()
        .flat_map(|group| &group.sections)
        .flat_map(|section| &section.rows)
        .find(|row| row.account_id == costs)
        .map(|row| row.amounts[0]);
    println!(
        "its entries' own debits less credits equal the profit and loss figure: {} ({} vs {:?})",
        Some(own.0 - own.1) == figure,
        own.0 - own.1,
        figure
    );

    println!("\n== Pinned figures and posting notices (company 1)");
    let mut subscription = reports.subscribe(&manager, COMPANY).await?;
    let later = ledger
        .post_entry(
            &manager,
            COMPANY,
            &NewEntry {
                entry_date: open,
                description: "Cash sale after the report was read".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(cash, money("500.00")),
                    NewLine::credit(sales, money("500.00")),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    println!(
        "manager posts entry {} dated {}",
        later.entry_number, later.entry_date
    );
    match tokio::time::timeout(Duration::from_secs(5), subscription.next()).await {
        Ok(Some(PostingEvent::Posted {
            company_id,
            entry_number,
        })) => {
            println!(
                "the report's subscription hears: posted, company {company_id} entry {entry_number}"
            )
        }
        other => println!("the report's subscription hears: {other:?}"),
    }
    let live = reports
        .profit_and_loss(&manager, COMPANY, &[year_2026, first_half], None)
        .await?;
    let again = reports
        .profit_and_loss(&manager, COMPANY, &[year_2026, first_half], Some(pinned))
        .await?;
    println!(
        "manager reads it again live: through entry {}, net income {}",
        live.header.through_entry_number,
        amounts(&live.net_income, live.header.minor_units)
    );
    println!(
        "manager reads it through entry {pinned}: the same report as before: {}",
        again == pl
    );
    show(
        "manager reads the overview through entry -1",
        reports
            .overview(&manager, COMPANY, quarter, Some(-1))
            .await
            .map(|o| o.header.through_entry_number),
    );

    println!("\n== Issued reports (company 1)");
    let issued = reports
        .issue_report(
            &manager,
            COMPANY,
            &Issue::ProfitAndLoss(vec![year_2026, first_half]),
            Some(pinned),
        )
        .await?;
    println!(
        "manager issues the pinned profit and loss: report {} kind {} version {} through entry {} by {} at {}",
        issued.id,
        issued.kind,
        issued.implementation_version,
        issued.through_entry_number,
        issued.issued_by_user_id,
        issued.issued_at
    );
    println!(
        "its content is the pinned live report: {}",
        issued.content == serde_json::to_value(&pl)?
    );
    println!("its net income: {}", issued.content["net_income"]);
    println!("its membership: {}", issued.membership);
    println!(
        "its chart has {} accounts; its layout keeps {} configured sections",
        issued.chart.as_array().map_or(0, Vec::len),
        issued.layout["configured_sections"]
            .as_array()
            .map_or(0, Vec::len)
    );
    for issue in [
        Issue::BalanceSheet(vec![date(2026, 12, 31)]),
        Issue::CashPosition(vec![date(2026, 12, 31)]),
        Issue::TrialBalance(date(2026, 12, 31)),
    ] {
        show(
            &format!("manager issues {}", issue.kind()),
            reports
                .issue_report(&manager, COMPANY, &issue, None)
                .await
                .map(|r| {
                    (
                        r.id,
                        r.kind,
                        r.through_entry_number,
                        r.membership.as_object().map_or(0, |m| m.len()),
                    )
                }),
        );
    }
    // The layout changes after the issue; the stored copy does not.
    println!(
        "manager clears the sections: {}",
        save_sections(db, services, &manager, json!([])).await?
    );
    let stored = reports
        .get_issued_report(&viewer, COMPANY, issued.id)
        .await?;
    println!(
        "viewer reads report {} back: unchanged {}, still {} configured sections",
        stored.id,
        stored == issued,
        stored.layout["configured_sections"]
            .as_array()
            .map_or(0, Vec::len)
    );
    cash_position(
        "while the live cash position now lists",
        &reports
            .cash_position(&manager, COMPANY, &[date(2026, 12, 31)], Some(pinned))
            .await?,
    );
    show(
        "viewer (no issue capability) issues a trial balance",
        reports
            .issue_report(
                &viewer,
                COMPANY,
                &Issue::TrialBalance(date(2026, 12, 31)),
                None,
            )
            .await
            .map(|r| r.id),
    );
    show(
        "manager issues a balance sheet of five dates",
        reports
            .issue_report(
                &manager,
                COMPANY,
                &Issue::BalanceSheet(vec![date(2026, 12, 31); 5]),
                None,
            )
            .await
            .map(|r| r.id),
    );
    show(
        "tenant 2's manager reads company 1's issued report",
        reports
            .get_issued_report(&other, COMPANY, issued.id)
            .await
            .map(|r| r.id),
    );
    show(
        "manager reads issued report 999999",
        reports
            .get_issued_report(&manager, COMPANY, 999_999)
            .await
            .map(|r| r.id),
    );
    let client = db.get().await?;
    client
        .execute(
            "UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=1",
            &[],
        )
        .await?;
    show(
        "manager issues a trial balance while company 1 is archived",
        reports
            .issue_report(
                &manager,
                COMPANY,
                &Issue::TrialBalance(date(2026, 12, 31)),
                None,
            )
            .await
            .map(|r| r.id),
    );
    show(
        "manager still reads the archived company's profit and loss",
        reports
            .profit_and_loss(&manager, COMPANY, &[year_2026], Some(pinned))
            .await
            .map(|r| r.net_income),
    );
    show(
        "manager reads its pinned balance sheet",
        reports
            .balance_sheet(&manager, COMPANY, &[date(2026, 12, 31)], Some(pinned))
            .await
            .map(|r| {
                (
                    r.total_assets,
                    r.groups
                        .iter()
                        .map(|g| g.sections.len())
                        .collect::<Vec<_>>(),
                )
            }),
    );
    client
        .execute(
            "UPDATE companies SET status='active' WHERE tenant_id=1 AND id=1",
            &[],
        )
        .await?;
    println!("datasets: {:?}", Reports::evidence_datasets());
    let page = reports
        .evidence_page(&manager, COMPANY, "issued", Some(2), None)
        .await?;
    println!(
        "manager reads 2 issued rows: fields {:?}, ids {:?}, next {:?}",
        page.fields,
        page.rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        page.next
    );
    show(
        "manager reads a limit of 501",
        reports
            .evidence_page(&manager, COMPANY, "issued", Some(501), None)
            .await
            .map(|p| p.rows.len()),
    );

    println!("\n== Refusals");
    show(
        "viewer (view only) reads the balance sheet",
        reports
            .balance_sheet(&viewer, COMPANY, &[date(2026, 12, 31)], Some(pinned))
            .await
            .map(|r| r.total_assets),
    );
    show(
        "contractor (affiliated with company 2) reads company 1's profit and loss",
        reports
            .profit_and_loss(&contractor, COMPANY, &[year_2026], None)
            .await
            .map(|r| r.net_income),
    );
    show(
        "tenant 2's manager reads company 1's cash position",
        reports
            .cash_position(&other, COMPANY, &[date(2026, 12, 31)], None)
            .await
            .map(|r| r.totals),
    );
    show(
        "tenant 2's manager reads tenant 2's company 100 (no book)",
        reports
            .overview(&other, 100, quarter, None)
            .await
            .map(|r| r.net_income),
    );
    show(
        "contractor subscribes to company 1's postings",
        reports
            .subscribe(&contractor, COMPANY)
            .await
            .map(|_| "subscribed"),
    );
    let five: Vec<Period> = vec![quarter; 5];
    show(
        "manager reads profit and loss for five periods",
        reports
            .profit_and_loss(&manager, COMPANY, &five, None)
            .await
            .map(|r| r.net_income),
    );
    show(
        "manager reads profit and loss for no period",
        reports
            .profit_and_loss(&manager, COMPANY, &[], None)
            .await
            .map(|r| r.net_income),
    );
    show(
        "manager reads profit and loss from 2026-07-01 to 2026-06-30",
        reports
            .profit_and_loss(
                &manager,
                COMPANY,
                &[Period {
                    from: date(2026, 7, 1),
                    to: date(2026, 6, 30),
                }],
                None,
            )
            .await
            .map(|r| r.net_income),
    );
    show(
        "manager reads the balance sheet as of no date",
        reports
            .balance_sheet(&manager, COMPANY, &[], None)
            .await
            .map(|r| r.total_assets),
    );
    show(
        "manager reads the cash position through entry -3",
        reports
            .cash_position(&manager, COMPANY, &[date(2026, 12, 31)], Some(-3))
            .await
            .map(|r| r.totals),
    );
    show(
        "previous period of one ending at the calendar's start",
        previous_period(Period {
            from: Date::MIN,
            to: date(2026, 1, 1),
        }),
    );
    let foreign = ledger
        .list_accounts(&sign_in(db, services, 1, 4).await?, 2)
        .await?
        .first()
        .map(|account| account.id)
        .ok_or("a company 2 account")?;
    show(
        "manager opens a company 2 account's activity in company 1",
        reports
            .account_activity(&manager, COMPANY, foreign, &ActivityQuery::default())
            .await
            .map(|a| a.debits),
    );
    show(
        "manager opens 1000's activity 0 entries at a time",
        reports
            .account_activity(
                &manager,
                COMPANY,
                cash,
                &ActivityQuery {
                    limit: Some(0),
                    ..ActivityQuery::default()
                },
            )
            .await
            .map(|a| a.debits),
    );
    for (step, sql) in [
        (
            "edits an issued report",
            "UPDATE finance_reports_issued SET kind='trial_balance'",
        ),
        (
            "deletes an issued report",
            "DELETE FROM finance_reports_issued",
        ),
        (
            "truncates the issued reports",
            "TRUNCATE finance_reports_issued",
        ),
        (
            "inserts one with a negative entry number",
            "INSERT INTO finance_reports_issued(tenant_id,company_id,kind,implementation_version,through_entry_number,\
             content,chart,layout,membership,issued_by_user_id,issued_at) VALUES(1,1,'x','1',-1,'{}','[]','{}','{}',1,now())",
        ),
    ] {
        match client.execute(sql, &[]).await {
            Ok(rows) => println!("the database {step}: {rows} rows"),
            Err(error) => println!(
                "the database refuses when someone {step}: {}",
                error
                    .as_db_error()
                    .map_or(error.to_string(), |e| e.message().to_owned())
            ),
        }
    }
    Ok(())
}
