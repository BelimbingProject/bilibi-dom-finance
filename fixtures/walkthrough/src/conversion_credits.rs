//! Source-backed credits and the independent settlement boundary on a composed,
//! disposable database. Exercise the owners' answers, not implementation text.
use super::*;
use bilibi_finance_ledger::{NewEntry, NewLine, PostOptions};
use bilibi_finance_payables::{AgingQuery, BalancePins as PayablePins};
use bilibi_finance_receivables::BalancePins as ReceivablePins;
pub(super) const SOURCE:&str="source_id,account_code,debit,credit\ncash,1000,1000,\nreceivables,1100,70,\nreceivables-credit,1110,,30\npayables,2000,,70\npayables-credit,2010,30,\ncapital,3000,,1000\n";
pub(super) const CREDIT_ONLY_SOURCE: &str =
    "source_id,account_code,debit,credit\ncustomer-credit,1100,,30\nsupplier-credit,2000,30,\n";
fn tax(value: Value) -> String {
    format!("\"{}\"", value.to_string().replace('"', "\"\""))
}
pub(super) fn csv(payable: bool, only: bool) -> String {
    let header = if payable { AP } else { AR }.lines().next().unwrap();
    let normal = if payable { "bill" } else { "invoice" };
    let credit = if payable {
        "supplier_credit"
    } else {
        "customer_credit"
    };
    let party = if payable {
        "company_010,Conversion vendor"
    } else {
        "company_010"
    };
    let (primary, historical, zero) = if payable {
        ("2000", "2010", "2020")
    } else {
        ("1100", "1110", "1120")
    };
    let mut out = format!("{header}\n");
    let mut row = |source: &str,
                   reference: &str,
                   due: &str,
                   gross: &str,
                   residual: &str,
                   control: &str,
                   kind: &str,
                   source_kind: &str,
                   facts: Value| {
        let party = if source == "zero-invoice" {
            if payable {
                "company_011,Other source supplier"
            } else {
                "company_011"
            }
        } else {
            party
        };
        let document_date = if source == "credit" {
            "2026-01-01"
        } else {
            "2026-05-01"
        };
        out.push_str(&format!("{source},{party},{reference},{document_date},{due},{gross},{residual},{control},company,{kind},{source_kind},source-system,MYR,{}\n",tax(facts)));
    };
    if !only {
        row(
            "invoice",
            "ORIGINAL-INVOICE",
            "2026-06-01",
            "106",
            "100",
            primary,
            normal,
            normal,
            json!({"status":"supplied","net":"100","tax_total":"6","lines":[{"code":"source-tax","rate":"6","taxable_amount":"100","net":"100","tax_amount":"6","gross":"106"}]}),
        );
    }
    row(
        "credit",
        "ORIGINAL-CREDIT",
        "2026-02-01",
        "53",
        "30",
        primary,
        credit,
        "credit_note",
        json!({"status":"supplied","net":"50","tax_total":"3","lines":[{"code":"source-tax","rate":"6","taxable_amount":"50","net":"50","tax_amount":"3","gross":"53"}]}),
    );
    if !only {
        row(
            "historical-credit",
            "HISTORICAL-CREDIT",
            "",
            "30",
            "30",
            historical,
            credit,
            if payable { "payment" } else { "receipt" },
            json!({"status":"unknown"}),
        );
        row(
            "zero-invoice",
            "ZERO-INVOICE",
            "2026-05-15",
            "20",
            "20",
            zero,
            normal,
            normal,
            json!({"status":"unknown"}),
        );
        row(
            "zero-credit",
            "ZERO-CREDIT",
            "",
            "20",
            "20",
            zero,
            credit,
            "credit_note",
            json!({"status":"supplied","net":"20","tax_total":"0"}),
        );
    }
    out
}
fn ar_evidence_rows(page: &bilibi_finance_receivables::EvidencePage) -> Vec<Value> {
    page.rows
        .iter()
        .map(|row| {
            Value::Object(
                page.fields
                    .iter()
                    .zip(row)
                    .map(|(key, value)| ((*key).to_owned(), value.clone()))
                    .collect(),
            )
        })
        .collect()
}
async fn tied(
    ar: &Receivables,
    ap: &Payables,
    s: &bilibi_tenancy::Scope,
    company: i64,
    day: jiff::civil::Date,
) -> Outcome<()> {
    let a = ar
        .tie_out_control_accounts(s, company, day, &ReceivablePins::default())
        .await?;
    let b = ap
        .tie_out_control_accounts(s, company, day, &PayablePins::default())
        .await?;
    assert!(a.tied_out, "receivables: {}", serde_json::to_string(&a)?);
    assert!(b.tied_out, "payables: {}", serde_json::to_string(&b)?);
    Ok(())
}
pub(super) async fn prove(db: &Db, services: &Services) -> Outcome<()> {
    let company = 6;
    let config = setup(db, services, company).await?;
    let reviewer = sign_in(db, services, 1, 4).await?;
    let manager = sign_in(db, services, 1, 1).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    for (code, kind) in [
        ("1190", AccountType::Asset),
        ("2190", AccountType::Liability),
    ] {
        let group = bilibi_finance_ledger::NewAccountGroup::new(code, "Source control group", kind);
        ledger
            .create_account_group(&reviewer, company, &group)
            .await?;
    }
    let chart = ledger.list_accounts(&reviewer, company).await?;
    let account = |code: &str| chart.iter().find(|a| a.code == code).unwrap().id;
    // A zero historical control can already be inactive at conversion.
    for code in ["1120", "2020"] {
        ledger
            .update_account(
                &reviewer,
                company,
                account(code),
                &bilibi_finance_ledger::AccountChange {
                    active: Some(false),
                    ..Default::default()
                },
            )
            .await?;
    }
    let before = ledger
        .trial_balance(&reviewer, company, date(2026, 7, 1), None)
        .await?;
    let ar_csv = csv(false, false);
    let ap_csv = csv(true, false);
    let ar_input = json!({"source_reference":"CREDIT-AR","currency_code":"MYR","series_id":config["invoice_series"],"csv":ar_csv,"opening_account_classifications":config["opening_account_classifications"]});
    let ap_input = json!({"source_reference":"CREDIT-AP","currency_code":"MYR","csv":ap_csv,"opening_account_classifications":config["opening_account_classifications"]});
    for input in [&ar_input, &ap_input] {
        for change in [
            "precision",
            "currency",
            "duplicate",
            "identity",
            "group",
            "mapping",
            "mismatch",
            "omitted",
            "unused_zero",
            "tax_breakdown",
            "tax_breakdown_without_aggregates",
            "tax_rate",
        ] {
            let mut wrong = input.clone();
            if change == "omitted" {
                wrong["opening_account_classifications"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&account("1110").to_string());
            } else if change == "unused_zero" {
                let (code, kind) = if input == &ar_input {
                    ("1200", "receivables_control")
                } else {
                    ("2100", "payables_control")
                };
                wrong["opening_account_classifications"][account(code).to_string()] = json!(kind);
            } else {
                let source = wrong["csv"].as_str().unwrap();
                let supplied_tax = tax(
                    json!({"status":"supplied","net":"100","tax_total":"6","lines":[{"code":"source-tax","rate":"6","taxable_amount":"100","net":"100","tax_amount":"6","gross":"106"}]}),
                );
                wrong["csv"] = json!(match change {
                    "precision" => source.replace(",106,100,", ",106,100.001,"),
                    "currency" => source.replace(",MYR,", ",USD,"),
                    "duplicate" => format!("{}{}\n", source, source.lines().nth(1).unwrap()),
                    "identity" => {
                        let credit = source
                            .lines()
                            .find(|line| line.starts_with("credit,"))
                            .unwrap();
                        format!(
                            "{}{}\n",
                            source.replacen(",53,30,", ",53,15,", 1),
                            credit
                                .replacen("credit,", "duplicate-credit,", 1)
                                .replacen(",53,30,", ",53,15,", 1)
                        )
                    }
                    "group" => source
                        .replace(",1100,", ",1190,")
                        .replace(",2000,", ",2190,"),
                    "mapping" => source
                        .replace(",1120,", ",1000,")
                        .replace(",2020,", ",1000,"),
                    "tax_breakdown" => source.replacen(
                        &supplied_tax,
                        &tax(json!({"status":"supplied","net":"100","tax_total":"6","lines":[{"code":"source-tax","rate":"6","taxable_amount":"100","net":"194","tax_amount":"6","gross":"200"}]})),
                        1,
                    ),
                    "tax_breakdown_without_aggregates" => source.replacen(
                        &supplied_tax,
                        &tax(json!({"status":"supplied","lines":[{"code":"source-tax","rate":"6","taxable_amount":"100","net":"194","tax_amount":"6"}]})),
                        1,
                    ),
                    "tax_rate" => source.replacen(
                        &supplied_tax,
                        &tax(json!({"status":"supplied","net":"100","tax_total":"6","lines":[{"code":"source-tax","rate":"7","taxable_amount":"100","net":"100","tax_amount":"6","gross":"106"}]})),
                        1,
                    ),
                    _ => source.replace(",106,100,", ",106,99,"),
                });
            }
            if change == "group" {
                let (code, kind) = if input == &ar_input {
                    ("1190", "receivables_control")
                } else {
                    ("2190", "payables_control")
                };
                wrong["opening_account_classifications"][account(code).to_string()] = json!(kind);
            }
            if input == &ar_input {
                let Err(error) = ar.import_opening_invoices(&reviewer, company, &wrong).await
                else {
                    panic!("AR accepted {change}");
                };
                if matches!(
                    change,
                    "tax_breakdown" | "tax_breakdown_without_aggregates" | "tax_rate"
                ) {
                    let fields = match &error {
                        bilibi_finance_receivables::ReceivableError::Invalid(fields) => fields,
                        _ => panic!("AR {change} returned the wrong refusal: {error:?}"),
                    };
                    assert!(fields
                        .iter()
                        .any(|field| field.message.contains("source tax line 1")));
                }
            } else {
                let Err(error) = ap.import_opening_bills(&reviewer, company, &wrong).await else {
                    panic!("AP accepted {change}");
                };
                if matches!(
                    change,
                    "tax_breakdown" | "tax_breakdown_without_aggregates" | "tax_rate"
                ) {
                    let fields = match &error {
                        bilibi_finance_payables::PayablesError::Invalid(fields) => fields,
                        _ => panic!("AP {change} returned the wrong refusal: {error:?}"),
                    };
                    assert!(fields
                        .iter()
                        .any(|field| field.message.contains("source tax line 1")));
                }
            }
        }
    }
    // Preserve each staging preparer, require independent complete-register review.
    let mut stage_ar = ar_input.clone();
    stage_ar["part"] = json!(1);
    stage_ar["part_count"] = json!(1);
    let mut stage_ap = ap_input.clone();
    stage_ap["part"] = json!(1);
    stage_ap["part_count"] = json!(1);
    ar.stage_opening_import(&manager, company, &stage_ar)
        .await?;
    ap.stage_opening_import(&manager, company, &stage_ap)
        .await?;
    let mut staged_ar = ar_input.clone();
    staged_ar["csv"] = json!("");
    staged_ar["staged"] = json!(true);
    let mut staged_ap = ap_input.clone();
    staged_ap["csv"] = json!("");
    staged_ap["staged"] = json!(true);
    assert!(ar
        .import_opening_invoices(&manager, company, &staged_ar)
        .await
        .is_err());
    assert!(ap
        .import_opening_bills(&manager, company, &staged_ap)
        .await
        .is_err());
    let mut duplicate_ar = stage_ar.clone();
    duplicate_ar["source_reference"] = json!("DUPLICATE-STAGED");
    duplicate_ar["part_count"] = json!(2);
    duplicate_ar["csv"] = json!(format!(
        "{}\n{}\n",
        ar_csv.lines().next().unwrap(),
        ar_csv.lines().nth(1).unwrap()
    ));
    ar.stage_opening_import(&manager, company, &duplicate_ar)
        .await?;
    duplicate_ar["part"] = json!(2);
    ar.stage_opening_import(&reviewer, company, &duplicate_ar)
        .await?;
    duplicate_ar["staged"] = json!(true);
    duplicate_ar["csv"] = json!("");
    assert!(ar
        .import_opening_invoices(&reviewer, company, &duplicate_ar)
        .await
        .is_err());
    let mut duplicate_ap = stage_ap.clone();
    duplicate_ap["source_reference"] = json!("DUPLICATE-STAGED");
    duplicate_ap["part_count"] = json!(2);
    duplicate_ap["csv"] = json!(format!(
        "{}\n{}\n",
        ap_csv.lines().next().unwrap(),
        ap_csv.lines().nth(1).unwrap()
    ));
    ap.stage_opening_import(&manager, company, &duplicate_ap)
        .await?;
    duplicate_ap["part"] = json!(2);
    ap.stage_opening_import(&reviewer, company, &duplicate_ap)
        .await?;
    duplicate_ap["staged"] = json!(true);
    duplicate_ap["csv"] = json!("");
    assert!(ap
        .import_opening_bills(&reviewer, company, &duplicate_ap)
        .await
        .is_err());
    let a = ar
        .import_opening_invoices(&reviewer, company, &staged_ar)
        .await?;
    let b = ap
        .import_opening_bills(&reviewer, company, &staged_ap)
        .await?;
    assert_eq!(
        ar.import_opening_invoices(&reviewer, company, &staged_ar)
            .await?["id"],
        a["id"]
    );
    assert_eq!(
        ap.import_opening_bills(&reviewer, company, &staged_ap)
            .await?["id"],
        b["id"]
    );
    let mut changed = ar_input.clone();
    changed["csv"] = json!(ar_csv.replace("source-system", "changed-system"));
    assert!(ar
        .import_opening_invoices(&reviewer, company, &changed)
        .await
        .is_err());
    assert_eq!(
        before,
        ledger
            .trial_balance(&reviewer, company, date(2026, 7, 1), None)
            .await?
    );
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 1)).await?;
    let invoices = ar
        .list_documents(&reviewer, company, &Default::default())
        .await?;
    let bills = ap
        .list_documents(&reviewer, company, &Default::default())
        .await?;
    let ar_id = |source: &str| {
        invoices
            .iter()
            .find(|d| d.opening.as_ref().is_some_and(|s| s["source_id"] == source))
            .unwrap()
            .id
    };
    let ap_id = |source: &str| {
        bills
            .iter()
            .find(|d| d.opening.as_ref().is_some_and(|s| s["source_id"] == source))
            .unwrap()
            .id
    };
    assert_eq!(
        invoices
            .iter()
            .find(|d| d.id == ar_id("credit"))
            .unwrap()
            .kind,
        "opening_credit"
    );
    let old_ar = ar
        .aged_receivables(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    let old_ap = ap
        .aged_payables(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    assert!(ar
        .aged_receivables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingOptions {
                through_entry_number: Some(old_ar.through_entry_number),
                through_opening_batch_id: Some(old_ar.through_opening_batch_id),
                ..Default::default()
            },
        )
        .await
        .is_err());
    assert!(ap
        .aged_payables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingQuery {
                through_entry_number: Some(old_ap.through_entry_number),
                through_opening_batch_id: Some(old_ap.through_opening_batch_id),
                ..Default::default()
            },
        )
        .await
        .is_err());
    let reports =
        services.get::<bilibi_finance_reports::Reports>(bilibi_finance_reports::SERVICE)?;
    assert!(reports
        .conversion_report(
            &reviewer,
            company,
            date(2026, 7, 31),
            &bilibi_finance_reports::ConversionPins {
                through_entry_number: Some(old_ar.through_entry_number),
                ..Default::default()
            },
        )
        .await
        .is_err());
    let write_off = ar
        .submit_write_off(
            &manager,
            company,
            &json!({
                "document_id":ar_id("invoice"),
                "entry_date":"2026-07-09",
                "amount":"1",
                "offset_account_id":config["expense"],
                "reason":"Backdated proposal after report capture",
                "evidence":"Disposable replay regression"
            }),
        )
        .await?;
    ar.reject_write_off(
        &reviewer,
        company,
        &json!({"write_off_id":write_off.id,"reason":"Do not post the replay regression"}),
    )
    .await?;
    let conversion = reports
        .conversion_report(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    let conversion_pins: bilibi_finance_reports::ConversionPins =
        serde_json::from_value(conversion["pins"].clone())?;
    assert_eq!(
        conversion_pins.receivables_settlement_number,
        Some(old_ar.through_settlement_number)
    );
    assert_eq!(
        conversion_pins.payables_settlement_number,
        Some(old_ap.through_settlement_number)
    );
    let artifact = reports
        .export_auditor_pack(&reviewer, company, date(2026, 7, 31))
        .await?;
    let pack = reports
        .read_auditor_pack(&reviewer, company, &artifact.id)
        .await?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(pack.bytes))?;
    let manifest: Value = serde_json::from_reader(archive.by_name("manifest.json")?)?;
    assert_eq!(
        manifest["balance_pins"]["receivables_settlement_number"],
        old_ar.through_settlement_number
    );
    assert_eq!(
        manifest["balance_pins"]["payables_settlement_number"],
        old_ap.through_settlement_number
    );
    for name in ["aged-receivables.csv", "aged-payables.csv"] {
        use std::io::Read;
        let mut content = String::new();
        archive.by_name(name)?.read_to_string(&mut content)?;
        let mut lines = content.lines();
        let header = lines.next().unwrap();
        for field in [
            "through_opening_batch_id",
            "through_settlement_number",
            "balance_side",
            "control_account_id",
        ] {
            assert!(header.contains(field));
        }
        let rows: Vec<_> = lines.collect();
        assert_eq!(rows.len(), 5);
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains("opening_credit"))
                .count(),
            3
        );
    }
    assert_eq!(old_ar.gross_debits.to_string(), "120.0000");
    assert_eq!(old_ar.customers[0].credits.len(), 3);
    assert_eq!(old_ap.parties[0].credits.len(), 3);
    let ar_credit = old_ar
        .customers
        .iter()
        .flat_map(|customer| &customer.credits)
        .find(|credit| credit.document_id == ar_id("credit"))
        .unwrap();
    assert_eq!(ar_credit.issue_date, "2026-01-01");
    assert_eq!(ar_credit.due_date, "2026-02-01");
    assert_eq!(ar_credit.days_overdue, 211);
    let ap_credit = old_ap
        .parties
        .iter()
        .flat_map(|party| &party.credits)
        .find(|credit| credit.id == ap_id("credit"))
        .unwrap();
    assert_eq!(ap_credit.document_date, date(2026, 1, 1));
    assert_eq!(ap_credit.due_date, date(2026, 2, 1));
    assert_eq!(ap_credit.days_overdue, 211);
    assert!(ar
        .aged_receivables(&reviewer, company, date(2026, 6, 30), &Default::default())
        .await?
        .customers
        .is_empty());
    assert!(ap
        .aged_payables(&reviewer, company, date(2026, 6, 30), &Default::default())
        .await?
        .parties
        .is_empty());
    {
        let (documents, primary, zero) = (&invoices, "credit", "zero-credit");
        let source = documents
            .iter()
            .find(|d| {
                d.opening
                    .as_ref()
                    .is_some_and(|s| s["source_id"] == primary)
            })
            .unwrap()
            .opening
            .as_ref()
            .unwrap();
        assert_eq!(source["original_total"], "53");
        assert_eq!(source["source_tax_facts"]["tax_total"], "3");
        assert!(documents
            .iter()
            .find(|d| d
                .opening
                .as_ref()
                .is_some_and(|s| s["source_id"] == primary))
            .unwrap()
            .description
            .as_deref()
            .is_some_and(|description| description.starts_with("Opening customer credit ")));
        assert_eq!(
            documents
                .iter()
                .find(|d| d.opening.as_ref().is_some_and(|s| s["source_id"] == zero))
                .unwrap()
                .opening
                .as_ref()
                .unwrap()["source_tax_facts"]["tax_total"],
            "0"
        );
    }
    assert!(bills
        .iter()
        .find(|d| {
            d.opening
                .as_ref()
                .is_some_and(|s| s["source_id"] == "credit")
        })
        .unwrap()
        .label()
        .starts_with("Supplier credit "));
    assert!(ar
        .tax_totals(&reviewer, company, &Default::default())
        .await?
        .is_empty());
    assert!(ap
        .tax_totals(&reviewer, company, &Default::default())
        .await?
        .is_empty());
    assert!(ar
        .list_issued_documents(&reviewer, company, None, None)
        .await?
        .is_empty());
    assert!(ar_evidence_rows(
        &ar.evidence_page(&reviewer, company, "opening_stage", None, None)
            .await?
    )
    .iter()
    .any(|r| r["payload"]["csv"] == ar_csv));
    assert!(ap
        .evidence_page(&reviewer, company, "opening_stage", None, None)
        .await?
        .rows
        .iter()
        .any(|r| r["payload"]["csv"] == ap_csv));
    assert!(ledger
        .get_entry(
            &reviewer,
            company,
            ledger
                .opening_context_on(&**db.get().await?, &reviewer, company)
                .await?
                .entry
                .id
        )
        .await?
        .owner
        .is_none());
    assert!(ar.apply_credit(&manager,company,&json!({"credit_document_id":ar_id("zero-credit"),"target_document_id":ar_id("zero-invoice"),"effective_date":"2026-07-10","amount":"1","reference":"wrong-party"})).await.is_err());
    assert!(ap.apply_credit(&manager,company,&json!({"credit_document_id":ap_id("zero-credit"),"target_document_id":ap_id("zero-invoice"),"effective_date":"2026-07-10","amount":"1","reference":"wrong-party"})).await.is_err());
    let apply_ar = json!({"credit_document_id":ar_id("credit"),"target_document_id":ar_id("invoice"),"effective_date":"2026-07-10","amount":"20","reference":"same-control"});
    let apply_ap = json!({"credit_document_id":ap_id("credit"),"target_document_id":ap_id("invoice"),"effective_date":"2026-07-10","amount":"20","reference":"same-control"});
    let ae = ar.apply_credit(&manager, company, &apply_ar).await?;
    let be = ap.apply_credit(&manager, company, &apply_ap).await?;
    assert!(ae["entry_id"].is_null() && be["entry_id"].is_null());
    assert_eq!(
        before,
        ledger
            .trial_balance(&reviewer, company, date(2026, 7, 1), None)
            .await?
    );
    let current_ar = ar
        .aged_receivables(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    let current_ap = ap
        .aged_payables(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    // An entryless action still requires both referenced postings inside the tuple.
    assert!(ar
        .aged_receivables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingOptions {
                through_entry_number: Some(0),
                through_opening_batch_id: Some(current_ar.through_opening_batch_id),
                through_settlement_number: Some(current_ar.through_settlement_number),
                ..Default::default()
            }
        )
        .await
        .is_err());
    assert!(ap
        .aged_payables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingQuery {
                through_entry_number: Some(0),
                through_opening_batch_id: Some(current_ap.through_opening_batch_id),
                through_settlement_number: Some(current_ap.through_settlement_number),
                ..Default::default()
            }
        )
        .await
        .is_err());
    assert_eq!(old_ar.through_entry_number, current_ar.through_entry_number);
    assert_eq!(
        old_ar.through_opening_batch_id,
        current_ar.through_opening_batch_id
    );
    assert!(old_ar.through_settlement_number < current_ar.through_settlement_number);
    assert_eq!(old_ap.through_entry_number, current_ap.through_entry_number);
    let replay_ar = ar
        .aged_receivables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingOptions {
                through_entry_number: Some(old_ar.through_entry_number),
                through_opening_batch_id: Some(old_ar.through_opening_batch_id),
                through_settlement_number: Some(old_ar.through_settlement_number),
                ..Default::default()
            },
        )
        .await?;
    let replay_ap = ap
        .aged_payables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingQuery {
                through_entry_number: Some(old_ap.through_entry_number),
                through_opening_batch_id: Some(old_ap.through_opening_batch_id),
                through_settlement_number: Some(old_ap.through_settlement_number),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&old_ar)?,
        serde_json::to_value(&replay_ar)?
    );
    assert_eq!(
        serde_json::to_value(&old_ap)?,
        serde_json::to_value(&replay_ap)?
    );
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    // Ordinary receipts and supplier payments share the dated capacity check.
    let receipt_input = json!({"series_id":config["payment_series"],"customer_id":config["customer_id"],"payment_date":"2026-07-11","amount":"81","deposit_account_id":config["bank"],"allocations":[{"document_id":ar_id("invoice"),"amount":"81"}]});
    assert!(ar
        .record_payment(&reviewer, company, &receipt_input)
        .await
        .is_err());
    let mut receipt_input = receipt_input;
    receipt_input["amount"] = json!("5");
    receipt_input["allocations"][0]["amount"] = json!("5");
    let receipt = ar
        .record_payment(&reviewer, company, &receipt_input)
        .await?;
    let mut payment_input = bilibi_finance_payables::NewPayment {
        payment_date: date(2026, 7, 11),
        vendor_id: config["vendor_id"].as_i64(),
        supplier_key: None,
        paid_from_account_id: config["bank"].as_i64().unwrap(),
        reference: Some("credit-capacity".into()),
        description: None,
        allocations: vec![bilibi_finance_payables::AllocationInput {
            document_id: ap_id("invoice"),
            amount: Money::parse("81", before.minor_units)?,
        }],
    };
    assert!(ap
        .submit_payment(&manager, company, &payment_input)
        .await
        .is_err());
    payment_input.allocations[0].amount = Money::parse("5", before.minor_units)?;
    let request = ap.submit_payment(&manager, company, &payment_input).await?;
    let payment = ap.approve_payment(&reviewer, company, request.id).await?;
    let registry = services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let client = db.get().await?;
    let ar_key = "finance.receivables.document.get";
    let bilibi_agent_api::Handler::Read(ar_read) = registry.operations[ar_key].definition.handler
    else {
        panic!("receivables document read must be a read operation");
    };
    let ar_record = ar_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":ar_id("invoice"),"as_of":"2026-07-31","through_entry_number":old_ar.through_entry_number,"through_opening_batch_id":old_ar.through_opening_batch_id,"through_settlement_number":old_ar.through_settlement_number}),
        )
        .await?
        .envelope(ar_key);
    assert!(ar_record["result"]["receipts"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(ar_record["result"]["settlements"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(ar_record["result"]["write_offs"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        ar_record["result"]["outstanding"]
            .as_str()
            .unwrap()
            .parse::<rust_decimal::Decimal>()?,
        rust_decimal::Decimal::from(100)
    );
    let ap_key = "finance.payables.document.get";
    let bilibi_agent_api::Handler::Read(ap_read) = registry.operations[ap_key].definition.handler
    else {
        panic!("payables document read must be a read operation");
    };
    let backdated_ar = ar
        .create_draft(
            &manager,
            company,
            &json!({
                "kind":"invoice",
                "series_id":config["invoice_series"],
                "customer_id":config["customer_id"],
                "issue_date":"2026-07-05",
                "description":"Later backdated membership proof",
                "lines":[{"description":"Later backdated membership proof","quantity":"1","unit_price":"1"}],
            }),
        )
        .await?;
    let backdated_ar = ar
        .issue_document(&manager, company, backdated_ar.id)
        .await?;
    assert!(ar_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":backdated_ar.id,"as_of":"2026-07-31","through_entry_number":old_ar.through_entry_number,"through_opening_batch_id":old_ar.through_opening_batch_id,"through_settlement_number":old_ar.through_settlement_number}),
        )
        .await
        .is_err());
    let backdated_ap = ap
        .create_bill(
            &manager,
            company,
            &NewDocument {
                vendor_id: config["vendor_id"].as_i64(),
                party_document_number: Some("LATER-BACKDATED".into()),
                document_date: date(2026, 7, 5),
                due_date: Some(date(2026, 7, 15)),
                description: Some("Later backdated membership proof".into()),
                lines: vec![DocumentLineInput {
                    description: "Later backdated membership proof".into(),
                    account_id: config["expense"].as_i64().expect("expense account"),
                    amount: Money::parse("1", before.minor_units)?,
                    tax_code_id: None,
                    external_order_line_id: None,
                    order_line_id: None,
                    quantity: None,
                }],
                tax_lines: None,
            },
        )
        .await?;
    let backdated_ap = bilibi_finance_walkthrough_fixture::payables::approve_then_post(
        &ap,
        &reviewer,
        &manager,
        company,
        backdated_ap.id,
    )
    .await?;
    assert!(ap_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":backdated_ap.id,"as_of":"2026-07-31","through_entry_number":old_ap.through_entry_number,"through_opening_batch_id":old_ap.through_opening_batch_id,"through_settlement_number":old_ap.through_settlement_number}),
        )
        .await
        .is_err());
    let ap_record = ap_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":ap_id("invoice"),"as_of":"2026-07-31","through_entry_number":old_ap.through_entry_number,"through_opening_batch_id":old_ap.through_opening_batch_id,"through_settlement_number":old_ap.through_settlement_number}),
        )
        .await?
        .envelope(ap_key);
    assert!(ap_record["result"]["settlements"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        ap_record["result"]["outstanding"]
            .as_str()
            .unwrap()
            .parse::<rust_decimal::Decimal>()?,
        rust_decimal::Decimal::from(100)
    );
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    ar.reverse_payment(
        &reviewer,
        company,
        receipt.id,
        &json!({"reversal_date":"2026-07-12","reason":"Disposable dated capacity proof"}),
    )
    .await?;
    ap.reverse_payment(
        &reviewer,
        company,
        payment.id,
        date(2026, 7, 12),
        Some("Disposable dated capacity proof".into()),
    )
    .await?;
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    let mut too_much = apply_ar.clone();
    too_much["reference"] = json!("too-much");
    too_much["amount"] = json!("11");
    assert!(ar.apply_credit(&manager, company, &too_much).await.is_err());
    let mut too_much = apply_ap.clone();
    too_much["reference"] = json!("too-much");
    too_much["amount"] = json!("11");
    assert!(ap.apply_credit(&manager, company, &too_much).await.is_err());
    // Conversion correction cannot orphan an unreversed settlement.
    let mut dependent_ar = ar_input.clone();
    dependent_ar["source_reference"] = json!("DEPENDENT-AR");
    dependent_ar["replaces_batch_id"] = a["id"].clone();
    dependent_ar["reason"] = json!("Reviewed conversion correction");
    let mut dependent_ap = ap_input.clone();
    dependent_ap["source_reference"] = json!("DEPENDENT-AP");
    dependent_ap["replaces_batch_id"] = b["id"].clone();
    dependent_ap["reason"] = json!("Reviewed conversion correction");
    assert!(ar
        .import_opening_invoices(&reviewer, company, &dependent_ar)
        .await
        .is_err());
    assert!(ap
        .import_opening_bills(&reviewer, company, &dependent_ap)
        .await
        .is_err());
    let reverse_ar =
        json!({"settlement_id":ae["id"],"effective_date":"2026-07-15","reference":"reverse-same"});
    let reverse_ap =
        json!({"settlement_id":be["id"],"effective_date":"2026-07-15","reference":"reverse-same"});
    ar.reverse_settlement(&manager, company, &reverse_ar)
        .await?;
    ap.reverse_settlement(&manager, company, &reverse_ap)
        .await?;
    let mut twice = reverse_ar.clone();
    twice["reference"] = json!("twice");
    assert!(ar
        .reverse_settlement(&manager, company, &twice)
        .await
        .is_err());
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    let mut backdated = apply_ar.clone();
    backdated["reference"] = json!("backdated-overdraw");
    backdated["effective_date"] = json!("2026-07-12");
    backdated["amount"] = json!("25");
    assert!(ar
        .apply_credit(&manager, company, &backdated)
        .await
        .is_err());
    let mut backdated = apply_ap.clone();
    backdated["reference"] = json!("backdated-overdraw");
    backdated["effective_date"] = json!("2026-07-12");
    backdated["amount"] = json!("25");
    assert!(ap
        .apply_credit(&manager, company, &backdated)
        .await
        .is_err());
    // Saved inactive controls remain settleable. The transfer is owned.
    let cross_ar = json!({"credit_document_id":ar_id("historical-credit"),"target_document_id":ar_id("invoice"),"effective_date":"2026-07-20","amount":"30","reference":"cross-control"});
    let cross_ap = json!({"credit_document_id":ap_id("historical-credit"),"target_document_id":ap_id("invoice"),"effective_date":"2026-07-20","amount":"30","reference":"cross-control"});
    let ax = ar.apply_credit(&manager, company, &cross_ar).await?;
    let bx = ap.apply_credit(&manager, company, &cross_ap).await?;
    // Deactivate only at zero, then restore through the owned reversal.
    // The subsequent application proves settlement of that saved inactive control.
    for code in ["1110", "2010"] {
        ledger
            .update_account(
                &reviewer,
                company,
                account(code),
                &bilibi_finance_ledger::AccountChange {
                    active: Some(false),
                    ..Default::default()
                },
            )
            .await?;
    }
    for (event, debit, credit) in [
        (&ax, account("1110"), account("1100")),
        (&bx, account("2000"), account("2010")),
    ] {
        let entry = ledger
            .get_entry(&reviewer, company, event["entry_id"].as_i64().unwrap())
            .await?;
        assert_eq!(
            entry.owner.as_ref().unwrap().module,
            if event == &ax {
                "finance/receivables"
            } else {
                "finance/payables"
            }
        );
        assert!(entry
            .lines
            .iter()
            .any(|l| l.account_id == debit
                && l.debit == Money::parse("30", before.minor_units).unwrap()));
        assert!(entry.lines.iter().any(|l| l.account_id == credit
            && l.credit == Money::parse("30", before.minor_units).unwrap()));
    }
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    let refund_ar = json!({"credit_document_id":ar_id("credit"),"effective_date":"2026-07-21","amount":"10","cash_account_id":config["bank"],"reference":"refund"});
    let refund_ap = json!({"credit_document_id":ap_id("credit"),"effective_date":"2026-07-21","amount":"10","cash_account_id":config["bank"],"reference":"refund"});
    let af = ar.refund_credit(&manager, company, &refund_ar).await?;
    let bf = ap.refund_credit(&manager, company, &refund_ap).await?;
    assert!(af["entry_id"].is_number() && bf["entry_id"].is_number());
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    for (receivable, event, reference) in [
        (true, &af, "reverse-refund"),
        (false, &bf, "reverse-refund"),
        (true, &ax, "reverse-transfer"),
        (false, &bx, "reverse-transfer"),
    ] {
        let input = json!({"settlement_id":event["id"],"effective_date":"2026-07-25","reference":reference});
        if receivable {
            ar.reverse_settlement(&manager, company, &input).await?;
        } else {
            ap.reverse_settlement(&manager, company, &input).await?;
        }
        tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    }
    let mut inactive_ar = cross_ar.clone();
    inactive_ar["reference"] = json!("inactive-transfer");
    inactive_ar["effective_date"] = json!("2026-07-26");
    let mut inactive_ap = cross_ap.clone();
    inactive_ap["reference"] = json!("inactive-transfer");
    inactive_ap["effective_date"] = json!("2026-07-26");
    let inactive_ar = ar.apply_credit(&manager, company, &inactive_ar).await?;
    let inactive_ap = ap.apply_credit(&manager, company, &inactive_ap).await?;
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    ar.reverse_settlement(&manager,company,&json!({"settlement_id":inactive_ar["id"],"effective_date":"2026-07-27","reference":"reverse-inactive"})).await?;
    ap.reverse_settlement(&manager,company,&json!({"settlement_id":inactive_ap["id"],"effective_date":"2026-07-27","reference":"reverse-inactive"})).await?;
    let ar_events = ar
        .evidence_page(&reviewer, company, "settlement", None, None)
        .await?;
    let ap_events = ap
        .evidence_page(&reviewer, company, "settlement", None, None)
        .await?;
    let ar_event_rows = ar_evidence_rows(&ar_events);
    for rows in [&ar_event_rows, &ap_events.rows] {
        assert!(rows
            .iter()
            .any(|e| e["kind"] == "reverse" && !e["reverses_settlement_id"].is_null()));
        assert!(rows
            .iter()
            .all(|e| !e["audit_trace_id"].as_str().unwrap_or("").is_empty()
                && !e["audit_action"].as_str().unwrap_or("").is_empty()));
    }
    // A split posting/settlement tuple is refused, rather than a false tie-out.
    assert!(ar
        .aged_receivables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingOptions {
                through_entry_number: Some(before.through_entry_number),
                ..Default::default()
            }
        )
        .await
        .is_err());
    assert!(ap
        .aged_payables(
            &reviewer,
            company,
            date(2026, 7, 31),
            &AgingQuery {
                through_entry_number: Some(before.through_entry_number),
                ..Default::default()
            }
        )
        .await
        .is_err());
    // Concurrent writers serialize under the company lock; only one can spend 20.
    let race_ar = [
        json!({"credit_document_id":ar_id("credit"),"target_document_id":ar_id("invoice"),"effective_date":"2026-07-26","amount":"20","reference":"race-a"}),
        json!({"credit_document_id":ar_id("credit"),"target_document_id":ar_id("invoice"),"effective_date":"2026-07-26","amount":"20","reference":"race-b"}),
    ];
    let (left, right) = tokio::join!(
        ar.apply_credit(&manager, company, &race_ar[0]),
        ar.apply_credit(&manager, company, &race_ar[1])
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let winner = left.or(right)?;
    ar.reverse_settlement(&manager,company,&json!({"settlement_id":winner["id"],"effective_date":"2026-07-27","reference":"reverse-race"})).await?;
    let race_ap = [
        json!({"credit_document_id":ap_id("credit"),"target_document_id":ap_id("invoice"),"effective_date":"2026-07-26","amount":"20","reference":"race-a"}),
        json!({"credit_document_id":ap_id("credit"),"target_document_id":ap_id("invoice"),"effective_date":"2026-07-26","amount":"20","reference":"race-b"}),
    ];
    let (left, right) = tokio::join!(
        ap.apply_credit(&manager, company, &race_ap[0]),
        ap.apply_credit(&manager, company, &race_ap[1])
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let winner = left.or(right)?;
    ap.reverse_settlement(&manager,company,&json!({"settlement_id":winner["id"],"effective_date":"2026-07-27","reference":"reverse-race"})).await?;
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    let before_fault = ledger
        .trial_balance(&reviewer, company, date(2026, 7, 31), None)
        .await?;
    let ar_before_fault = ar
        .aged_receivables(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    let ap_before_fault = ap
        .aged_payables(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    // The owned cash entry has been attempted before this deliberately late fault.
    db.get().await?.batch_execute("CREATE FUNCTION walkthrough_credit_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.reference='late-fault' THEN RAISE EXCEPTION 'disposable late settlement fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_credit_fault BEFORE INSERT ON finance_receivables_settlements FOR EACH ROW EXECUTE FUNCTION walkthrough_credit_fault(); CREATE TRIGGER walkthrough_credit_fault BEFORE INSERT ON finance_payables_settlements FOR EACH ROW EXECUTE FUNCTION walkthrough_credit_fault();").await?;
    let late_ar = json!({"credit_document_id":ar_id("credit"),"effective_date":"2026-07-28","amount":"1","cash_account_id":config["bank"],"reference":"late-fault"});
    let late_ap = json!({"credit_document_id":ap_id("credit"),"effective_date":"2026-07-28","amount":"1","cash_account_id":config["bank"],"reference":"late-fault"});
    assert!(ar.refund_credit(&manager, company, &late_ar).await.is_err());
    assert!(ap.refund_credit(&manager, company, &late_ap).await.is_err());
    db.get().await?.batch_execute("DROP TRIGGER walkthrough_credit_fault ON finance_receivables_settlements; DROP TRIGGER walkthrough_credit_fault ON finance_payables_settlements; DROP FUNCTION walkthrough_credit_fault();").await?;
    assert_eq!(
        before_fault,
        ledger
            .trial_balance(&reviewer, company, date(2026, 7, 31), None)
            .await?
    );
    assert_eq!(
        ar_before_fault.through_settlement_number,
        ar.aged_receivables(&reviewer, company, date(2026, 7, 31), &Default::default())
            .await?
            .through_settlement_number
    );
    assert_eq!(
        ap_before_fault.through_settlement_number,
        ap.aged_payables(&reviewer, company, date(2026, 7, 31), &Default::default())
            .await?
            .through_settlement_number
    );
    let year = ledger
        .list_fiscal_years(&reviewer, company)
        .await?
        .remove(0);
    let july = year
        .periods
        .iter()
        .find(|p| p.period_number == 7)
        .unwrap()
        .id;
    for period in year.periods.iter().filter(|p| p.period_number <= 7) {
        ledger
            .close_period(&reviewer, company, period.id, None)
            .await?;
    }
    let mut closed_ar = late_ar.clone();
    closed_ar["reference"] = json!("closed-period");
    let mut closed_ap = late_ap.clone();
    closed_ap["reference"] = json!("closed-period");
    assert!(ar
        .refund_credit(&manager, company, &closed_ar)
        .await
        .is_err());
    assert!(ap
        .refund_credit(&manager, company, &closed_ap)
        .await
        .is_err());
    ledger
        .reopen_period(
            &reviewer,
            company,
            july,
            "Continue disposable settlement proof",
        )
        .await?;
    // Future entryless events advance their own stream but do not alter July amounts.
    let future_ar=ar.apply_credit(&manager,company,&json!({"credit_document_id":ar_id("credit"),"target_document_id":ar_id("invoice"),"effective_date":"2026-09-01","amount":"5","reference":"future-application"})).await?;
    let future_ap=ap.apply_credit(&manager,company,&json!({"credit_document_id":ap_id("credit"),"target_document_id":ap_id("invoice"),"effective_date":"2026-09-01","amount":"5","reference":"future-application"})).await?;
    assert_eq!(
        ar_before_fault.total,
        ar.aged_receivables(&reviewer, company, date(2026, 7, 31), &Default::default())
            .await?
            .total
    );
    assert_eq!(
        ap_before_fault.total,
        ap.aged_payables(&reviewer, company, date(2026, 7, 31), &Default::default())
            .await?
            .total
    );
    ar.reverse_settlement(&manager,company,&json!({"settlement_id":future_ar["id"],"effective_date":"2026-09-02","reference":"reverse-future"})).await?;
    ap.reverse_settlement(&manager,company,&json!({"settlement_id":future_ap["id"],"effective_date":"2026-09-02","reference":"reverse-future"})).await?;
    // Correct at conversion only after dependent settlements have been reversed.
    let mut replacement_ar = stage_ar.clone();
    replacement_ar["source_reference"] = json!("REPLACEMENT-AR");
    replacement_ar["replaces_batch_id"] = a["id"].clone();
    replacement_ar["reason"] = json!("Correct source system identification");
    replacement_ar["csv"] = json!(ar_csv.replace("source-system", "corrected-system"));
    let mut replacement_ap = stage_ap.clone();
    replacement_ap["source_reference"] = json!("REPLACEMENT-AP");
    replacement_ap["replaces_batch_id"] = b["id"].clone();
    replacement_ap["reason"] = json!("Correct source system identification");
    replacement_ap["csv"] = json!(ap_csv.replace("source-system", "corrected-system"));
    ar.stage_opening_import(&manager, company, &replacement_ar)
        .await?;
    ap.stage_opening_import(&manager, company, &replacement_ap)
        .await?;
    replacement_ar["staged"] = json!(true);
    replacement_ar["csv"] = json!("");
    replacement_ap["staged"] = json!(true);
    replacement_ap["csv"] = json!("");
    let replaced_ar = ar
        .import_opening_invoices(&reviewer, company, &replacement_ar)
        .await?;
    let replaced_ap = ap
        .import_opening_bills(&reviewer, company, &replacement_ap)
        .await?;
    let replacement_ar_document = ar
        .list_documents(&reviewer, company, &Default::default())
        .await?
        .into_iter()
        .find(|document| {
            document.opening.as_ref().is_some_and(|opening| {
                opening["batch_id"] == replaced_ar["id"] && opening["source_id"] == "invoice"
            })
        })
        .expect("replacement customer opening document");
    let replacement_ap_document = ap
        .list_documents(&reviewer, company, &Default::default())
        .await?
        .into_iter()
        .find(|document| {
            document.opening.as_ref().is_some_and(|opening| {
                opening["batch_id"] == replaced_ap["id"] && opening["source_id"] == "invoice"
            })
        })
        .expect("replacement supplier opening document");
    assert!(ar_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":replacement_ar_document.id,"as_of":"2026-07-31","through_entry_number":old_ar.through_entry_number,"through_opening_batch_id":old_ar.through_opening_batch_id,"through_settlement_number":old_ar.through_settlement_number}),
        )
        .await
        .is_err());
    assert!(ap_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":replacement_ap_document.id,"as_of":"2026-07-31","through_entry_number":old_ap.through_entry_number,"through_opening_batch_id":old_ap.through_opening_batch_id,"through_settlement_number":old_ap.through_settlement_number}),
        )
        .await
        .is_err());
    assert!(ar_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":ar_id("invoice"),"as_of":"2026-07-31","through_entry_number":old_ar.through_entry_number,"through_opening_batch_id":old_ar.through_opening_batch_id,"through_settlement_number":old_ar.through_settlement_number}),
        )
        .await
        .is_ok());
    assert!(ap_read
        .call(
            &**client,
            &reviewer,
            &json!({"company_id":company,"document_id":ap_id("invoice"),"as_of":"2026-07-31","through_entry_number":old_ap.through_entry_number,"through_opening_batch_id":old_ap.through_opening_batch_id,"through_settlement_number":old_ap.through_settlement_number}),
        )
        .await
        .is_ok());
    assert!(ar
        .aged_receivables(&reviewer, company, date(2026, 7, 12), &Default::default(),)
        .await
        .is_err());
    assert!(ap
        .aged_payables(&reviewer, company, date(2026, 7, 12), &Default::default(),)
        .await
        .is_err());
    let replay = reports
        .conversion_report(&reviewer, company, date(2026, 7, 31), &conversion_pins)
        .await?;
    assert_eq!(conversion["receivables"], replay["receivables"]);
    assert_eq!(conversion["payables"], replay["payables"]);
    assert_eq!(conversion["pins"], replay["pins"]);
    assert_ne!(replaced_ar["id"], a["id"]);
    assert_ne!(replaced_ap["id"], b["id"]);
    tied(&ar, &ap, &reviewer, company, date(2026, 7, 31)).await?;
    assert_eq!(
        serde_json::to_value(&old_ap)?,
        serde_json::to_value(
            ap.aged_payables(
                &reviewer,
                company,
                date(2026, 7, 31),
                &AgingQuery {
                    through_entry_number: old_ap.pins().through_entry_number,
                    through_opening_batch_id: old_ap.pins().through_opening_batch_id,
                    through_settlement_number: old_ap.pins().through_settlement_number,
                    ..Default::default()
                }
            )
            .await?
        )?
    );

    // An unrelated owner remains a visible difference on its own control.
    let amount = Money::parse("1", before.minor_units)?;
    ledger
        .post_entry(
            &manager,
            company,
            &NewEntry {
                entry_date: date(2026, 7, 30),
                description: "Unrelated control adjustment".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(account("1100"), amount),
                    NewLine::credit(account("3000"), amount),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    let difference = ar
        .tie_out_control_accounts(&reviewer, company, date(2026, 7, 31), &Default::default())
        .await?;
    assert!(!difference.tied_out);
    assert!(difference
        .accounts
        .iter()
        .any(|a| a.account_id == account("1100") && a.difference.to_string() == "-1.0000"));
    // Earlier source and the complete pin tuple still replay after later entries.
    assert_eq!(
        serde_json::to_value(&old_ar)?,
        serde_json::to_value(
            ar.aged_receivables(
                &reviewer,
                company,
                date(2026, 7, 31),
                &AgingOptions {
                    through_entry_number: old_ar.pins().through_entry_number,
                    through_opening_batch_id: old_ar.pins().through_opening_batch_id,
                    through_settlement_number: old_ar.pins().through_settlement_number,
                    ..Default::default()
                }
            )
            .await?
        )?
    );
    let only = setup(db, services, 7).await?;
    ar.import_opening_invoices(&reviewer,7,&json!({"source_reference":"CREDIT-ONLY","currency_code":"MYR","series_id":only["invoice_series"],"csv":csv(false,true),"opening_account_classifications":only["opening_account_classifications"]})).await?;
    ap.import_opening_bills(&reviewer,7,&json!({"source_reference":"CREDIT-ONLY","currency_code":"MYR","csv":csv(true,true),"opening_account_classifications":only["opening_account_classifications"]})).await?;
    tied(&ar, &ap, &reviewer, 7, date(2026, 7, 31)).await?;
    println!("PASS: mixed, credit-only, partly settled, contra-sign and zero-net controls; source tax evidence; staged independent import; entryless application/reversal replay; saved inactive cross-control transfers, refunds and owned reversals; complete pins and per-control differences");
    Ok(())
}
