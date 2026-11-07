//! NS2 running proof, the `sst` binary's last section: SST returns opened,
//! adjusted, finalised into a snapshot, reproduced, amended, discarded,
//! exported and listed as evidence, through the real facades only, on top of
//! NS1's registration, mappings and documents. The database's own refusals
//! run last, each rolled back.
use super::{amounts, d, db_refuses, field, refused, show, Outcome};
use bilibi_authz::grants::Grants;
use bilibi_database::Db;
use bilibi_finance_receivables::Receivables;
use bilibi_finance_sst::{
    AdjustmentInput, ExportFormat, MappingInput, RegistrationInput, ReturnStatus, Sst,
    SstError as E, TaxType,
};
use bilibi_finance_walkthrough_fixture::{sign_in, COMPANY};
use bilibi_module_registry::Services;
use jiff::civil::date;
use serde_json::json;
use std::collections::HashMap;

/// What NS1 set up that the returns build on.
pub(crate) struct Fixture<'a> {
    pub db: &'a Db,
    pub services: &'a Services,
    pub invoice_series: i64,
    pub customer: i64,
    pub codes: &'a HashMap<&'static str, i64>,
    pub sales_tax_account: i64,
}

fn adjust(box_key: &str, taxable: Option<&str>, tax: Option<&str>, text: &str) -> AdjustmentInput {
    AdjustmentInput {
        box_key: box_key.into(),
        taxable_amount: taxable.map(str::to_owned),
        tax_amount: tax.map(str::to_owned),
        description: Some(text.into()),
        reference: None,
    }
}

