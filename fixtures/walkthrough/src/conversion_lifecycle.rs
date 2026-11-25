//! Executable conversion lifecycle proof, composed with the actual owner services.
use bilibi_database::Db;
use bilibi_finance_fixed_assets::{DisposalInput, EventInput, FixedAssets};
use bilibi_finance_ledger::{Ledger, Money};
use bilibi_finance_payables::{AllocationInput, NewPayment, Payables};
use bilibi_finance_receivables::Receivables;

use bilibi_module_registry::Services;
use jiff::civil::date;
use serde_json::{json, Value};
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
pub async fn prove(
    db: &Db,
    services: &Services,
    config: &Value,
    batches: [Value; 3],
    run_id: i64,
) -> Outcome<()> {
    let company = 3;
    let preparer = crate::in_company(db, services, 1, company).await?;
    let approver = crate::in_company(db, services, 4, company).await?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let assets = services.get::<FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let day = date(2026, 8, 1);
    let units = ledger.get_book(&approver, company).await?.minor_units;
    let money = |text| Money::parse(text, units).expect("fixture money");
    let ar_before = ar
        .aged_receivables(&approver, company, day, &Default::default())
        .await?;
    let ap_before = ap
        .aged_payables(&approver, company, day, &Default::default())
        .await?;
    let invoice = ar_before.customers[0].documents[0].document_id;
    let bill = ap
        .list_documents(&approver, company, &Default::default())
        .await?
        .into_iter()
        .find(|d| d.document_type == "bill")
        .unwrap()
        .id;
    let asset = assets
        .register_report(&approver, company, day, &Default::default())
        .await?
        .rows[0]
        .asset
        .id;
    // 10,000 source rows (the register limit) exceed one direct import. Each
    // chunk is independently retained; none is settleable until reconciliation.
    let header="source_id,customer_code,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\n";
    let chunk = |start, end| {
        let mut csv = header.to_owned();
        for n in start..end {
            csv.push_str(&format!(
                "corrected-{n},company_010,CORRECTED-{n},2026-05-15,2026-06-30,0.30,0.30,1100,company,invoice,invoice,source-system,MYR,unknown\n"
            ));
        }
        csv
    };
    let common = json!({"source_reference":"AR-CORRECTED","currency_code":"MYR","series_id":config["invoice_series"],"replaces_batch_id":batches[0]["id"],"reason":"Replace incomplete source invoice detail","part_count":20,"opening_account_classifications":config["opening_account_classifications"]});
    let mut part1 = common.clone();
    part1["part"] = json!(1);
    part1["csv"] = json!(chunk(0, 500));
    assert_eq!(
        ar.stage_opening_import(&preparer, company, &part1).await?["state"],
        "committed"
    );
    assert_eq!(
        ar.stage_opening_import(&preparer, company, &part1).await?["already_staged"],
        true
    );
    let mut finalize_ar = common.clone();
    finalize_ar["staged"] = json!(true);
    finalize_ar["csv"] = json!("");
    assert!(ar
        .import_opening_invoices(&approver, company, &finalize_ar)
        .await
        .is_err());
    let mut changed = part1.clone();
    changed["csv"] = json!(chunk(0, 499));
    assert!(ar
        .stage_opening_import(&preparer, company, &changed)
        .await
        .is_err());
    changed = part1.clone();
    changed["part_count"] = json!(21);
    assert!(ar
        .stage_opening_import(&preparer, company, &changed)
        .await
        .is_err());
    for part in 2..=20 {
        let mut next = common.clone();
        next["part"] = json!(part);
        next["csv"] = json!(chunk((part - 1) * 500, part * 500));
        ar.stage_opening_import(&preparer, company, &next).await?;
    }
    assert!(ar
        .import_opening_invoices(&preparer, company, &finalize_ar)
        .await
        .is_err());

    let ap_header="source_id,vendor_code,vendor_name,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\n";
    let ap_chunk = |start, end| {
        let mut csv = ap_header.to_owned();
        for n in start..end {
            csv.push_str(&format!(
                "corrected-{n},company_010,Conversion vendor,CORRECTED-BILL-{n},2026-05-01,2026-06-15,0.20,0.15,2000,company,bill,bill,source-system,MYR,unknown\n"
            ));
        }
        csv
    };
    let ap_common = json!({"source_reference":"AP-CORRECTED","currency_code":"MYR","replaces_batch_id":batches[1]["id"],"reason":"Correct source bill original total","part_count":20,"opening_account_classifications":config["opening_account_classifications"]});
    let revision = config["revision"].as_i64().unwrap();
    let asset_header="source_id,description,category_revision_id,acquisition_date,in_service_date,cost,accumulated_depreciation,remaining_life_months,residual_value\n";
    let asset_chunk = |start, end| {
        let mut csv = asset_header.to_owned();
        for n in start..end {
            csv.push_str(&format!(
                "corrected-{n},Source register asset {n},{revision},2024-06-01,2024-07-01,1.20,0.18,102,0\n"
            ));
        }
        csv
    };
    let assets_common = json!({"source_reference":"ASSETS-CORRECTED","currency_code":"MYR","replaces_batch_id":batches[2]["id"],"reason":"Correct remaining prospective life","part_count":20});
    for part in 1..=20 {
        let mut bills = ap_common.clone();
        bills["part"] = json!(part);
        bills["csv"] = json!(ap_chunk((part - 1) * 500, part * 500));
        assert_eq!(
            ap.stage_opening_import(&preparer, company, &bills).await?["state"],
            "committed"
        );
        let mut register = assets_common.clone();
        register["part"] = json!(part);
        register["csv"] = json!(asset_chunk((part - 1) * 500, part * 500));
        assert_eq!(
            assets
                .stage_opening_import(&preparer, company, &register)
                .await?["state"],
            "committed"
        );
    }
    let mut part_ap = ap_common.clone();
    part_ap["staged"] = json!(true);
    part_ap["csv"] = json!("");
    let mut part_assets = assets_common.clone();
    part_assets["staged"] = json!(true);
    part_assets["csv"] = json!("");
    let mut bills_retry = ap_common.clone();
    bills_retry["part"] = json!(20);
    bills_retry["csv"] = json!(ap_chunk(9500, 10000));
    assert_eq!(
        ap.stage_opening_import(&preparer, company, &bills_retry)
            .await?["already_staged"],
        true
    );
    bills_retry["csv"] = json!(ap_chunk(9500, 9999));
    assert!(ap
        .stage_opening_import(&preparer, company, &bills_retry)
        .await
        .is_err());
    let mut register_retry = assets_common.clone();
    register_retry["part"] = json!(20);
    register_retry["csv"] = json!(asset_chunk(9500, 10000));
    assert_eq!(
        assets
            .stage_opening_import(&preparer, company, &register_retry)
            .await?["already_staged"],
        true
    );
    register_retry["csv"] = json!(asset_chunk(9500, 9999));
    assert!(assets
        .stage_opening_import(&preparer, company, &register_retry)
        .await
        .is_err());
    assert!(ap
        .import_opening_bills(&preparer, company, &part_ap)
        .await
        .is_err());
    assert!(assets
        .import_opening_register(&preparer, company, &part_assets)
        .await
        .is_err());

    // Corrections refuse standing dependent receipts and payments. Their own
    // audited reversal paths are the only way to release those dependencies.
    let receipt=ar.record_payment(&approver,company,&json!({"series_id":config["payment_series"],"customer_id":config["customer_id"],"payment_date":"2026-07-20","amount":"5","deposit_account_id":config["bank"],"allocations":[{"document_id":invoice,"amount":"5"}]})).await?;
    assert!(ar
        .import_opening_invoices(&approver, company, &finalize_ar)
        .await
        .is_err());
    ar.reverse_payment(
        &approver,
        company,
        receipt.id,
        &json!({"reversal_date":"2026-07-21","reason":"Release source correction"}),
    )
    .await?;
    let request = ap
        .submit_payment(
            &preparer,
            company,
            &NewPayment {
                payment_date: date(2026, 7, 20),
                vendor_id: config["vendor_id"].as_i64(),
                supplier_key: None,
                paid_from_account_id: config["bank"].as_i64().unwrap(),
                reference: None,
                description: None,
                allocations: vec![AllocationInput {
                    document_id: bill,
                    amount: money("5"),
                }],
            },
        )
        .await?;
    assert!(ap
        .import_opening_bills(&approver, company, &part_ap)
        .await
        .is_err());
    let paid = ap.approve_payment(&approver, company, request.id).await?;
    assert!(ap
        .import_opening_bills(&approver, company, &part_ap)
        .await
        .is_err());
    ap.reverse_payment(
        &approver,
        company,
        paid.id,
        date(2026, 7, 21),
        Some("Release source correction".into()),
    )
    .await?;
    assert!(assets
        .import_opening_register(&approver, company, &part_assets)
        .await
        .is_err());
    assets
        .dispose(
            &approver,
            company,
            asset,
            &DisposalInput {
                event: EventInput {
                    effective_date: date(2026, 7, 31),
                    reason: "Dependent disposal proof".into(),
                },
                proceeds: money("0"),
                settlement_account_id: None,
            },
        )
        .await?;
    assert!(assets
        .import_opening_register(&approver, company, &part_assets)
        .await
        .is_err());
    assets
        .reverse_disposal(
            &approver,
            company,
            asset,
            &EventInput {
                effective_date: day,
                reason: "Release source correction".into(),
            },
        )
        .await?;
    assets
        .reverse_run(
            &approver,
            company,
            run_id,
            &EventInput {
                effective_date: day,
                reason: "Release source correction".into(),
            },
        )
        .await?;
    let asset_before = assets
        .register_report(&approver, company, day, &Default::default())
        .await?;
    let before_entry = ledger
        .trial_balance(&approver, company, day, None)
        .await?
        .through_entry_number;

    // A late storage refusal after hundreds of inserted snapshots must roll
    // back the whole register, its recognition and all affected audit changes.
    db.get().await?.batch_execute("CREATE FUNCTION walkthrough_opening_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.description='Opening invoice CORRECTED-9999' THEN RAISE EXCEPTION 'disposable late correction fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_opening_fault BEFORE INSERT ON finance_receivables_documents FOR EACH ROW EXECUTE FUNCTION walkthrough_opening_fault();").await?;
    assert!(ar
        .import_opening_invoices(&approver, company, &finalize_ar)
        .await
        .is_err());
    db.get().await?.batch_execute("DROP TRIGGER walkthrough_opening_fault ON finance_receivables_documents; DROP FUNCTION walkthrough_opening_fault();").await?;
    assert_eq!(
        ar.aged_receivables(&approver, company, day, &Default::default())
            .await?
            .customers[0]
            .documents[0]
            .document_id,
        invoice
    );
    let competing = {
        let ar = ar.clone();
        let approver = approver.clone();
        let finalize_ar = finalize_ar.clone();
        tokio::spawn(async move {
            ar.import_opening_invoices(&approver, company, &finalize_ar)
                .await
        })
    };
    let left = ar
        .import_opening_invoices(&approver, company, &finalize_ar)
        .await;
    let right = competing.await?;
    let replaced_ar = left?;
    assert_eq!(replaced_ar, right?);
    let replaced_ap = ap
        .import_opening_bills(&approver, company, &part_ap)
        .await?;
    let replaced_assets = assets
        .import_opening_register(&approver, company, &part_assets)
        .await?;
    assert_ne!(replaced_ar["id"], batches[0]["id"]);
    assert_ne!(replaced_ap["id"], batches[1]["id"]);
    assert_ne!(replaced_assets["id"], batches[2]["id"]);
    assert_eq!(
        ledger
            .trial_balance(&approver, company, day, None)
            .await?
            .through_entry_number,
        before_entry
    );
    let ar_after = ar
        .aged_receivables(&approver, company, day, &Default::default())
        .await?;
    assert_eq!(ar_after.customers[0].documents.len(), 10_000);
    assert_eq!(ar_after.total, ar_before.total);
    let old_ar_pins = ar_before.pins();
    let old_ap_pins = ap_before.pins();
    // These pins preceded the extra payment/reversal proof too.
    let ar_old = ar
        .aged_receivables(
            &approver,
            company,
            day,
            &bilibi_finance_receivables::AgingOptions {
                through_entry_number: old_ar_pins.through_entry_number,
                through_opening_batch_id: old_ar_pins.through_opening_batch_id,
                through_settlement_number: old_ar_pins.through_settlement_number,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&ar_old)?,
        serde_json::to_value(&ar_before)?
    );
    let ap_old = ap
        .aged_payables(
            &approver,
            company,
            day,
            &bilibi_finance_payables::AgingQuery {
                through_entry_number: old_ap_pins.through_entry_number,
                through_opening_batch_id: old_ap_pins.through_opening_batch_id,
                through_settlement_number: old_ap_pins.through_settlement_number,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&ap_old)?,
        serde_json::to_value(&ap_before)?
    );
    let asset_old = assets
        .register_report(&approver, company, day, &asset_before.pins())
        .await?;
    assert_eq!(asset_old.rows[0].asset.id, asset);
    assert_eq!(asset_old.reconciliation, asset_before.reconciliation);
    assert_ne!(
        assets
            .register_report(&approver, company, day, &Default::default())
            .await?
            .rows[0]
            .asset
            .id,
        asset
    );
    assert!(
        ar.tie_out_control_accounts(&approver, company, day, &Default::default())
            .await?
            .tied_out
    );
    assert!(
        ap.tie_out_control_accounts(&approver, company, day, &Default::default())
            .await?
            .tied_out
    );
    assert!(ar.record_payment(&approver,company,&json!({"series_id":config["payment_series"],"customer_id":config["customer_id"],"payment_date":"2026-08-01","amount":"5","deposit_account_id":config["bank"],"allocations":[{"document_id":invoice,"amount":"5"}]})).await.is_err());
    // Old source exports survive and identical historical retries remain safe.
    assert_eq!(ar.import_opening_invoices(&approver,company,&json!({"source_reference":batches[0]["source_reference"],"currency_code":"MYR","series_id":config["invoice_series"],"csv":batches[0]["source_csv"],"opening_account_classifications":config["opening_account_classifications"]})).await?,batches[0]);
    let opening = ledger
        .opening_context_on(&**db.get().await?, &approver, company)
        .await?;
    assert!(ledger
        .reverse_entry(
            &approver,
            company,
            opening.entry.id,
            &bilibi_finance_ledger::Reversal {
                entry_date: day,
                description: "Refused after append-only replacement".into(),
                reference: None
            }
        )
        .await
        .is_err());
    for table in [
        "finance_receivables_opening_stages",
        "finance_receivables_opening_decisions",
        "finance_payables_opening_stages",
        "finance_payables_opening_decisions",
        "finance_fixed_assets_opening_stages",
        "finance_fixed_assets_opening_decisions",
        "finance_ledger_opening_recognition_replacements",
    ] {
        assert!(db
            .get()
            .await?
            .execute(
                &format!(
                    "UPDATE {table} SET company_id=company_id WHERE tenant_id=1 AND company_id=$1"
                ),
                &[&company]
            )
            .await
            .is_err());
    }
    let bills = ap
        .aged_payables(&approver, company, day, &Default::default())
        .await?;
    assert_eq!(bills.parties[0].documents.len(), 10_000);
    assert_eq!(bills.total, ap_before.total);
    let register = assets
        .register_report(&approver, company, day, &Default::default())
        .await?;
    assert_eq!(register.rows.len(), 10_000);
    let first_ids: std::collections::HashSet<i64> =
        register.rows.iter().map(|r| r.asset.id).collect();
    assert_eq!(first_ids.len(), 10_000);
    let second_asset_chunk = |start, end| {
        let mut csv = asset_header.to_owned();
        for n in start..end {
            csv.push_str(&format!(
                "second-{n},Second source register asset {n},{revision},2024-06-01,2024-07-01,1.20,0.18,51,0\n"
            ));
        }
        csv
    };
    let second_common = json!({"source_reference":"ASSETS-CORRECTED-AGAIN","currency_code":"MYR","replaces_batch_id":replaced_assets["id"],"reason":"Correct the corrected register again","part_count":20});
    for part in 1..=20 {
        let mut stage = second_common.clone();
        stage["part"] = json!(part);
        stage["csv"] = json!(second_asset_chunk((part - 1) * 500, part * 500));
        assert_eq!(
            assets
                .stage_opening_import(&preparer, company, &stage)
                .await?["state"],
            "committed"
        );
    }
    let mut finalize_again = second_common.clone();
    finalize_again["staged"] = json!(true);
    finalize_again["csv"] = json!("");
    assert!(assets
        .import_opening_register(&preparer, company, &finalize_again)
        .await
        .is_err());
    let replaced_again = assets
        .import_opening_register(&approver, company, &finalize_again)
        .await?;
    assert_ne!(replaced_again["id"], replaced_assets["id"]);
    assert_eq!(
        assets
            .import_opening_register(&approver, company, &finalize_again)
            .await?,
        replaced_again
    );
    let again = assets
        .register_report(&approver, company, day, &Default::default())
        .await?;
    assert_eq!(again.rows.len(), 10_000);
    assert!(again.rows.iter().all(|r| !first_ids.contains(&r.asset.id)));
    let first_old = assets
        .register_report(&approver, company, day, &register.pins())
        .await?;
    assert_eq!(
        first_old
            .rows
            .iter()
            .map(|r| r.asset.id)
            .collect::<std::collections::HashSet<_>>(),
        first_ids
    );
    assert_eq!(first_old.reconciliation, register.reconciliation);
    let original_old = assets
        .register_report(&approver, company, day, &asset_before.pins())
        .await?;
    assert_eq!(original_old.rows.len(), 2);
    assert_eq!(original_old.rows[0].asset.id, asset);
    assert_eq!(
        ledger
            .trial_balance(&approver, company, day, None)
            .await?
            .through_entry_number,
        before_entry
    );
    let run = assets
        .depreciate(&approver, company, date(2026, 8, 1))
        .await?;
    assert_eq!(run.total, money("400"));
    println!("Opening lifecycle: interrupted twenty-batch/10,000-invoice, bill and asset corrections resumed, then a second 10,000-asset replacement of the first; identical/concurrent retries, changed-source and self-approval refusals, late correction rollback, dependent receipts/payments/depreciation/disposal and owner reversals, accounting-date and original report boundaries, unchanged opening postings and prospective catch-up passed.");
    Ok(())
}
