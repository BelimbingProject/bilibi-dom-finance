//! Executable Payroll contracts, mirroring Bilimbi Finance.Payroll.
//! Historical records below prove correction independently of People,
//! not recognition of People facts, and introduce no production provider.
use crate::sign_in;
use bilibi_database::{Change, Db};
use bilibi_finance_ledger::{
    EntryOwner, Ledger, LedgerError, Money, NewEntry, NewFiscalYear, NewLine, PostOptions,
};
use bilibi_finance_payroll::{MappingChange, Payroll, PayrollError, Reversal, SettingsChange};
use bilibi_module_registry::Services;
use jiff::civil::date;
use serde_json::Value;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

#[macro_export]
macro_rules! walkthrough_payroll_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture.finance.payroll", $crate::registry::Provenance { module: "finance/payroll", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &[], verbs: &[], capabilities: &[], system_principals: &[],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &["finance.payroll.view", "finance.payroll.settings.manage", "finance.payroll.postings.post"] },
                $crate::authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &["finance.payroll.view"] },
                $crate::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &["finance.payroll.view"] },
            ],
        }) }
    };
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    println!("\n== Payroll facade contracts (historical correction fixture)");
    let payroll = services.get::<Payroll>(bilibi_finance_payroll::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let foreign = sign_in(db, services, 2, 5).await?;
    let contractor = sign_in(db, services, 1, 7).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let accounts = ledger.list_accounts(&manager, 1).await?;
    let account = |code: &str| accounts.iter().find(|a| a.code == code).unwrap().id;
    let (net, claims, expense, asset) = (
        account("2000"),
        account("2100"),
        account("5000"),
        account("1000"),
    );
    let settings = SettingsChange {
        net_pay_account_id: Some(net),
        claims_payable_account_id: Some(claims),
        reason: Some("Fixture configuration".into()),
    };
    assert!(matches!(
        payroll.put_settings(&viewer, 1, &settings).await,
        Err(PayrollError::Unauthorized)
    ));
    assert!(matches!(
        payroll.get_settings(&foreign, 1).await,
        Err(PayrollError::CompanyNotFound)
    ));
    assert!(payroll.get_settings(&contractor, 1).await.is_err());
    assert!(matches!(
        payroll.put_settings(&admin, 3, &settings).await,
        Err(PayrollError::CompanyArchived)
    ));
    assert!(matches!(
        payroll.put_settings(&staff, 1, &settings).await,
        Err(PayrollError::Unauthorized)
    ));
    assert!(payroll
        .put_settings(
            &manager,
            1,
            &SettingsChange {
                net_pay_account_id: Some(expense),
                ..settings.clone()
            }
        )
        .await
        .is_err());
    let saved = payroll.put_settings(&manager, 1, &settings).await?;
    assert_eq!(payroll.get_settings(&staff, 1).await?, saved);
    assert_eq!(
        (saved.net_pay_account_id, saved.claims_payable_account_id),
        (net, claims)
    );
    let n = payroll.list_settings_changes(&manager, 1).await?.len();
    payroll
        .put_settings(&manager, 1, &SettingsChange::default())
        .await?;
    assert_eq!(payroll.list_settings_changes(&manager, 1).await?.len(), n);
    // Leave exactly one slot: validation must use its write connection, not
    // wait forever for a second one while holding the company lock.
    let mut held = Vec::new();
    let status = db.status();
    let free = status.max_size - status.size + status.available;
    for _ in 1..free {
        held.push(db.get().await?);
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        payroll.put_settings(&manager, 1, &SettingsChange::default()),
    )
    .await??;
    drop(held);
    println!("configuration completes with all but one pool slot occupied");
    for (direction, exp, liab) in [
        ("earning", Some(expense), None),
        ("deduction", None, Some(claims)),
        ("employer", Some(asset), Some(net)),
        ("claim", Some(asset), None),
    ] {
        let change = MappingChange {
            kind: if direction == "claim" {
                "claim_category"
            } else {
                "component"
            }
            .into(),
            code: format!("fixture-{direction}"),
            direction: direction.into(),
            expense_account_id: exp,
            liability_account_id: liab,
            reason: None,
        };
        assert!(payroll.put_mapping(&viewer, 1, &change).await.is_err());
        let saved = payroll.put_mapping(&manager, 1, &change).await?;
        assert_eq!(
            (saved.expense_account_id, saved.liability_account_id),
            (exp, liab)
        );
        let n = payroll.list_mapping_changes(&manager, 1).await?.len();
        payroll.put_mapping(&manager, 1, &change).await?;
        assert_eq!(payroll.list_mapping_changes(&manager, 1).await?.len(), n);
        assert!(payroll
            .put_mapping(
                &manager,
                1,
                &MappingChange {
                    expense_account_id: Some(net),
                    ..change
                }
            )
            .await
            .is_err());
    }
    assert_eq!(payroll.list_mappings(&viewer, 1).await?.len(), 4);
    if !payroll.people_available() {
        let before = payroll.list_posting_events(&manager, 1).await?.len();
        let entries_before = ledger
            .list_entries(&manager, 1, &Default::default())
            .await?;
        assert!(matches!(
            payroll.list_payroll_runs(&manager, 1).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        assert!(matches!(
            payroll.list_claims(&manager, 1).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        assert!(matches!(
            payroll.post_payroll_run(&manager, 1, 101).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        assert!(matches!(
            payroll.post_claim(&manager, 1, 102).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        assert!(matches!(
            payroll.post_claims(&manager, 1, &[102, 103]).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        assert!(matches!(
            payroll.post_claim(&viewer, 1, 102).await,
            Err(PayrollError::Unauthorized)
        ));
        assert_eq!(
            payroll.list_posting_events(&manager, 1).await?.len(),
            before
        );
        assert_eq!(
            ledger
                .list_entries(&manager, 1, &Default::default())
                .await?,
            entries_before
        );
        println!(
            "People source: unavailable; run, claim and batch refused without entries or attempts"
        );
    } else {
        assert!(payroll.list_payroll_runs(&manager, 1).await?.is_empty());
        assert!(payroll.list_claims(&manager, 1).await?.is_empty());
        assert!(matches!(
            payroll.post_payroll_run(&manager, 1, 101).await,
            Err(PayrollError::SourceNotFound)
        ));
        println!("People source: available; no approval events have arrived in this fresh fixture");
    }

    // Use a fresh year, independent of periods closed by preceding scenarios.
    ledger
        .open_fiscal_year(
            &manager,
            1,
            &NewFiscalYear {
                start_date: Some(date(2027, 1, 1)),
                end_date: None,
            },
        )
        .await?;
    let units = ledger.get_book(&manager, 1).await?.minor_units;
    let new = NewEntry {
        entry_date: date(2027, 2, 2),
        description: "Historical payroll fixture".into(),
        reference: None,
        lines: vec![
            NewLine::debit(expense, Money::parse("137.29", units)?),
            NewLine::credit(net, Money::parse("137.29", units)?),
        ],
    };
    let mut client = db.get().await?;
    let mut tx = ledger
        .write(
            &mut client,
            &manager,
            1,
            &[bilibi_finance_payroll::POST_POSTINGS],
            "fixture.payroll.rollback",
        )
        .await?;
    let rolled = ledger
        .post_entry_in(&mut tx, &manager, 1, &new, &PostOptions::default())
        .await?;
    tx.rollback().await?;
    assert!(matches!(
        ledger.get_entry(&manager, 1, rolled.id).await,
        Err(LedgerError::EntryNotFound)
    ));
    // Owning a view-authorized boundary does not grant ledger posting.
    let mut tx = ledger
        .write(
            &mut client,
            &viewer,
            1,
            &[bilibi_finance_payroll::VIEW],
            "fixture.payroll.denied",
        )
        .await?;
    assert!(matches!(
        ledger
            .post_entry_in(&mut tx, &viewer, 1, &new, &PostOptions::default())
            .await,
        Err(LedgerError::Unauthorized)
    ));
    tx.rollback().await?;
    let mut ids = Vec::new();
    // The third exercises page confirmation; the fourth stays available for
    // the browser walkthrough. Neither is a recognition of People facts.
    for source in [9001_i64, 9002, 9003, 9004] {
        let mut tx = ledger
            .write(
                &mut client,
                &manager,
                1,
                &[bilibi_finance_payroll::POST_POSTINGS],
                "fixture.payroll.historical",
            )
            .await?;
        let entry = ledger
            .post_entry_in(
                &mut tx,
                &manager,
                1,
                &new,
                &PostOptions {
                    owner: Some(EntryOwner {
                        module: "finance/payroll".into(),
                        record: format!("payroll_run:{source}"),
                    }),
                    ..PostOptions::default()
                },
            )
            .await?;
        let entry_id = entry.id;
        let number = entry.entry_number;
        let id = tx.mutate(move |sql| Box::pin(async move {
            let value: Value = sql.query_one("INSERT INTO finance_payroll_posting_events AS e(tenant_id,company_id,source_kind,source_id,source_reference,event,entry_id,entry_number,entry_date,total,currency_code,actor_user_id,occurred_at) VALUES(1,1,'payroll_run',$1,'Historical fixture','posted',$2,$3,'2027-02-02',137.29,'MYR',1,now()) RETURNING to_jsonb(e)", &[&source,&entry_id,&number]).await?.get(0);
            let id = value["id"].as_i64().unwrap();
            Ok((id, vec![Change { kind: "finance.payroll.posting_event".into(), id, old: Value::Null, new: value }]))
        })).await?;
        tx.commit().await?;
        ids.push(id);
    }
    let original = payroll.get_posting(&manager, 1, ids[0]).await?;
    assert!(matches!(
        payroll.get_posting(&foreign, 1, ids[0]).await,
        Err(PayrollError::CompanyNotFound)
    ));
    assert!(payroll.get_posting(&manager, 2, ids[0]).await.is_err());
    assert!(matches!(
        payroll
            .reverse_posting(&viewer, 1, ids[0], &Reversal::default())
            .await,
        Err(PayrollError::Unauthorized)
    ));
    assert!(payroll
        .reverse_posting(&manager, 1, ids[0], &Reversal::default())
        .await
        .is_err());
    assert!(payroll
        .reverse_posting(
            &manager,
            1,
            ids[0],
            &Reversal {
                reason: "Too early".into(),
                entry_date: Some(date(2027, 2, 1))
            }
        )
        .await
        .is_err());
    assert!(payroll
        .get_posting(&manager, 1, ids[0])
        .await?
        .last_failure
        .is_some());
    let reversal = Reversal {
        reason: "Historical correction".into(),
        entry_date: Some(date(2027, 2, 3)),
    };
    let grants = services.get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::VIEW, Some(false))
        .await?;
    assert!(matches!(
        ledger.get_book(&manager, 1).await,
        Err(LedgerError::Unauthorized)
    ));
    let corrected = payroll
        .reverse_posting(&manager, 1, ids[0], &reversal)
        .await?;
    grants
        .set_direct(&admin, 1, bilibi_finance_ledger::VIEW, None)
        .await?;
    println!("historical reversal succeeds with ledger posting but explicitly denied ledger view");
    assert!(corrected.last_failure.is_none());
    let mirror = ledger
        .get_entry(&manager, 1, corrected.reversal.unwrap().entry_id)
        .await?;
    let original_entry = ledger.get_entry(&manager, 1, original.entry_id).await?;
    assert_eq!(mirror.entry_date, date(2027, 2, 3));
    for (a, b) in original_entry.lines.iter().zip(&mirror.lines) {
        assert_eq!(
            (a.account_id, a.debit, a.credit),
            (b.account_id, b.credit, b.debit)
        );
    }
    assert!(matches!(
        payroll
            .reverse_posting(&manager, 1, ids[0], &reversal)
            .await,
        Err(PayrollError::AlreadyReversed)
    ));
    let (a, b) = tokio::join!(
        payroll.reverse_posting(&manager, 1, ids[1], &reversal),
        payroll.reverse_posting(&manager, 1, ids[1], &reversal)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(
        matches!(a, Err(PayrollError::AlreadyReversed))
            || matches!(b, Err(PayrollError::AlreadyReversed))
    );
    assert!(payroll
        .list_posting_events(&manager, 1)
        .await?
        .iter()
        .any(|r| r["event"] == "reversal_failed"));
    assert_eq!(payroll.list_postings(&viewer, 1).await?.len(), 4);
    for dataset in Payroll::evidence_datasets() {
        for (limit, after) in [(-1, 0), (0, 0), (501, 0), (1, -1)] {
            assert!(matches!(
                payroll
                    .evidence_page(&manager, 1, dataset, Some(limit), Some(after))
                    .await,
                Err(PayrollError::InvalidOption)
            ));
        }
        let all = payroll
            .evidence_page(&viewer, 1, dataset, Some(500), None)
            .await?;
        assert_eq!(
            all.rows,
            payroll
                .evidence_page(&viewer, 1, dataset, None, None)
                .await?
                .rows
        );
        let mut after = None;
        let mut rows = Vec::new();
        loop {
            let page = payroll
                .evidence_page(&viewer, 1, dataset, Some(1), after)
                .await?;
            rows.extend(page.rows);
            match page.next {
                Some(next) => {
                    assert!(next > after.unwrap_or(0));
                    after = Some(next);
                }
                None => break,
            }
        }
        assert_eq!(rows, all.rows);
        assert!(payroll
            .evidence_page(&foreign, 1, dataset, None, None)
            .await
            .is_err());
    }
    assert!(matches!(
        payroll
            .evidence_page(&manager, 1, "not-a-dataset", None, None)
            .await,
        Err(PayrollError::InvalidOption)
    ));
    for (table, kind) in [
        ("finance_payroll_settings", "settings"),
        ("finance_payroll_mappings", "mapping"),
        ("finance_payroll_settings_changes", "settings_change"),
        ("finance_payroll_mapping_changes", "mapping_change"),
        ("finance_payroll_posting_events", "posting_event"),
    ] {
        let missing: i64 = client.query_one(&format!("SELECT count(*) FROM {table} r WHERE tenant_id=1 AND company_id=1 AND NOT EXISTS(SELECT 1 FROM base_audit_mutations a WHERE a.auditable_type=$1 AND a.auditable_id=r.id::text)"), &[&format!("finance.payroll.{kind}")]).await?.get(0);
        assert_eq!(missing, 0, "unaudited {table}");
        for sql in [
            format!("DELETE FROM {table} WHERE tenant_id=1 AND company_id=1"),
            format!("TRUNCATE {table} CASCADE"),
            if matches!(kind, "settings" | "mapping") {
                format!("UPDATE {table} SET company_id=2 WHERE tenant_id=1 AND company_id=1")
            } else {
                format!("UPDATE {table} SET id=id WHERE tenant_id=1 AND company_id=1")
            },
        ] {
            client.batch_execute("BEGIN").await?;
            let refused = client.batch_execute(&sql).await.is_err();
            client.batch_execute("ROLLBACK").await?;
            assert!(refused, "accepted {sql}");
        }
    }
    for sql in [
        "UPDATE finance_payroll_mappings SET code='changed' WHERE tenant_id=1 AND company_id=1".to_owned(),
        "UPDATE finance_payroll_settings SET tenant_id=2 WHERE tenant_id=1 AND company_id=1".to_owned(),
        // Copying a success must hit the source/entry uniqueness guard.
        "INSERT INTO finance_payroll_posting_events(tenant_id,company_id,source_kind,source_id,source_reference,event,entry_id,entry_number,entry_date,total,currency_code,reason,actor_user_id,occurred_at) SELECT tenant_id,company_id,source_kind,source_id,source_reference,event,entry_id,entry_number,entry_date,total,currency_code,reason,actor_user_id,occurred_at FROM finance_payroll_posting_events WHERE event='posted' LIMIT 1".to_owned(),
        "INSERT INTO finance_payroll_posting_events(tenant_id,company_id,source_kind,source_id,source_reference,event,actor_user_id,occurred_at) VALUES(1,1,'claim',9999,'Malformed','posted',1,now())".to_owned(),
        "INSERT INTO finance_payroll_posting_events(tenant_id,company_id,source_kind,source_id,source_reference,event,reason,actor_user_id,occurred_at) VALUES(1,1,'claim',9999,'Unposted','reversal_failed','No original',1,now())".to_owned(),
    ] {
        client.batch_execute("BEGIN").await?;
        let refused = client.batch_execute(&sql).await.is_err();
        client.batch_execute("ROLLBACK").await?;
        assert!(refused, "accepted {sql}");
    }
    println!(
        "Payroll contracts passed: configuration/no-op, isolation, unavailable/no events, owner rollback, independent capability, historical reversal/race/mirror, five evidence datasets, audit and retention guards"
    );
    crate::payroll_ui::run(db, services, ids[2]).await?;
    Ok(())
}