pub(crate) async fn run(f: Fixture<'_>) -> Outcome<()> {
    println!("\n== SST NS2: returns opened, adjusted, finalised, reproduced, amended, discarded, exported");
    let (db, services) = (f.db, f.services);
    let sst = services.get::<Sst>(bilibi_finance_sst::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let manager = sign_in(db, services, 1, 1).await?;
    let second = sign_in(db, services, 1, 1).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let other = sign_in(db, services, 2, 5).await?;
    let raw = db.get().await?;
    let jan = date(2026, 1, 1);
    let invoice = |day: &str, code: &str, price: &str| {
        json!({"kind":"invoice","series_id":f.invoice_series,"customer_id":f.customer,"issue_date":day,
            "lines":[{"description":format!("{code} line"),"quantity":"1","unit_price":price,"tax_code_id":f.codes[code]}]})
    };
    let user = manager.user();
    let withhold = |cap: &'static str| grants.set_direct(&admin, user, cap, Some(false));
    let restore = |cap: &'static str| grants.set_direct(&admin, user, cap, None);

    // Opening.
    withhold(bilibi_finance_sst::PREPARE_RETURNS).await?;
    let denied = sst
        .open_return(&manager, COMPANY, TaxType::SalesTax, jan)
        .await;
    restore(bilibi_finance_sst::PREPARE_RETURNS).await?;
    refused("without the prepare grant, no return", denied, |e| {
        matches!(e, E::Unauthorized)
    })?;
    refused(
        "staff cannot open a return",
        sst.open_return(&staff, COMPANY, TaxType::SalesTax, jan)
            .await,
        |e| matches!(e, E::Unauthorized),
    )?;
    refused(
        "a period before the registration",
        sst.open_return(&manager, COMPANY, TaxType::SalesTax, date(2025, 11, 1))
            .await,
        |e| matches!(e, E::NotRegistered),
    )?;
    refused(
        "a date that starts no period",
        sst.open_return(&manager, COMPANY, TaxType::SalesTax, date(2026, 1, 15))
            .await,
        |e| matches!(e, E::NotATaxablePeriod),
    )?;
    let sales = sst
        .open_return(&manager, COMPANY, TaxType::SalesTax, jan)
        .await?;
    assert_eq!(
        (
            sales.return_number,
            sales.status,
            sales.period_end,
            sales.due_date
        ),
        (1, ReturnStatus::Open, date(2026, 2, 28), date(2026, 3, 31))
    );
    assert_eq!(
        sales
            .events
            .iter()
            .map(|e| e.action.as_str())
            .collect::<Vec<_>>(),
        ["opened"]
    );
    assert_eq!(sales.opened_by_user_id, manager.user());
    println!(
        "return {} opened: sales tax {}..{}, due {}, under registration {}",
        sales.return_number,
        sales.period_start,
        sales.period_end,
        sales.due_date,
        sales.registration_id
    );
    refused(
        "a second return of the same period",
        sst.open_return(&manager, COMPANY, TaxType::SalesTax, jan)
            .await,
        |e| matches!(e, E::ReturnOpen),
    )?;

    // Adjustments.
    let id = sales.id;
    for (label, input, name, message) in [
        (
            "a total",
            adjust("12", None, Some("1"), "Total"),
            "box",
            "is not a box of this return a preparer adjusts",
        ),
        (
            "a service tax box on a sales tax return",
            adjust("11c", Some("1"), Some("1"), "Services"),
            "box",
            "is not a box of this return a preparer adjusts",
        ),
        (
            "a negative tax",
            adjust("13d", None, Some("-1"), "Relief"),
            "tax_amount",
            "must not be negative",
        ),
        (
            "a value on a tax-only box",
            adjust("13d", Some("5"), Some("1"), "Relief"),
            "taxable_amount",
            "is not a column of this box",
        ),
        (
            "an amount finer than the book",
            adjust("13d", None, Some("1.234"), "Relief"),
            "tax_amount",
            "has more than 2 decimal places",
        ),
        (
            "sixteen digits",
            adjust("13d", None, Some("10000000000000000"), "Relief"),
            "tax_amount",
            "is too large",
        ),
        (
            "nothing to add",
            adjust("13d", None, Some("0"), "Relief"),
            "tax_amount",
            "or the value must be above zero",
        ),
        (
            "a rated box without its value",
            adjust("11a", None, Some("5"), "Own use"),
            "taxable_amount",
            "can't be blank",
        ),
        (
            "no description",
            adjust("13d", None, Some("4"), " "),
            "description",
            "can't be blank",
        ),
    ] {
        field(
            label,
            sst.add_adjustment(&manager, COMPANY, id, &input).await,
            name,
            message,
        )?;
    }
    refused(
        "an adjustment to a missing return",
        sst.add_adjustment(
            &manager,
            COMPANY,
            999_999,
            &adjust("13d", None, Some("4"), "x"),
        )
        .await,
        |e| matches!(e, E::ReturnNotFound),
    )?;
    let relief = sst
        .add_adjustment(
            &manager,
            COMPANY,
            id,
            &AdjustmentInput {
                reference: Some("JKDM No. 2 approval 7".into()),
                ..adjust("13d", None, Some("4.00"), "Bad debt relief approved")
            },
        )
        .await?;
    let own_use = sst
        .add_adjustment(
            &manager,
            COMPANY,
            id,
            &adjust("11b", Some("50"), Some("5"), "Goods taken for own use"),
        )
        .await?;
    let deduction = sst
        .add_adjustment(
            &manager,
            COMPANY,
            id,
            &adjust("13b", None, Some("3"), "Sales tax deduction"),
        )
        .await?;
    assert_eq!(
        (
            relief.adjustment_number,
            own_use.adjustment_number,
            deduction.adjustment_number
        ),
        (1, 2, 3)
    );
    assert_eq!(
        (relief.taxable_amount, relief.tax_amount),
        (Some(d("0")), Some(d("4")))
    );
    field(
        "a withdrawal without a reason",
        sst.withdraw_adjustment(&manager, COMPANY, deduction.id, Some(""))
            .await,
        "description",
        "can't be blank",
    )?;
    let withdrawal = sst
        .withdraw_adjustment(&manager, COMPANY, deduction.id, Some("Claimed twice"))
        .await?;
    assert_eq!(
        (
            withdrawal.action.as_str(),
            withdrawal.withdraws_adjustment_id
        ),
        ("withdrawn", Some(deduction.id))
    );
    refused(
        "withdrawing it again",
        sst.withdraw_adjustment(&manager, COMPANY, deduction.id, Some("Again"))
            .await,
        |e| matches!(e, E::AdjustmentWithdrawn),
    )?;
    refused(
        "withdrawing a withdrawal",
        sst.withdraw_adjustment(&manager, COMPANY, withdrawal.id, Some("Again"))
            .await,
        |e| matches!(e, E::AdjustmentNotFound),
    )?;
    let report = sst.get_return(&manager, COMPANY, id).await?;
    let figures = report.figures.as_ref().expect("an open return's figures");
    show("open sales tax return with adjustments", figures);
    assert_eq!(amounts(figures, "11b"), (Some(d("960")), Some(d("96"))));
    assert_eq!(amounts(figures, "13d").1, Some(d("4")));
    assert_eq!(amounts(figures, "13b").1, Some(d("0")));
    assert_eq!(figures.tax_payable, d("102"));
    assert_eq!(
        report
            .adjustments
            .iter()
            .map(|a| (a.adjustment_number, a.standing))
            .collect::<Vec<_>>(),
        [(1, true), (2, true), (3, false)]
    );
    assert_eq!(
        report.adjustments[2]
            .withdrawal
            .as_ref()
            .map(|w| w.description.as_str()),
        Some("Claimed twice")
    );
    assert!(report.period_ended && report.posted_since.is_none() && report.amends.is_none());
    assert_eq!(
        report.registration_number.as_deref(),
        Some("X01-2601-00000001")
    );
    println!("adjustments 1 (13(d) relief 4) and 2 (11(b) own use 50/5) stand, 3 (13(b) 3) is withdrawn: item 14 is 102");

    // What stops a return finalising.
    let new_code = ar
        .create_tax_code(
            &admin,
            COMPANY,
            &json!({"code":"NEW10","name":"New 10% goods","rate":"10","account_id":f.sales_tax_account}),
        )
        .await?
        .id;
    let mut codes = f.codes.clone();
    codes.insert("NEW10", new_code);
    let invoice = |day: &str, code: &str, price: &str| {
        let mut v = invoice(day, "ST10", price);
        v["lines"][0]["tax_code_id"] = json!(codes[code]);
        v["lines"][0]["description"] = json!(format!("{code} line"));
        v
    };
    super::issue(&ar, &admin, invoice("2026-02-27", "NEW10", "100")).await?;
    withhold(bilibi_finance_sst::FINALISE_RETURNS).await?;
    let denied = sst.finalise_return(&manager, COMPANY, id).await;
    restore(bilibi_finance_sst::FINALISE_RETURNS).await?;
    refused("without the finalise grant", denied, |e| {
        matches!(e, E::Unauthorized)
    })?;
    refused(
        "an unmapped tax code",
        sst.finalise_return(&manager, COMPANY, id).await,
        |e| matches!(e, E::UnmappedTaxCodes(rows) if rows.iter().map(|r| r.tax_code.as_str()).eq(["NEW10"])),
    )?;
    let map = |code: i64, key: &str| MappingInput {
        source: "receivables".into(),
        tax_code_id: code,
        box_key: Some(key.into()),
        reason: Some("New goods code".into()),
    };
    sst.map_tax_code(&manager, COMPANY, &map(new_code, "11a"))
        .await?;
    refused(
        "a 10% code in the 5% box",
        sst.finalise_return(&manager, COMPANY, id).await,
        |e| matches!(e, E::RateMismatch(rows) if rows.iter().map(|r| r.tax_code.as_str()).eq(["NEW10"])),
    )?;
    sst.map_tax_code(&manager, COMPANY, &map(new_code, "11b"))
        .await?;
    let current = sst
        .taxable_periods(&manager, COMPANY, None, Some(1))
        .await?[0];
    let running = sst
        .open_return(&manager, COMPANY, TaxType::SalesTax, current.period_start)
        .await?;
    refused(
        &format!(
            "the running period {}..{}",
            current.period_start, current.period_end
        ),
        sst.finalise_return(&manager, COMPANY, running.id).await,
        |e| matches!(e, E::PeriodNotEnded),
    )?;
    field(
        "a discard without a reason",
        sst.discard_return(&manager, COMPANY, running.id, None)
            .await,
        "reason",
        "is needed to discard a return",
    )?;
    let discarded = sst
        .discard_return(&manager, COMPANY, running.id, Some("Prepared too early"))
        .await?;
    assert_eq!(discarded.status, ReturnStatus::Discarded);
    refused(
        "discarding it again",
        sst.discard_return(&manager, COMPANY, running.id, Some("Again"))
            .await,
        |e| matches!(e, E::ReturnDiscarded),
    )?;
    refused(
        "an adjustment to a discarded return",
        sst.add_adjustment(
            &manager,
            COMPANY,
            running.id,
            &adjust("13d", None, Some("1"), "x"),
        )
        .await,
        |e| matches!(e, E::ReturnDiscarded),
    )?;
    let discarded_report = sst.get_return(&manager, COMPANY, running.id).await?;
    assert!(discarded_report.figures.is_none());
    refused(
        "exporting a discarded return",
        sst.export_return(&manager, COMPANY, running.id, ExportFormat::Csv)
            .await,
        |e| matches!(e, E::ReturnDiscarded),
    )?;
    let reopened = sst
        .open_return(&manager, COMPANY, TaxType::SalesTax, current.period_start)
        .await?;
    assert_ne!(reopened.id, running.id);
    sst.discard_return(&manager, COMPANY, reopened.id, Some("Not yet"))
        .await?;
    println!("the running period's return refused as not ended, discarded with a reason, kept on record and prepared again");

    // Finalising.
    let draft = sst
        .export_return(&manager, COMPANY, id, ExportFormat::Csv)
        .await?;
    assert!(draft
        .content
        .contains("A,,Status,\"Draft, not finalised\",\r\n"));
    let before = sst
        .get_return(&manager, COMPANY, id)
        .await?
        .figures
        .expect("figures");
    assert_eq!(amounts(&before, "11b"), (Some(d("1060")), Some(d("106"))));
    let finalised = sst.finalise_return(&manager, COMPANY, id).await?;
    let event = finalised
        .events
        .iter()
        .find(|e| e.action == "finalised")
        .expect("finalised event");
    assert_eq!(
        (
            finalised.status,
            finalised.version,
            finalised.tax_payable,
            finalised.currency_code.as_deref()
        ),
        (
            ReturnStatus::Finalised,
            Some(1),
            Some(d("112")),
            Some("MYR")
        )
    );
    assert_eq!(event.recorded_by_user_id, manager.user());
    println!(
        "return {} finalised as version 1: tax payable {}, read through entry {:?}, mapping {:?}, adjustment {:?}",
        finalised.return_number,
        event.tax_payable.unwrap_or_default(),
        event.through_entry_number,
        event.through_mapping_number,
        event.through_adjustment_number
    );
    let stored = sst.get_return(&manager, COMPANY, id).await?;
    let snapshot = stored.figures.as_ref().expect("snapshot");
    assert_eq!(
        (&snapshot.boxes, &snapshot.sources, &snapshot.accounts),
        (&before.boxes, &before.sources, &before.accounts)
    );
    assert_eq!(
        snapshot.through_adjustment_number,
        event.through_adjustment_number
    );
    assert_eq!(stored.posted_since.as_ref().map(|p| p.boxes.len()), Some(0));
    let companies = services.get::<bilibi_company::CompanyService>(bilibi_company::SERVICE)?;
    companies
        .suspend_company(
            &admin,
            22,
            Some("Walkthrough: customer suspended after finalising"),
        )
        .await?;
    raw.execute(
        "UPDATE companies SET legal_name='SST later customer' WHERE tenant_id=1 AND id=22",
        &[],
    )
    .await?;
    let renamed = sst.get_return(&manager, COMPANY, id).await?;
    assert_eq!(renamed.figures.as_ref(), Some(snapshot));
    assert!(
        sst.reproduce_return(&manager, COMPANY, id)
            .await?
            .reproduced
    );
    companies
        .reactivate_company(&admin, 22, Some("Walkthrough: customer reactivated"))
        .await?;
    println!("its stored snapshot equals the figures read just before: every box, source row and account");
    for (label, result) in [
        (
            "an adjustment to a finalised return",
            sst.add_adjustment(&manager, COMPANY, id, &adjust("13d", None, Some("1"), "x"))
                .await
                .map(|_| ()),
        ),
        (
            "finalising it again",
            sst.finalise_return(&manager, COMPANY, id).await.map(|_| ()),
        ),
        (
            "discarding a finalised return",
            sst.discard_return(&manager, COMPANY, id, Some("x"))
                .await
                .map(|_| ()),
        ),
        (
            "withdrawing an adjustment of a finalised return",
            sst.withdraw_adjustment(&manager, COMPANY, relief.id, Some("x"))
                .await
                .map(|_| ()),
        ),
        (
            "a new return of a finalised period",
            sst.open_return(&manager, COMPANY, TaxType::SalesTax, jan)
                .await
                .map(|_| ()),
        ),
    ] {
        refused(label, result, |e| matches!(e, E::ReturnFinalised))?;
    }
    let reproduced = sst.reproduce_return(&manager, COMPANY, id).await?;
    assert!(reproduced.reproduced);
    refused(
        "reproducing a discarded return",
        sst.reproduce_return(&manager, COMPANY, running.id).await,
        |e| matches!(e, E::ReturnNotFinalised),
    )?;
    field(
        "a cycle change into the reported period",
        sst.register(
            &manager,
            COMPANY,
            &RegistrationInput {
                period_start: Some(date(2026, 2, 1)),
                period_months: Some(1),
                ..Default::default()
            },
        )
        .await,
        "period_start",
        "must fall after 2026-02-28, the end of the last finalised return's period, to change the cycle or basis",
    )?;

    // A document dated in the period and posted after it was finalised.
    super::issue(&ar, &admin, invoice("2026-02-26", "ST10", "20")).await?;
    let after = sst.get_return(&manager, COMPANY, id).await?;
    let since = after.posted_since.as_ref().expect("posted since");
    assert_eq!(after.figures.as_ref(), Some(snapshot));
    assert_eq!(
        since
            .boxes
            .iter()
            .map(|b| (b.key, b.tax_change))
            .collect::<Vec<_>>(),
        [
            ("11b", Some(d("2"))),
            ("12", Some(d("2"))),
            ("14", Some(d("2")))
        ]
    );
    assert!(
        sst.reproduce_return(&manager, COMPANY, id)
            .await?
            .reproduced
    );
    println!(
        "a February invoice posted later: the snapshot stands, posted_since through entry {} shows 11(b), 12 and 14 up 2; reproduction still matches",
        since.through_entry_number
    );
    withhold("finance.receivables.view").await?;
    let snapshot_only = sst.get_return(&manager, COMPANY, id).await;
    restore("finance.receivables.view").await?;
    let snapshot_only = snapshot_only?;
    assert!(snapshot_only.posted_since.is_none() && snapshot_only.figures.is_some());
    println!("without the receivables view the snapshot still reads, without posted_since");

    // Amending.
    field(
        "an amendment without a reason",
        sst.amend_return(&manager, COMPANY, id, Some("  ")).await,
        "reason",
        "is needed to amend a finalised return",
    )?;
    withhold(bilibi_finance_sst::AMEND_RETURNS).await?;
    let denied = sst
        .amend_return(&manager, COMPANY, id, Some("Late invoice"))
        .await;
    restore(bilibi_finance_sst::AMEND_RETURNS).await?;
    refused("without the amend grant", denied, |e| {
        matches!(e, E::Unauthorized)
    })?;
    refused(
        "amending a discarded return",
        sst.amend_return(&manager, COMPANY, running.id, Some("x"))
            .await,
        |e| matches!(e, E::ReturnNotFinalised),
    )?;
    let amendment = sst
        .amend_return(
            &manager,
            COMPANY,
            id,
            Some("Invoice of 26 February left out"),
        )
        .await?;
    assert_eq!(
        (amendment.amends_return_id, amendment.status),
        (Some(id), ReturnStatus::Open)
    );
    refused(
        "a second amendment while one is open",
        sst.amend_return(&manager, COMPANY, id, Some("Again")).await,
        |e| matches!(e, E::ReturnOpen),
    )?;
    let amended = sst.get_return(&manager, COMPANY, amendment.id).await?;
    assert_eq!(
        amended
            .adjustments
            .iter()
            .map(|a| (a.box_key.as_str(), a.copied_from_adjustment_id))
            .collect::<Vec<_>>(),
        [("13d", Some(relief.id)), ("11b", Some(own_use.id))]
    );
    let copy = &amended.adjustments[0];
    sst.withdraw_adjustment(
        &manager,
        COMPANY,
        copy.id,
        Some("Relief reversed: debt recovered"),
    )
    .await?;
    let amended = sst.get_return(&manager, COMPANY, amendment.id).await?;
    let shown = amended.amends.as_ref().expect("amends");
    assert_eq!(
        (shown.return_id, shown.version, shown.tax_payable),
        (id, Some(1), d("112"))
    );
    assert_eq!(
        shown
            .boxes
            .iter()
            .map(|b| (b.key, b.tax_change))
            .collect::<Vec<_>>(),
        [
            ("11b", Some(d("2"))),
            ("12", Some(d("2"))),
            ("13d", Some(d("-4"))),
            ("14", Some(d("6")))
        ]
    );
    let amended_final = sst.finalise_return(&manager, COMPANY, amendment.id).await?;
    assert_eq!(
        (amended_final.version, amended_final.tax_payable),
        (Some(2), Some(d("118")))
    );
    let original = sst.get_return(&manager, COMPANY, id).await?;
    assert_eq!(
        original.tax_return.superseded_by_return_id,
        Some(amendment.id)
    );
    refused(
        "amending a superseded version",
        sst.amend_return(&manager, COMPANY, id, Some("Again")).await,
        |e| matches!(e, E::ReturnSuperseded),
    )?;
    assert!(
        sst.reproduce_return(&manager, COMPANY, amendment.id)
            .await?
            .reproduced
    );
    println!(
        "return {} amends version 1 with copies of its standing adjustments, withdraws the copied relief, and is finalised as version 2 (tax payable 118); version 1 is superseded",
        amendment.return_number
    );

    // Two sessions finalise the same service tax return at once.
    let service = sst
        .open_return(&manager, COMPANY, TaxType::ServiceTax, jan)
        .await?;
    let (one, two) = tokio::join!(
        sst.finalise_return(&manager, COMPANY, service.id),
        sst.finalise_return(&second, COMPANY, service.id)
    );
    let outcomes = [one.is_ok(), two.is_ok()];
    assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1);
    refused(
        "the slower of two concurrent finalisings",
        if outcomes[0] { two } else { one },
        |e| matches!(e, E::ReturnFinalised),
    )?;
    let service_report = sst.get_return(&manager, COMPANY, service.id).await?;
    show(
        "finalised service tax return (payment basis)",
        service_report.figures.as_ref().expect("snapshot"),
    );
    assert_eq!(service_report.tax_return.version, Some(1));

    // Export.
    let csv = sst
        .export_return(&manager, COMPANY, amendment.id, ExportFormat::Csv)
        .await?;
    assert_eq!(
        (csv.filename.as_str(), csv.content_type),
        (
            format!(
                "sst-02-sales-tax-2026-01-01-2026-02-28-return-{}.csv",
                amendment.return_number
            )
            .as_str(),
            "text/csv"
        )
    );
    for line in [
        "part,item,description,value,tax\r\n",
        "A,,Status,\"Finalised, version 2\",\r\n",
        "A,,Amendment,yes,\r\n",
        "A,1,SST registration number,X01-2601-00000001,\r\n",
        "A,4,Return and payment due date,2026-03-31,\r\n",
        "B2,11(b),Taxable goods at 10%,1080.00,108.00\r\n",
        "B2,13(d),Bad debt relief,,0.00\r\n",
        "B2,14,Tax payable before penalty,,118.00\r\n",
    ] {
        assert!(csv.content.contains(line), "CSV lacks {line:?}");
    }
    println!(
        "CSV {} ({} lines):",
        csv.filename,
        csv.content.lines().count()
    );
    for line in csv.content.lines().take(12) {
        println!("  {line}");
    }
    let open_june = sst
        .open_return(&manager, COMPANY, TaxType::SalesTax, date(2026, 5, 1))
        .await?;
    sst.add_adjustment(
        &manager,
        COMPANY,
        open_june.id,
        &AdjustmentInput {
            reference: Some("=HYPERLINK(\"x\")".into()),
            ..adjust(
                "11b",
                Some("10"),
                Some("1"),
                "<script>alert(1)</script> own use",
            )
        },
    )
    .await?;
    let html = sst
        .export_return(&manager, COMPANY, open_june.id, ExportFormat::Html)
        .await?;
    assert_eq!(html.content_type, "text/html");
    assert!(html.content.contains(
        "<p class=\"draft\">Draft: not finalised. Its figures change as documents post.</p>"
    ));
    assert!(html
        .content
        .contains("&lt;script&gt;alert(1)&lt;/script&gt; own use (=HYPERLINK(&quot;x&quot;))"));
    assert!(!html.content.contains("<script>"));
    println!(
        "printable {} is marked a draft and escapes the preparer's text",
        html.filename
    );

    // An overlapping cycle, and a discarded return that no longer blocks it.
    let v5 = sst
        .register(
            &manager,
            COMPANY,
            &RegistrationInput {
                period_months: Some(1),
                period_start: Some(date(2026, 6, 1)),
                reason: Some("Monthly from June".into()),
                ..Default::default()
            },
        )
        .await?;
    refused(
        "June under the monthly cycle while May-June is prepared",
        sst.open_return(&manager, COMPANY, TaxType::SalesTax, date(2026, 6, 1))
            .await,
        |e| matches!(e, E::PeriodOverlaps),
    )?;
    sst.discard_return(
        &manager,
        COMPANY,
        open_june.id,
        Some("Cycle varied to monthly"),
    )
    .await?;
    let june = sst
        .open_return(&manager, COMPANY, TaxType::SalesTax, date(2026, 6, 1))
        .await?;
    assert_eq!(
        (june.registration_id, june.period_end),
        (v5.id, date(2026, 6, 30))
    );
    println!("registration version {} varies the cycle to monthly from June: the open May-June return overlaps until it is discarded", v5.version);

    // Listing.
    let all = sst
        .list_returns(&manager, COMPANY, None, None, None)
        .await?;
    println!("returns on record:");
    for r in &all {
        println!(
            "  {} {} {}..{} {}{}{}",
            r.return_number,
            r.tax_type,
            r.period_start,
            r.period_end,
            r.status.as_str(),
            r.version.map_or(String::new(), |v| format!(" version {v}")),
            r.superseded_by_return_id
                .map_or(String::new(), |s| format!(", superseded by return {s}"))
        );
    }
    assert!(all.windows(2).all(|w| w[0].id < w[1].id));
    let services_only = sst
        .list_returns(&manager, COMPANY, Some(TaxType::ServiceTax), None, None)
        .await?;
    assert_eq!(
        services_only.iter().map(|r| r.id).collect::<Vec<_>>(),
        [service.id]
    );
    let page = sst
        .list_returns(&manager, COMPANY, None, Some(2), Some(all[0].id))
        .await?;
    assert_eq!(
        page.iter().map(|r| r.id).collect::<Vec<_>>(),
        [all[1].id, all[2].id]
    );
    refused(
        "a page of 501 returns",
        sst.list_returns(&manager, COMPANY, None, Some(501), None)
            .await,
        |e| matches!(e, E::InvalidOption),
    )?;

    // Evidence.
    assert_eq!(
        Sst::evidence_datasets(),
        [
            "adjustment",
            "mapping",
            "registration",
            "return",
            "return_account",
            "return_box",
            "return_event",
            "return_source"
        ]
    );
    for dataset in Sst::evidence_datasets() {
        let page = sst
            .evidence_page(&manager, COMPANY, dataset, None, None)
            .await?;
        println!(
            "evidence {dataset}: {} rows of {} fields",
            page.rows.len(),
            page.fields.len()
        );
    }
    let first = sst
        .evidence_page(&manager, COMPANY, "return", Some(2), None)
        .await?;
    assert_eq!(first.next, first.rows[1][0].as_i64());
    assert_eq!(
        (first.rows[0][6].as_str(), first.fields[6]),
        (Some("2026-01-01"), "period_start")
    );
    let event_page = sst
        .evidence_page(&manager, COMPANY, "return_event", None, None)
        .await?;
    assert!(event_page
        .rows
        .iter()
        .any(|r| r[13].as_str() == Some("112.0000")));
    refused(
        "an unknown dataset",
        sst.evidence_page(&manager, COMPANY, "secrets", None, None)
            .await,
        |e| matches!(e, E::InvalidOption),
    )?;
    refused(
        "another tenant's evidence",
        sst.evidence_page(&other, COMPANY, "return", None, None)
            .await,
        |e| matches!(e, E::Unauthorized | E::CompanyNotFound),
    )?;

    // Scope.
    refused(
        "another tenant reads no return",
        sst.get_return(&other, COMPANY, id).await,
        |e| matches!(e, E::Unauthorized | E::CompanyNotFound),
    )?;
    refused(
        "staff read no return",
        sst.get_return(&staff, COMPANY, id).await,
        |e| matches!(e, E::Unauthorized),
    )?;
    raw.batch_execute("UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=1")
        .await?;
    let archived = sst
        .add_adjustment(
            &manager,
            COMPANY,
            june.id,
            &adjust("13d", None, Some("1"), "x"),
        )
        .await;
    let readable = sst.get_return(&manager, COMPANY, id).await;
    raw.batch_execute("UPDATE companies SET status='active' WHERE tenant_id=1 AND id=1")
        .await?;
    refused("an archived company", archived, |e| {
        matches!(e, E::CompanyArchived)
    })?;
    assert_eq!(readable?.tax_return.version, Some(1));

    // Every row is audited.
    for (table, kind) in [
        ("finance_sst_returns", "return"),
        ("finance_sst_return_events", "return_event"),
        ("finance_sst_return_adjustments", "adjustment"),
        ("finance_sst_return_boxes", "return_box"),
        ("finance_sst_return_sources", "return_source"),
        ("finance_sst_return_accounts", "return_account"),
    ] {
        let (rows, missing): (i64, i64) = {
            let r = raw
                .query_one(
                    &format!("SELECT count(*), count(*) FILTER (WHERE NOT EXISTS (SELECT 1 FROM base_audit_mutations a WHERE a.tenant_id=r.tenant_id AND a.auditable_type=$1 AND a.auditable_id=r.id::text)) FROM {table} r"),
                    &[&format!("finance.sst.{kind}")],
                )
                .await?;
            (r.get(0), r.get(1))
        };
        assert!(rows > 0 && missing == 0, "unaudited {table}");
        println!("{table}: {rows} rows, each with its audit mutation");
    }

    // The database's own rules, each rolled back.
    let june_id = june.id;
    db_refuses(
        &raw,
        "update a return event",
        "UPDATE finance_sst_return_events SET reason='edited'",
        "UPDATE is refused",
    )
    .await?;
    db_refuses(
        &raw,
        "delete an adjustment",
        "DELETE FROM finance_sst_return_adjustments",
        "DELETE is refused",
    )
    .await?;
    db_refuses(&raw, "a box added to a finalised return later", &format!("INSERT INTO finance_sst_return_boxes (tenant_id,company_id,return_id,line_number,box,taxable_amount,tax_amount) VALUES (1,1,{id},99,'13b',NULL,1)"), "is not being finalised").await?;
    db_refuses(&raw, "an adjustment to a finalised return", &format!("INSERT INTO finance_sst_return_adjustments (tenant_id,company_id,adjustment_number,return_id,action,box,taxable_amount,tax_amount,description,recorded_by_user_id,recorded_at) VALUES (1,1,0,{id},'added','13d',0,1,'Late relief',1,now())"), "its adjustments are kept as they are").await?;
    db_refuses(&raw, "opening a recorded return again", &format!("INSERT INTO finance_sst_return_events (tenant_id,company_id,event_number,return_id,action,recorded_by_user_id,recorded_at) VALUES (1,1,0,{june_id},'opened',1,now())"), "it is not opened again").await?;
    db_refuses(&raw, "discarding a finalised return", &format!("INSERT INTO finance_sst_return_events (tenant_id,company_id,event_number,return_id,action,reason,recorded_by_user_id,recorded_at) VALUES (1,1,0,{id},'discarded','x',1,now())"), "is already closed").await?;
    let foreign = deduction.id;
    db_refuses(&raw, "withdrawing another return's adjustment", &format!("INSERT INTO finance_sst_return_adjustments (tenant_id,company_id,adjustment_number,return_id,action,withdraws_adjustment_id,description,recorded_by_user_id,recorded_at) VALUES (1,1,0,{june_id},'withdrawn',{foreign},'Again',1,now())"), "is not an adjustment added to return").await?;
    let added = sst
        .add_adjustment(
            &manager,
            COMPANY,
            june_id,
            &adjust("13d", None, Some("1"), "Relief"),
        )
        .await?;
    sst.withdraw_adjustment(&manager, COMPANY, added.id, Some("Not approved"))
        .await?;
    let twice = added.id;
    db_refuses(&raw, "a second withdrawal of one adjustment", &format!("INSERT INTO finance_sst_return_adjustments (tenant_id,company_id,adjustment_number,return_id,action,withdraws_adjustment_id,description,recorded_by_user_id,recorded_at) VALUES (1,1,0,{june_id},'withdrawn',{twice},'Again',1,now())"), "finance_sst_return_adjustments_withdraws_unique").await?;
    println!("NS2: open, adjust, withdraw, finalise, snapshot, reproduce, posted-since, amend, discard, overlap, export, evidence, scope, audit and SQL proofs passed");
    Ok(())
}
