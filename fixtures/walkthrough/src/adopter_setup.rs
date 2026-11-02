//! First-adopter setup proof against real contracts and a disposable database.
use crate::{sign_in, COMPANY};
use bilibi_agent_api::{Handler, Registry, WriteHandler};
use bilibi_database::{Db, Error, MutationBoundary};
use bilibi_finance_ledger::{Ledger, MinorUnits, Money};
use bilibi_finance_reconciliation::Reconciliation;
use bilibi_finance_reports::Reports;
use bilibi_module_registry::Services;
use serde_json::json;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
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
/// Roll every opening back so the existing walkthrough starts unchanged.
pub async fn opening(db: &Db, services: &Services) -> Outcome<()> {
    bilibi_walkthrough_fixture::install(db).await?;
    services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let manager = sign_in(db, services, 1, 1).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let handler = write(&registry, "finance.ledger.book.open")?;
    let input = json!({"company_id":COMPANY,"currency_code":"MYR","minor_units":2,"separation_required":false,"approval_reason":"One-person company"});
    let mut client = db.get().await?;
    for (label, scope, input, accepted) in [
        (
            "missing reason",
            &manager,
            json!({"company_id":COMPANY,"currency_code":"MYR","minor_units":2,"separation_required":false}),
            false,
        ),
        (
            "chart manager without duty permission",
            &staff,
            input.clone(),
            false,
        ),
        ("one-person company", &manager, input, true),
        (
            "default independent approval",
            &manager,
            json!({"company_id":COMPANY,"currency_code":"MYR","minor_units":2}),
            true,
        ),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                scope.mutation_context("finance.walkthrough.adopter.open"),
            )
            .await?;
        match handler.preview(tx.sql(), scope, &input).await {
            Ok(review) => {
                if !accepted {
                    return Err(Error::Internal.into());
                }
                println!(
                    "{label} preview: {}",
                    review.envelope("finance.ledger.book.open")
                );
                handler.commit(&mut tx, scope, &input).await?;
                let state = registry
                    .call(
                        tx.sql(),
                        scope,
                        "finance.ledger.book.get",
                        &json!({"company_id":COMPANY}),
                    )
                    .await?;
                println!("{label} before rollback: {}", state["result"]);
            }
            Err(error) => {
                if accepted {
                    return Err(error.into());
                }
                println!("{label}: refused ({error})");
            }
        }
        tx.rollback().await?;
    }
    let state = registry
        .call(
            &**client,
            &manager,
            "finance.ledger.book.get",
            &json!({"company_id":COMPANY}),
        )
        .await?;
    if !state["result"]["book"].is_null() {
        return Err(Error::Internal.into());
    }
    println!(
        "opening rollbacks leave no book or duty change: {}",
        state["result"]
    );
    let status = db.status();
    let mut held = Vec::new();
    for _ in 0..status.max_size - status.size + status.available {
        held.push(db.get().await?);
    }
    let waiting = std::time::Duration::from_secs(5);
    let input = json!({"company_id":COMPANY,"currency_code":"MYR","minor_units":2});
    let tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.adopter.pool"),
        )
        .await?;
    let review = tokio::time::timeout(waiting, handler.preview(tx.sql(), &manager, &input))
        .await
        .map_err(|_| "book.open preview waited for a second pooled connection")??;
    let listed = tokio::time::timeout(
        waiting,
        registry.call(
            tx.sql(),
            &manager,
            "finance.ledger.book.currencies",
            &json!({"company_id":COMPANY}),
        ),
    )
    .await
    .map_err(|_| "book.currencies waited for a second pooled connection")??;
    tx.rollback().await?;
    drop(held);
    println!(
        "book.open preview and book.currencies complete with every pool slot occupied: {} / {}",
        review.envelope("finance.ledger.book.open"),
        listed["result"]["reference"]
    );
    for (value, units) in [
        ("12345.50", 2),
        ("-12345.50", 2),
        ("12345", 0),
        ("12345.1234", 4),
    ] {
        let units = MinorUnits::new(units).ok_or(Error::Invalid)?;
        println!(
            "money {value} at {units} decimal places: {}",
            Money::parse(value, units)?.display(units)
        );
    }
    println!(
        "money without chart access: {}",
        Money::parse("12345.50", MinorUnits::new(2).ok_or(Error::Invalid)?)?.display(None)
    );
    Ok(())
}

