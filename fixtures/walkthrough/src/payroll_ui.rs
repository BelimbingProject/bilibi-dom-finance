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

    let mut save = json!({"type": "save_settings", "settings[net_pay_account_id]": settings.claims_payable_account_id.to_string(), "settings[claims_payable_account_id]": settings.net_pay_account_id.to_string(), "settings[reason]": "界".repeat(400)});
    let before = payroll.list_settings_changes(&manager, 1).await?.len();
    save["type"] = json!("change_settings");
    event(setup.as_mut(), &manager, save.clone()).await?;
    assert_eq!(
        payroll.list_settings_changes(&manager, 1).await?.len(),
        before,
        "typing must not write"
    );
    save["type"] = json!("save_settings");
    let html = event(setup.as_mut(), &manager, save.clone()).await?;
    assert!(html.contains("Posting accounts saved."));
    assert_eq!(
        payroll
            .list_settings_changes(&manager, 1)
            .await?
            .last()
            .unwrap()["reason"],
        "界".repeat(400)
    );
    save["settings[net_pay_account_id]"] = json!("invalid");
    let html = event(setup.as_mut(), &manager, save).await?;
    assert!(html.contains("payroll-settings-net-pay-account-error-0"));

    let mut mapping = json!({"type": "save_mapping", "mapping[kind]": "component", "mapping[code]": "ui-deduction", "mapping[direction]": "deduction", "mapping[expense_account_id]": "", "mapping[liability_account_id]": "", "mapping[reason]": ""});
    let html = event(setup.as_mut(), &manager, mapping.clone()).await?;
    assert!(html.contains("payroll-mapping-liability-account-error-0"));
    assert!(html.contains("is needed"));
    assert!(html.contains("value=\"ui-deduction\""));
    mapping["mapping[liability_account_id]"] = json!(settings.net_pay_account_id.to_string());
    assert!(event(setup.as_mut(), &manager, mapping.clone())
        .await?
        .contains("Mapping ui-deduction (deduction) saved."));
    let mapped = payroll
        .list_mappings(&manager, 1)
        .await?
        .into_iter()
        .find(|m| m.code == "ui-deduction")
        .unwrap();
    let before = payroll.list_mapping_changes(&manager, 1).await?.len();
    mapping["mapping[liability_account_id]"] =
        json!(settings.claims_payable_account_id.to_string());
    mapping["mapping[reason]"] = json!("界".repeat(501));
    assert!(event(setup.as_mut(), &manager, mapping.clone())
        .await?
        .contains("should be at most 500 character(s)"));
    assert_eq!(
        payroll.list_mapping_changes(&manager, 1).await?.len(),
        before
    );
    mapping["mapping[reason]"] = json!("");
    assert!(event(
        setup.as_mut(),
        &manager,
        json!({"type":"edit_mapping", "id":mapped.id.to_string()})
    )
    .await?
    .contains("value=\"ui-deduction\""));
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
    // A stale settings form rechecks a revoked capability and remains mounted.
    let mappings_before = payroll.list_mappings(&manager, 1).await?;
    let changes_before = payroll.list_mapping_changes(&manager, 1).await?;
    grants
        .set_direct(&admin, 1, MANAGE_SETTINGS, Some(false))
        .await?;
    assert!(event(setup.as_mut(), &manager, mapping)
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
    assert!(html.contains(&format!("#{}", settings.net_pay_account_id)));
    assert!(html.contains("id=\"payroll-mappings-table\""));
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::VIEW, None)
        .await?;

    // A malformed date may never silently become company-local today.
    event(
        postings.as_mut(),
        &manager,
        json!({"type":"reverse", "id":historical.to_string()}),
    )
    .await?;
    let draft = json!({"type":"confirm_reversal", "reversal[reason]":"UI historical correction", "reversal[entry_date]":"not-a-date"});
    let html = event(postings.as_mut(), &manager, draft.clone()).await?;
    assert!(html.contains("payroll-reversal-date-error-0"));
    assert!(!html.contains("id=\"payroll-reversal-dialog\""));
    let mut draft = draft;
    draft["reversal[entry_date]"] = json!("2027-02-04");
    for malformed in [json!(123), json!([]), json!({}), Value::Null] {
        // Even an existing pending request must be cleared by bad input.
        event(postings.as_mut(), &manager, draft.clone()).await?;
        let mut forged = draft.clone();
        forged["reversal[entry_date]"] = malformed;
        let html = event(postings.as_mut(), &manager, forged).await?;
        assert!(html.contains("payroll-reversal-date-error-0"));
        assert!(!html.contains("id=\"payroll-reversal-dialog\""));
        event(
            postings.as_mut(),
            &manager,
            json!({"type":"commit_reversal"}),
        )
        .await?;
        assert!(payroll
            .get_posting(&manager, 1, historical)
            .await?
            .reversal
            .is_none());
    }
    assert!(event(postings.as_mut(), &manager, draft.clone())
        .await?
        .contains("id=\"payroll-reversal-dialog\""));
    event(
        postings.as_mut(),
        &manager,
        json!({"type":"cancel_confirmation"}),
    )
    .await?;
    event(
        postings.as_mut(),
        &manager,
        json!({"type":"commit_reversal"}),
    )
    .await?;
    assert!(payroll
        .get_posting(&manager, 1, historical)
        .await?
        .reversal
        .is_none());
    event(postings.as_mut(), &manager, draft.clone()).await?;
    grants
        .set_direct(&admin, 1, POST_POSTINGS, Some(false))
        .await?;
    assert!(event(
        postings.as_mut(),
        &manager,
        json!({"type":"commit_reversal"})
    )
    .await?
    .contains("You do not have permission for that action."));
    assert!(payroll
        .get_posting(&manager, 1, historical)
        .await?
        .reversal
        .is_none());
    grants.set_direct(&admin, 1, POST_POSTINGS, None).await?;
    event(postings.as_mut(), &manager, draft).await?;
    assert!(event(
        postings.as_mut(),
        &manager,
        json!({"type":"commit_reversal", "id":"999999"})
    )
    .await?
    .contains("Reversed as entry"));
    let count = payroll.list_posting_events(&manager, 1).await?.len();
    event(
        postings.as_mut(),
        &manager,
        json!({"type":"commit_reversal"}),
    )
    .await?;
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
        "Payroll UI contracts passed: People absent/no writes; form-change read-only; Unicode reason; inline errors/drafts; mapping upsert; independent columns; Ledger-withheld IDs; revoked settings and reversal grants; invalid date; confirmation cancel/single commit; complete history past 500"
    );
    Ok(())
}
