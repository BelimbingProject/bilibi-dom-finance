//! Reusable chart publication, proposal and independent company copies.
//! Uses the same reviewed handlers as the UI/MCP and the real facades.
use crate::sign_in;
use bilibi_agent_api::{Handler, Registry, WriteHandler};
use bilibi_company::{CompanyService, CreateOutcome, NewCompany};
use bilibi_database::{Db, Error, MutationBoundary};
use bilibi_finance_ledger::{AccountChange, Ledger, LedgerError, NewBook, NewChartTemplate};
use bilibi_module_registry::Services;
use jiff::civil::date;
use serde_json::json;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
fn ensure(ok: bool, detail: &str) -> Outcome<()> {
    if !ok {
        return Err(detail.into());
    }
    println!("{detail}: proved");
    Ok(())
}
fn handler(registry: &Registry, key: &str) -> Outcome<&'static dyn WriteHandler> {
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
    println!("\n== Reusable chart templates and proposal");
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let companies = services.get::<CompanyService>(bilibi_company::SERVICE)?;
    let manager = sign_in(db, services, 2, 5).await?;
    let outsider = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let new = NewChartTemplate {
        key: "practice-chart".into(),
        name: "Practice chart".into(),
        description: "Reusable company chart".into(),
        optional_group_codes: vec!["1100".into()],
    };
    ensure(
        matches!(
            ledger.save_chart_template(&viewer, 1, &new).await,
            Err(LedgerError::Unauthorized)
        ),
        "viewer cannot publish",
    )?;
    ensure(
        matches!(
            ledger.save_chart_template(&staff, 1, &new).await,
            Err(LedgerError::Unauthorized)
        ),
        "account manager without publication permission cannot publish",
    )?;
    ensure(
        ledger
            .save_chart_template(&outsider, 100, &new)
            .await
            .is_err(),
        "another tenant cannot publish the source chart",
    )?;
    let nested = NewChartTemplate {
        optional_group_codes: vec!["1000".into(), "1100".into()],
        ..new.clone()
    };
    ensure(
        matches!(
            ledger.save_chart_template(&manager, 100, &nested).await,
            Err(LedgerError::Invalid(_))
        ),
        "nested optional branches are refused before mutation",
    )?;
    let input = json!({"company_id":100,"key":new.key,"name":new.name,"description":new.description,"optional_group_codes":new.optional_group_codes});
    let save = handler(&registry, "finance.ledger.chart_template.save")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.template.rollback"),
        )
        .await?;
    let preview = save.preview(tx.sql(), &manager, &input).await?;
    println!(
        "publish review: {}",
        preview.envelope("finance.ledger.chart_template.save")
    );
    save.commit(&mut tx, &manager, &input).await?;
    tx.rollback().await?;
    ensure(
        !ledger
            .chart_templates(&manager, 100)
            .await?
            .iter()
            .any(|t| t.key == "saved:practice-chart"),
        "a rolled-back publication is absent",
    )?;
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.template.save"),
        )
        .await?;
    save.preview(tx.sql(), &manager, &input).await?;
    save.commit(&mut tx, &manager, &input).await?;
    tx.commit().await?;
    let account = ledger
        .list_accounts(&manager, 100)
        .await?
        .into_iter()
        .find(|a| a.postable())
        .ok_or(Error::NotFound)?;
    let update = handler(&registry, "finance.ledger.account.update")?;
    let changed =
        json!({"company_id":100,"account_id":account.id,"name":"Changed after template preview"});
    let mut stale_input = input.clone();
    stale_input["key"] = json!("stale-preview");
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.template.stale"),
        )
        .await?;
    update.preview(tx.sql(), &manager, &changed).await?;
    save.preview(tx.sql(), &manager, &stale_input).await?;
    update.commit(&mut tx, &manager, &changed).await?;
    ensure(
        matches!(
            save.commit(&mut tx, &manager, &stale_input).await,
            Err(bilibi_agent_api::Error::Backend(
                bilibi_database::Error::Conflict
            ))
        ),
        "same-plan source edit refuses unreviewed publication",
    )?;
    tx.rollback().await?;
    ensure(
        ledger
            .list_accounts(&manager, 100)
            .await?
            .iter()
            .any(|a| a.id == account.id && a.name == account.name),
        "stale publication plan rolls back its earlier chart edit",
    )?;
    let template = ledger
        .chart_templates(&manager, 100)
        .await?
        .into_iter()
        .find(|t| t.key == "saved:practice-chart")
        .ok_or(Error::NotFound)?;
    ensure(
        template.sections.len() == 1 && template.sections[0].name.contains("Current assets"),
        "optional branch saved as a section",
    )?;
    ensure(
        matches!(
            ledger.save_chart_template(&manager, 100, &new).await,
            Err(LedgerError::Invalid(_))
        ),
        "duplicate publication is refused without overwriting",
    )?;
    ensure(
        ledger
            .preview_chart_template(&outsider, 1, &template.key, &[])
            .await
            .is_err(),
        "published templates stay within the tenant",
    )?;
    ensure(
        matches!(
            ledger
                .propose_chart_template(&staff, 1, Some("my-small-business"))
                .await,
            Err(LedgerError::Unauthorized)
        ),
        "account manager cannot change the tenant proposal",
    )?;
    let admin = sign_in(db, services, 1, 4).await?;
    let grants = services.get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants
        .set_direct(
            &admin,
            1,
            bilibi_finance_ledger::MANAGE_TEMPLATES,
            Some(false),
        )
        .await?;
    ensure(
        !ledger.list_accounts(&outsider, 1).await?.is_empty(),
        "chart manager retains source read access while publication is denied",
    )?;
    ensure(
        matches!(
            ledger.save_chart_template(&outsider, 1, &new).await,
            Err(LedgerError::Unauthorized)
        ),
        "publication capability is checked independently of chart access",
    )?;
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::MANAGE_TEMPLATES, None)
        .await?;
    let settings = services.get::<bilibi_settings::Settings>("base/settings.store")?;
    let mut tx = boundary
        .begin(
            &mut client,
            admin.mutation_context("finance.walkthrough.template.operator-default"),
        )
        .await?;
    settings
        .put(
            &mut tx,
            &admin,
            bilibi_settings::Target::Global,
            "finance.ledger.default_chart_template",
            json!("my-small-business"),
        )
        .await?;
    tx.commit().await?;
    ensure(
        ledger.chart_template_proposal(&manager, 100).await?["configured"]
            == json!("my-small-business"),
        "operator global default is proposed across tenants",
    )?;
    let propose = handler(&registry, "finance.ledger.chart_template.propose")?;
    let proposal = json!({"company_id":100,"template":template.key});
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.template.propose"),
        )
        .await?;
    propose.preview(tx.sql(), &manager, &proposal).await?;
    propose.commit(&mut tx, &manager, &proposal).await?;
    tx.commit().await?;
    ensure(
        ledger.chart_template_proposal(&manager, 100).await?["configured"] == json!(template.key),
        "proposal round-trips through the contract",
    )?;
    ensure(
        ledger.chart_template_proposal(&outsider, 1).await?["configured"]
            == json!("my-small-business"),
        "tenant proposal leaves another tenant's inherited proposal unchanged",
    )?;
    let mut targets = Vec::new();
    for name in [
        "Template adopter A",
        "Template adopter B",
        "Template browser setup",
    ] {
        let company = match companies
            .create_company(
                &manager,
                &NewCompany {
                    name: name.into(),
                    ..NewCompany::default()
                },
            )
            .await?
        {
            CreateOutcome::Created(company) => company.id,
            other => return Err(format!("company create refused: {other:?}").into()),
        };
        crate::give_desk(db, 2, 5, company).await?;
        targets.push(company);
    }
    for &company in &targets[..2] {
        ledger
            .open_book(
                &manager,
                company,
                &NewBook {
                    currency_code: "MYR".into(),
                    minor_units: 2,
                },
            )
            .await?;
        ensure(
            ledger.list_accounts(&manager, company).await?.is_empty(),
            "opening with a configured proposal creates no accounts",
        )?;
        let sections = if company == targets[0] {
            Vec::new()
        } else {
            vec![template.sections[0].key.clone()]
        };
        let preview = ledger
            .preview_chart_template(&manager, company, &template.key, &sections)
            .await?;
        let apply = handler(&registry, "finance.ledger.chart_template.apply")?;
        let input = json!({"company_id":company,"template":template.key,"sections":sections});
        let mut tx = boundary
            .begin(
                &mut client,
                manager.mutation_context("finance.walkthrough.template.apply"),
            )
            .await?;
        let facts = apply
            .preview(tx.sql(), &manager, &input)
            .await?
            .envelope("finance.ledger.chart_template.apply");
        ensure(
            facts["result"]["changes"]["rows"] == json!(preview.template.rows),
            "contract and facade preview identical exact rows",
        )?;
        apply.commit(&mut tx, &manager, &input).await?;
        tx.commit().await?;
        let chart = ledger.list_accounts(&manager, company).await?;
        ensure(
            chart.len() == preview.template.rows.len(),
            "applied chart has exactly the reviewed rows",
        )?;
        ensure(
            chart.iter().any(|a| a.code == "1100") == !sections.is_empty(),
            "only chosen optional sections are created",
        )?;
        ensure(
            ledger
                .trial_balance(&manager, company, date(2026, 12, 31), None)
                .await?
                .rows
                .is_empty(),
            "template creates no opening balances",
        )?;
        ensure(
            ledger
                .apply_chart_template(&manager, company, &template.key, &sections)
                .await
                .is_err(),
            "retry cannot add duplicate accounts",
        )?;
    }
    let account = ledger
        .list_accounts(&manager, targets[0])
        .await?
        .into_iter()
        .find(|a| a.postable())
        .ok_or(Error::NotFound)?;
    ledger
        .update_account(
            &manager,
            targets[0],
            account.id,
            &AccountChange {
                name: Some("Company-specific account name".into()),
                ..AccountChange::default()
            },
        )
        .await?;
    ensure(
        ledger
            .list_accounts(&manager, targets[1])
            .await?
            .iter()
            .find(|a| a.code == account.code)
            .is_some_and(|a| a.name == account.name),
        "adopters customise their charts independently",
    )?;
    ensure(
        ledger
            .chart_templates(&manager, 100)
            .await?
            .into_iter()
            .find(|t| t.key == template.key)
            == Some(template.clone()),
        "template remains frozen after adopter changes",
    )?;
    ensure(
        !ledger
            .preview_chart_template(&manager, 100, &template.key, &[])
            .await?
            .applicable,
        "existing source chart cannot be reseeded",
    )?;
    let exported = ledger
        .evidence_page(&manager, 100, "chart_template", Some(500), None)
        .await?;
    ensure(
        exported
            .rows
            .iter()
            .any(|row| row["key"] == json!(template.key)),
        "saved template is in authorised evidence export",
    )?;
    ensure(db.get().await?.query_one("SELECT count(*) FROM base_audit_mutations WHERE auditable_type='finance/ledger.chart_template'",&[]).await?.get::<_,i64>(0)==1,"publication has one captured row audit; refused/rolled-back saves have none")?;
    let racing = NewChartTemplate {
        key: "duplicate-race".into(),
        optional_group_codes: Vec::new(),
        ..new.clone()
    };
    let (left, right) = tokio::join!(
        ledger.save_chart_template(&manager, 100, &racing),
        ledger.save_chart_template(&manager, targets[0], &racing)
    );
    ensure(
        matches!(
            (&left, &right),
            (Ok(_), Err(LedgerError::Invalid(_))) | (Err(LedgerError::Invalid(_)), Ok(_))
        ),
        "concurrent same-key publications from different companies produce one snapshot and one field refusal",
    )?;
    let rejected = db
        .get()
        .await?
        .execute(
            "UPDATE finance_ledger_chart_templates SET key=key WHERE tenant_id=2",
            &[],
        )
        .await
        .is_err();
    ensure(rejected, "database refuses editing a published snapshot")?;
    ledger.propose_chart_template(&manager, 100, None).await?;
    ensure(
        ledger.chart_template_proposal(&manager, 100).await?["proposed"].is_null(),
        "proposal can be cleared without changing charts",
    )?;
    ledger
        .propose_chart_template(&manager, 100, Some(&template.key))
        .await?;
    println!(
        "Browser setup company: {}; source company: 100; sign in as manager@other.example.test",
        targets[2]
    );
    Ok(())
}
