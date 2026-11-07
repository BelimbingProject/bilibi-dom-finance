//! Executable page contracts on the real Payroll PageService and database.
//! These are disposable walkthrough scenarios, never unit tests or product data.
use crate::sign_in;
use bilibi_authz::grants::Grants;
use bilibi_database::{Db, Error};
use bilibi_finance_payroll::{MappingChange, Payroll, MANAGE_SETTINGS, POST_POSTINGS};
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use serde_json::{json, Value};
use std::collections::HashMap;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

async fn mount(
    services: &Services,
    scope: &Scope,
    operation: &'static str,
) -> Outcome<Box<dyn LiveState<Scope, Error>>> {
    let pages = services.get::<PageService<Scope, Error>>("finance/payroll.pages")?;
    let map = HashMap::new();
    Ok(pages
        .0
        .mount(PageRequest {
            operation,
            scope: Some(scope),
            path: &map,
            query: &map,
            body: &[],
            framed: false,
        })
        .await?
        .unwrap())
}
async fn event(
    state: &mut dyn LiveState<Scope, Error>,
    scope: &Scope,
    value: Value,
) -> Outcome<String> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&value),
        })
        .await?
        .html)
}
async fn refresh(state: &mut dyn LiveState<Scope, Error>, scope: &Scope) -> Outcome<String> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?
        .html)
}

