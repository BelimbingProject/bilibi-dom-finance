//! The starter chart and account group walkthrough, on Company 100 (tenant 2),
//! which no other walkthrough has given a chart: a company that opens its book
//! and then chooses how to start. Every line is the facade's, or the operation
//! contract's, actual answer; nothing here is a test double. A starter chart
//! is previewed before it is applied, never applied to a chart that has
//! accounts, and never leaves a link between the company and the template.
use crate::sign_in;
use bilibi_agent_api::{Handler, Registry, WriteHandler};
use bilibi_database::{Db, Error, MutationBoundary};
use bilibi_finance_ledger::{
    AccountGroupChange, Ledger, LedgerError, NewAccount, NewBook, NewEntry, NewFiscalYear, NewLine,
    PostOptions, SERVICE,
};
use bilibi_module_registry::Services;
use jiff::civil::date;
use serde_json::json;
use std::fmt::Debug;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
const COMPANY: i64 = 100;
const TEMPLATE: &str = "my-small-business";

fn show<T: Debug>(step: &str, result: Result<T, LedgerError>) {
    match result {
        Ok(value) => println!("{step}: {value:?}"),
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}

fn write(registry: &Registry, key: &str) -> Outcome<&'static dyn WriteHandler> {
    match registry
        .operations
        .get(key)
        .ok_or(Error::NotFound)?
        .definition
        .handler
    {
        Handler::SafeWrite(handler) => Ok(handler),
        _ => Err(Error::Invalid.into()),
    }
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    println!("\n== Starter charts and account groups (Company 100, tenant 2)");
    let ledger = services.get::<Ledger>(SERVICE)?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let manager = sign_in(db, services, 2, 5).await?;
    let first_tenant_manager = sign_in(db, services, 1, 1).await?;
    let first_tenant_viewer = sign_in(db, services, 1, 2).await?;

    for template in ledger.chart_templates(&manager, COMPANY).await? {
        let core: Vec<_> = template
            .rows
            .iter()
            .filter(|r| r.section.is_none())
            .collect();
        let groups = core.iter().filter(|r| r.is_group).count();
        println!(
            "starter chart {} ({}): {:?}, core {groups} groups and {} accounts",
            template.key,
            template.name,
            template.review,
            core.len() - groups
        );
        for section in &template.sections {
            println!(
                "  optional section {} ({}): {} rows, unticked by default",
                section.key,
                section.name,
                template
                    .rows
                    .iter()
                    .filter(|r| r.section.as_deref() == Some(section.key.as_str()))
                    .count()
            );
        }
    }
    show(
        "manager previews the starter chart before opening the book",
        ledger
            .preview_chart_template(&manager, COMPANY, TEMPLATE, &[])
            .await
            .map(|p| {
                (
                    p.applicable,
                    p.existing_rows,
                    p.conflicting_codes,
                    p.template.rows.len(),
                    p.chosen_sections,
                )
            }),
    );
    for chosen in [vec!["sst"], vec!["payroll"], vec!["sst", "payroll"]] {
        let chosen: Vec<String> = chosen.into_iter().map(String::from).collect();
        show(
            &format!("manager previews with sections {chosen:?} ticked"),
            ledger
                .preview_chart_template(&manager, COMPANY, TEMPLATE, &chosen)
                .await
                .map(|p| (p.template.rows.len(), p.chosen_sections)),
        );
    }
    show(
        "manager previews with a section the chart does not have",
        ledger
            .preview_chart_template(&manager, COMPANY, TEMPLATE, &["nope".into()])
            .await
            .map(|p| p.template.rows.len()),
    );
    show(
        "manager previews a starter chart that does not exist",
        ledger
            .preview_chart_template(&manager, COMPANY, "nope", &[])
            .await
            .map(|p| p.applicable),
    );
    show(
        "tenant 1's manager previews tenant 2's company 100",
        ledger
            .preview_chart_template(&first_tenant_manager, COMPANY, TEMPLATE, &[])
            .await
            .map(|p| p.applicable),
    );
    show(
        "tenant 1's viewer applies the starter chart to company 1",
        ledger
            .apply_chart_template(&first_tenant_viewer, 1, TEMPLATE, &[])
            .await,
    );
    let before = ledger.list_accounts(&first_tenant_manager, 1).await?.len();
    show(
        "tenant 1's manager applies the starter chart to company 1, which has accounts",
        ledger
            .apply_chart_template(&first_tenant_manager, 1, TEMPLATE, &[])
            .await,
    );
    let after = ledger.list_accounts(&first_tenant_manager, 1).await?.len();
    println!(
        "company 1's chart before and after the refused application: {before} and {after} rows"
    );

    ledger
        .open_book(
            &manager,
            COMPANY,
            &NewBook {
                currency_code: "MYR".into(),
                minor_units: 2,
            },
        )
        .await?;
    println!("manager opens company 100's book; its chart is empty");

    // The operation contract an agent calls, and the page calls: list, get,
    // then a reviewed apply. The first apply is rolled back to show a review
    // changes nothing until it is committed.
    let listed = registry
        .call(
            &mut **db.get().await?,
            &manager,
            "finance.ledger.chart_template.list",
            &json!({"company_id": COMPANY}),
        )
        .await?;
    println!("contract chart_template.list: {}", listed["result"]);
    let handler = write(&registry, "finance.ledger.chart_template.apply")?;
    let mut client = db.get().await?;
    for sections in [
        json!([]),
        json!(["sst"]),
        json!(["payroll"]),
        json!(["sst", "payroll"]),
        json!([
            "sst",
            "payroll",
            "inventory",
            "intangibles",
            "tax_recoverable",
            "deposits",
            "returns",
            "finance_income"
        ]),
    ] {
        let input = json!({"company_id": COMPANY, "template": TEMPLATE, "sections": sections});
        let mut tx = boundary
            .begin(
                &mut client,
                manager.mutation_context("finance.walkthrough.chart.dry-run"),
            )
            .await?;
        let review = handler.preview(tx.sql(), &manager, &input).await?;
        let envelope = review.envelope("finance.ledger.chart_template.apply");
        let listed: Vec<&str> = envelope["result"]["changes"]["rows"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|r| r["code"].as_str())
            .collect();
        println!(
            "contract chart_template.apply review with sections {sections}: {} groups, {} accounts, {} rows listed; SST group 2140 listed: {}, payroll group 2150 listed: {}",
            envelope["result"]["changes"]["groups"],
            envelope["result"]["changes"]["accounts"],
            listed.len(),
            listed.contains(&"2140"),
            listed.contains(&"2150"),
        );
        handler.commit(&mut tx, &manager, &input).await?;
        tx.rollback().await?;
        println!(
            "contract apply rolled back: {} rows in the chart",
            ledger
                .list_accounts_on(&**client, &manager, COMPANY)
                .await?
                .len()
        );
    }
    let unknown = json!({"company_id": COMPANY, "template": TEMPLATE, "sections": ["nope"]});
    let tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.chart.unknown-section"),
        )
        .await?;
    println!(
        "contract apply with an unknown section: refused = {}",
        handler.preview(tx.sql(), &manager, &unknown).await.is_err()
    );
    tx.rollback().await?;

    // The committed application ticks only the SST section; no other
    // section was chosen, so none of their rows are created.
    let sections = vec!["sst".to_owned()];
    let input = json!({"company_id": COMPANY, "template": TEMPLATE, "sections": sections});

    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.chart.apply"),
        )
        .await?;
    handler.preview(tx.sql(), &manager, &input).await?;
    let record = handler.commit(&mut tx, &manager, &input).await?;
    tx.commit().await?;
    println!(
        "contract apply committed: {}",
        record.envelope("finance.ledger.chart_template.apply")
    );

    drop(client);
    // What exists is exactly what the preview listed.
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    let expected = ledger
        .preview_chart_template(&manager, COMPANY, TEMPLATE, &sections)
        .await?
        .template;
    let template = &expected;
    let by_id: std::collections::HashMap<i64, &str> =
        chart.iter().map(|a| (a.id, a.code.as_str())).collect();
    let same = chart.len() == template.rows.len()
        && template.rows.iter().all(|row| {
            chart.iter().any(|a| {
                a.code == row.code
                    && a.name == row.name
                    && a.account_type == row.account_type
                    && a.is_group == row.is_group
                    && a.retained_earnings == row.retained_earnings
                    && a.parent_id.and_then(|id| by_id.get(&id).copied())
                        == row.parent_code.as_deref()
            })
        });
    let has = |code: &str| chart.iter().any(|a| a.code == code);
    println!(
        "SST section created (2140, 2141, 2142): {}; every other section omitted (2125, 2150, 2160, 6120, 6130, 6135, 1160, 1170, 1240, 1245, 6392, 2130, 4300, 4520): {}",
        ["2140", "2141", "2142"].iter().all(|c| has(c)),
        [
            "2125", "2150", "2160", "6120", "6130", "6135", "1160", "1170", "1240", "1245", "6392",
            "2130", "4300", "4520"
        ]
        .iter()
        .all(|c| !has(c)),
    );
    println!(
        "created {} rows ({} groups, {} retained-earnings accounts); equal to the preview: {same}",
        chart.len(),
        chart.iter().filter(|a| a.is_group).count(),
        chart.iter().filter(|a| a.retained_earnings).count()
    );
    show(
        "manager applies the starter chart again",
        ledger
            .apply_chart_template(&manager, COMPANY, TEMPLATE, &sections)
            .await,
    );
    show(
        "manager previews it again",
        ledger
            .preview_chart_template(&manager, COMPANY, TEMPLATE, &sections)
            .await
            .map(|p| (p.applicable, p.existing_rows, p.conflicting_codes.len())),
    );
    let accounts_audited: i64 = db
        .get()
        .await?
        .query_one(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='finance.ledger.account' AND auditable_id IN (SELECT id::text FROM finance_ledger_accounts WHERE company_id=$1)",
            &[&COMPANY],
        )
        .await?
        .get(0);
    println!("audit mutations for company 100's chart rows: {accounts_audited}");

    // No balance appears from a starter chart; groups total what is under them.
    ledger
        .open_fiscal_year(
            &manager,
            COMPANY,
            &NewFiscalYear {
                start_date: Some(date(2026, 1, 1)),
                end_date: None,
            },
        )
        .await?;
    let find = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    let (bank, capital, current_assets, assets) =
        (find("1120")?, find("3100")?, find("1100")?, find("1000")?);
    let units = ledger.get_book(&manager, COMPANY).await?.minor_units;
    let money = |text: &str| bilibi_finance_ledger::Money::parse(text, units).expect("amount");
    let entry = |description: &str, debit: i64, credit: i64, amount: &str| NewEntry {
        entry_date: date(2026, 10, 1),
        description: description.into(),
        reference: None,
        lines: vec![
            NewLine::debit(debit, money(amount)),
            NewLine::credit(credit, money(amount)),
        ],
    };
    show(
        "manager reads the chart before any posting",
        ledger
            .chart_balances(&manager, COMPANY, date(2026, 12, 31), None)
            .await
            .map(|c| {
                c.rows
                    .iter()
                    .filter(|r| {
                        r.balance.is_some_and(|b| !b.is_zero())
                            || r.total.is_some_and(|t| !t.is_zero())
                    })
                    .count()
            }),
    );
    show(
        "manager posts to the group 1100: Dr 1100 / Cr 3100 10.00",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry("Posted to a group", current_assets, capital, "10.00"),
                &PostOptions::default(),
            )
            .await
            .map(|e| e.entry_number),
    );
    show(
        "manager posts capital: Dr 1120 Bank / Cr 3100 1,000.00",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry("Capital injected", bank, capital, "1000.00"),
                &PostOptions::default(),
            )
            .await
            .map(|e| e.entry_number),
    );
    let balances = ledger
        .chart_balances(&manager, COMPANY, date(2026, 12, 31), None)
        .await?;
    for code in ["1000", "1100", "1120", "1130", "3000", "3100"] {
        let row = balances
            .rows
            .iter()
            .find(|r| r.account.code == code)
            .ok_or(Error::NotFound)?;
        println!(
            "  {} {:<22} {:<7} balance {:>10} total {:>10}",
            row.account.code,
            row.account.name,
            if row.account.is_group {
                "group"
            } else {
                "account"
            },
            row.balance.map_or("-".into(), |m| m.shown(units)),
            row.total.map_or("-".into(), |m| m.shown(units)),
        );
    }
    let trial = ledger
        .trial_balance(&manager, COMPANY, date(2026, 12, 31), None)
        .await?;
    println!(
        "trial balance rows (accounts with postings only): {:?}",
        trial
            .rows
            .iter()
            .map(|r| r.account.code.as_str())
            .collect::<Vec<_>>()
    );
    println!(
        "group 1000 total equals its accounts' balances: {}",
        balances
            .rows
            .iter()
            .find(|r| r.account.id == assets)
            .and_then(|r| r.total)
            == Some(
                balances
                    .rows
                    .iter()
                    .filter(|r| !r.account.is_group
                        && r.account.account_type == bilibi_finance_ledger::AccountType::Asset)
                    .filter_map(|r| r.balance)
                    .sum()
            )
    );

    // The company's chart is its own: renaming here never reaches the
    // template, and nothing links the two.
    show(
        "manager renames group 1100",
        ledger
            .update_account_group(
                &manager,
                COMPANY,
                current_assets,
                &AccountGroupChange {
                    name: Some("Current assets (company's own name)".into()),
                    ..AccountGroupChange::default()
                },
            )
            .await
            .map(|g| g.name),
    );
    println!(
        "the template's group 1100 is still named {:?}",
        ledger
            .chart_templates(&manager, COMPANY)
            .await?
            .iter()
            .find(|t| t.key == TEMPLATE)
            .and_then(|t| t.rows.iter().find(|r| r.code == "1100"))
            .map(|r| r.name.as_str())
    );
    show(
        "manager adds an account 1135 under the template's group 1100",
        ledger
            .create_account(&manager, COMPANY, &{
                let mut new = NewAccount::new(
                    "1135",
                    "Staff advances",
                    bilibi_finance_ledger::AccountType::Asset,
                );
                new.parent_id = Some(current_assets);
                new
            })
            .await
            .map(|a| a.code),
    );
    Ok(())
}