pub async fn reports(db: &Db, services: &Services) -> Outcome<()> {
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let reports = services.get::<Reports>(bilibi_finance_reports::SERVICE)?;
    let reconciliation = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let handler = write(&registry, "finance.reports.sections.set")?;
    let mut client = db.get().await?;
    let original = registry
        .call(
            &**client,
            &manager,
            "finance.reports.sections.get",
            &json!({"company_id":COMPANY}),
        )
        .await?["result"]["sections"]
        .clone();
    let empty = json!({"company_id":COMPANY,"sections":[]});
    let explicit = json!({"company_id":COMPANY,"sections":[{"report":"cash_position","name":"No cash selected","accounts":[]}]});
    for input in [&empty, &explicit] {
        let mut tx = boundary
            .begin(
                &mut client,
                manager.mutation_context("finance.walkthrough.adopter.layout"),
            )
            .await?;
        handler.preview(tx.sql(), &manager, input).await?;
        handler.commit(&mut tx, &manager, input).await?;
        let own = registry
            .call(
                tx.sql(),
                &manager,
                "finance.reports.sections.get",
                &json!({"company_id":COMPANY}),
            )
            .await?;
        if own["result"]["sections"] != input["sections"] {
            return Err(Error::Internal.into());
        }
        println!(
            "layout reads its own uncommitted change through OperationClient: {}",
            own["result"]
        );
        tx.commit().await?;
        let report = reports
            .cash_position(&viewer, COMPANY, &[jiff::civil::date(2026, 12, 31)], None)
            .await?;
        let actual: Vec<_> = report
            .sections
            .iter()
            .flat_map(|section| section.rows.iter().map(|row| row.account_id))
            .collect();
        let expected = if input == &empty {
            reconciliation.cash_account_ids(&viewer, COMPANY).await?
        } else {
            Vec::new()
        };
        let mut actual = actual;
        actual.sort_unstable();
        let mut expected = expected;
        expected.sort_unstable();
        if actual != expected {
            return Err(Error::Internal.into());
        }
        println!(
            "cash membership for ledger viewer agrees with default/explicit choice: {actual:?}; totals {:?}",
            report.totals
        );
    }
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.adopter.layout.rollback"),
        )
        .await?;
    handler.preview(tx.sql(), &manager, &empty).await?;
    handler.preview(tx.sql(), &manager, &explicit).await?;
    handler.commit(&mut tx, &manager, &empty).await?;
    match handler.commit(&mut tx, &manager, &explicit).await {
        Err(bilibi_agent_api::Error::Backend(bilibi_database::Error::Conflict)) => println!(
            "two layout steps reviewed before execution: stale second step rolls the shared transaction back"
        ),
        _ => return Err(Error::Internal.into()),
    }
    tx.rollback().await?;
    let after = registry
        .call(
            &**client,
            &manager,
            "finance.reports.sections.get",
            &json!({"company_id":COMPANY}),
        )
        .await?;
    if after["result"]["sections"] != explicit["sections"] {
        return Err(Error::Internal.into());
    }
    let restore = json!({"company_id":COMPANY,"sections":original});
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.adopter.layout.restore"),
        )
        .await?;
    handler.preview(tx.sql(), &manager, &restore).await?;
    handler.commit(&mut tx, &manager, &restore).await?;
    tx.commit().await?;
    println!(
        "layout restored; book uses {} decimal places",
        ledger.get_book(&manager, COMPANY).await?.minor_units
    );
    Ok(())
}