pub async fn run(db: &Db, services: &Services, historical: i64) -> Outcome<()> {
    let payroll = services.get::<Payroll>(bilibi_finance_payroll::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let manager = sign_in(db, services, 1, 1).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let mut setup = mount(services, &manager, "setup").await?;
    let settings = payroll.get_settings(&manager, 1).await?;
    let entries = payroll.list_posting_events(&manager, 1).await?;
    let mut postings = mount(services, &manager, "postings").await?;
    let html = refresh(postings.as_mut(), &manager).await?;
    if !payroll.people_available() {
        assert!(html.contains("Payroll is not in use"));
        assert!(!html.contains("id=\"payroll-runs\""));
        for kind in ["post_run", "post_claim", "post_all_claims"] {
            let html = event(
                postings.as_mut(),
                &manager,
                json!({"type": kind, "id": "9003"}),
            )
            .await?;
            assert!(html.contains("The payroll and claims source is not installed."));
        }
    } else {
        assert!(html.contains("id=\"payroll-runs-table\""));
        assert!(html.contains("id=\"payroll-claims-table\""));
    }
    assert_eq!(payroll.list_posting_events(&manager, 1).await?, entries);

    // Each posting account is chosen in place; once set up, one saves alone.
    let before = payroll.list_settings_changes(&manager, 1).await?.len();
    let swap = json!({"type": "save_account", "field": "net_pay_account_id", "id": "", "value": settings.claims_payable_account_id.to_string()});
    let html = event(setup.as_mut(), &manager, swap).await?;
    assert!(html.contains("id=\"payroll-settings-net-pay\""));
    assert_eq!(
        payroll.get_settings(&manager, 1).await?.net_pay_account_id,
        settings.claims_payable_account_id
    );
    assert_eq!(
        payroll
            .get_settings(&manager, 1)
            .await?
            .claims_payable_account_id,
        settings.claims_payable_account_id,
        "the other account stays as it was"
    );
    assert_eq!(
        payroll.list_settings_changes(&manager, 1).await?.len(),
        before + 1
    );
    let back = json!({"type": "save_account", "field": "net_pay_account_id", "id": "", "value": settings.net_pay_account_id.to_string()});
    event(setup.as_mut(), &manager, back).await?;
    let html = event(
        setup.as_mut(),
        &manager,
        json!({"type": "save_account", "field": "net_pay_account_id", "id": "", "value": "invalid"}),
    )
    .await?;
    assert!(html.contains("choose an account"));
    assert_eq!(payroll.get_settings(&manager, 1).await?, settings);

    // "+ Mapping" opens the dialog; a refusal shows beside its field, the
    // dialog stays open with what was typed, and a save closes it.
    let html = event(setup.as_mut(), &manager, json!({"type": "new_mapping"})).await?;
    assert!(html.contains("id=\"payroll-mapping-modal\""));
    let mut mapping = json!({"type": "save_mapping", "mapping[kind]": "component", "mapping[code]": "ui-deduction", "mapping[direction]": "deduction", "mapping[liability_account_id]": ""});
    let html = event(setup.as_mut(), &manager, mapping.clone()).await?;
    assert!(
        html.contains("payroll-mapping-liability_account_id-error-0") || html.contains("is needed")
    );
    assert!(html.contains("value=\"ui-deduction\""));
    assert!(html.contains("id=\"payroll-mapping-modal\""));
    // A deduction posts to a liability only: the dialog offers no expense account.
    assert!(!html.contains("id=\"payroll-mapping-expense_account_id\""));
    mapping["mapping[liability_account_id]"] = json!(settings.net_pay_account_id.to_string());
    let html = event(setup.as_mut(), &manager, mapping.clone()).await?;
    assert!(html.contains("Mapping ui-deduction (Deduction) saved."));
    assert!(!html.contains("id=\"payroll-mapping-modal\""));
    // An earning saves with its direction as the facade names it.
    event(setup.as_mut(), &manager, json!({"type": "new_mapping"})).await?;
    let expense = bilibi_finance_ledger::Ledger::list_accounts(
        &*services.get::<bilibi_finance_ledger::Ledger>(bilibi_finance_ledger::SERVICE)?,
        &manager,
        1,
    )
    .await?
    .into_iter()
    .find(|a| a.postable() && a.account_type == bilibi_finance_ledger::AccountType::Expense)
    .expect("an expense account");
    let html = event(
        setup.as_mut(),
        &manager,
        json!({"type": "save_mapping", "mapping[kind]": "component", "mapping[code]": "ui-earning", "mapping[direction]": "earning", "mapping[expense_account_id]": expense.id.to_string()}),
    )
    .await?;
    assert!(html.contains("Mapping ui-earning (Earning) saved."));
    let mapped = payroll
        .list_mappings(&manager, 1)
        .await?
        .into_iter()
        .find(|m| m.code == "ui-deduction")
        .unwrap();
    // A mapping's account is changed in place on its row.
    let before = payroll.list_mapping_changes(&manager, 1).await?.len();
    event(
        setup.as_mut(),
        &manager,
        json!({"type": "save_mapping_account", "id": mapped.id.to_string(), "field": "liability_account_id", "value": settings.claims_payable_account_id.to_string()}),
    )
    .await?;
    assert_eq!(
        payroll.list_mapping_changes(&manager, 1).await?.len(),
        before + 1
    );
    assert_eq!(
        payroll
            .list_mappings(&manager, 1)
            .await?
            .into_iter()
            .find(|m| m.id == mapped.id)
            .unwrap()
            .liability_account_id,
        Some(settings.claims_payable_account_id)
    );
    // Each of the three tables has independent arrangement state.
    let tables = [
        "payroll-mappings-table",
        "payroll-settings-changes-table",
        "payroll-mapping-changes-table",
    ];
    for (index, table) in tables.iter().enumerate() {
        let column = if index == 1 { "claims" } else { "liability" };
        let html = event(
            setup.as_mut(),
            &manager,
            json!({"type":format!("grid_{index}"), "op":"remove", "spec":column}),
        )
        .await?;
        assert!(!html.contains(&format!("id=\"{table}-chip-{column}\"")));
        // Removing a same-named column cannot alter another table.
        let other = if index == 0 { tables[2] } else { tables[0] };
        assert!(html.contains(&format!("id=\"{other}-chip-liability\"")));
        event(
            setup.as_mut(),
            &manager,
            json!({"type":format!("grid_{index}"), "op":"reset"}),
        )
        .await?;
    }
    // A stale page rechecks a revoked capability and writes nothing.
    let mappings_before = payroll.list_mappings(&manager, 1).await?;
    let changes_before = payroll.list_mapping_changes(&manager, 1).await?;
    grants
        .set_direct(&admin, 1, MANAGE_SETTINGS, Some(false))
        .await?;
    assert!(event(
        setup.as_mut(),
        &manager,
        json!({"type": "save_mapping_account", "id": mapped.id.to_string(), "field": "liability_account_id", "value": settings.net_pay_account_id.to_string()}),
    )
    .await?
    .contains("You do not have permission for that action."));
    assert_eq!(payroll.list_mappings(&manager, 1).await?, mappings_before);
    assert_eq!(
        payroll.list_mapping_changes(&manager, 1).await?,
        changes_before
    );
    grants.set_direct(&admin, 1, MANAGE_SETTINGS, None).await?;
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::VIEW, Some(false))
        .await?;
    let html = refresh(setup.as_mut(), &manager).await?;
    assert!(html.contains("Account unavailable"));
    assert!(html.contains("id=\"payroll-mappings-table\""));
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::VIEW, None)
        .await?;

    // The reversal dialog is the confirmation. A malformed date may never
    // silently become company-local today, and the dialog stays open.
    let html = event(
        postings.as_mut(),
        &manager,
        json!({"type":"reverse", "id":historical.to_string()}),
    )
    .await?;
    assert!(html.contains("id=\"payroll-reversal-modal\""));
    let draft = json!({"type":"confirm_reversal", "reversal[reason]":"UI historical correction", "reversal[entry_date]":"not-a-date"});
    let html = event(postings.as_mut(), &manager, draft.clone()).await?;
    assert!(html.contains("payroll-reversal-date-error-0"));
    assert!(html.contains("id=\"payroll-reversal-modal\""));
    let mut draft = draft;
    draft["reversal[entry_date]"] = json!("2027-02-04");
    for malformed in [json!(123), json!([]), json!({}), Value::Null] {
        let mut forged = draft.clone();
        forged["reversal[entry_date]"] = malformed;
        let html = event(postings.as_mut(), &manager, forged).await?;
        assert!(html.contains("payroll-reversal-date-error-0"));
        assert!(payroll
            .get_posting(&manager, 1, historical)
            .await?
            .reversal
            .is_none());
    }
    // Cancel closes the dialog and writes nothing.
    let html = event(
        postings.as_mut(),
        &manager,
        json!({"type":"cancel_reversal"}),
    )
    .await?;
    assert!(!html.contains("id=\"payroll-reversal-modal\""));
    assert!(payroll
        .get_posting(&manager, 1, historical)
        .await?
        .reversal
        .is_none());
    event(
        postings.as_mut(),
        &manager,
        json!({"type":"reverse", "id":historical.to_string()}),
    )
    .await?;
    grants
        .set_direct(&admin, 1, POST_POSTINGS, Some(false))
        .await?;
    assert!(event(postings.as_mut(), &manager, draft.clone())
        .await?
        .contains("You do not have permission for that action."));
    assert!(payroll
        .get_posting(&manager, 1, historical)
        .await?
        .reversal
        .is_none());
    grants.set_direct(&admin, 1, POST_POSTINGS, None).await?;
    assert!(event(postings.as_mut(), &manager, draft.clone())
        .await?
        .contains("Reversed as journal"));
    // A repeated submit after it closed writes nothing more.
    let count = payroll.list_posting_events(&manager, 1).await?.len();
    event(postings.as_mut(), &manager, draft).await?;
    assert_eq!(payroll.list_posting_events(&manager, 1).await?.len(), count);
    assert_eq!(
        payroll
            .get_posting(&manager, 1, historical)
            .await?
            .reversal
            .unwrap()
            .reason,
        "UI historical correction"
    );

    // An unpaged change history must still show the newest change past the
    // evidence page boundary. Accounts alternate so every call is a real write.
    let before = payroll.list_mapping_changes(&manager, 1).await?.len();
    for i in 0..501 {
        payroll
            .put_mapping(
                &manager,
                1,
                &MappingChange {
                    kind: "component".into(),
                    code: "history-boundary".into(),
                    direction: "deduction".into(),
                    expense_account_id: None,
                    liability_account_id: Some(if i % 2 == 0 {
                        settings.net_pay_account_id
                    } else {
                        settings.claims_payable_account_id
                    }),
                    reason: Some(format!("Boundary {i}")),
                },
            )
            .await?;
    }
    assert_eq!(
        payroll.list_mapping_changes(&manager, 1).await?.len(),
        before + 501
    );
    assert!(refresh(setup.as_mut(), &manager)
        .await?
        .contains("Boundary 500"));
    println!(
        "Payroll UI contracts passed: People absent/no writes; accounts in place; + Mapping dialog with field refusals and only its direction's accounts; row accounts in place; independent columns; Ledger withheld; revoked settings and reversal grants; invalid date keeps the dialog; cancel/single commit; complete history past 500"
    );
    Ok(())
}
