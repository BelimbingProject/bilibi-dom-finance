//! A receipt may keep an amount it applies to no invoice (a deposit or an
//! overpayment) as a customer credit: the disposable proof and browser host.
//! Run on a fresh database; `serve` keeps the proved records at
//! APP_ORIGIN/BIND_ADDR for the browser walkthrough afterwards.
use bilibi_agent_api::{Handler, Registry};
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, MutationBoundary};
use bilibi_finance_ledger::{AccountType, Ledger, LedgerError, Money, NewAccount};
use bilibi_finance_receivables::{AgingOptions, ReceivableError, Receivables};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use jiff::civil::date;
use rust_decimal::Decimal;
use serde_json::{json, Value};

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();

fn refused<T: std::fmt::Debug>(answer: Result<T, ReceivableError>, field: &str, words: &str) {
    let refusals: Vec<(String, String)> = match &answer {
        Err(ReceivableError::Invalid(errors)) => errors
            .iter()
            .map(|e| (e.field.to_string(), e.message.to_string()))
            .collect(),
        _ => Vec::new(),
    };
    assert!(
        refusals
            .iter()
            .any(|(f, m)| f == field && m.contains(words)),
        "{answer:?}"
    );
    println!("PASS: refused at {field}: {words}");
}

#[tokio::main]
async fn main() -> Outcome<()> {
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or("http://127.0.0.1:43998".into());
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    let services = &app.services;
    bilibi_finance_walkthrough_fixture::without_step_up(services)?;
    let installed = install(&db, services).await?;
    let admin = sign_in(&db, services, 1, 4).await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    for capability in bilibi_finance_receivables::CAPABILITIES
        .into_iter()
        .chain(bilibi_finance_ledger::CAPABILITIES)
    {
        grants
            .set_direct(&admin, 1, capability, Some(true))
            .await?;
    }
    // The manager records receipts; the administrator is the second person a
    // refund needs; the viewer reads; the other tenant's manager reaches nothing.
    let manager = sign_in(&db, services, 1, 1).await?;
    let checker = sign_in(&db, services, 1, 4).await?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    let other = sign_in(&db, services, 2, 5).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let mutations = services.get::<MutationBoundary>("base/database.mutations")?;
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .expect("fixture account")
            .id
    };
    let (bank, control) = (account("1000"), account("1100"));
    let units = installed.book.minor_units;
    let money = |text: &str| Money::parse(text, units).expect("fixture money");
    let held = ledger
        .create_account(
            &manager,
            1,
            &NewAccount::new("2150", "Customer credits", AccountType::Liability),
        )
        .await?
        .id;
    ar.configure(
        &manager,
        1,
        &json!({"receivable_account_id":control,"income_account_id":account("4000")}),
    )
    .await?;
    let invoices = ar.create_series(&manager,1,&json!({"code":"CC-I","name":"Invoices","kind":"invoice","prefix":"INV-","number_width":5})).await?;
    let receipts = ar.create_series(&manager,1,&json!({"code":"CC-R","name":"Receipts","kind":"payment","prefix":"RCP-","number_width":5})).await?;
    let buyer = ar
        .create_customer(
            &manager,
            1,
            &json!({"record_kind":"company","record_id":10,"payment_terms_days":30}),
        )
        .await?;
    let other_buyer = ar
        .create_customer(
            &manager,
            1,
            &json!({"record_kind":"company","record_id":11,"payment_terms_days":30}),
        )
        .await?;
    let invoice = |customer: i64, day: &str, amount: &str| {
        json!({"kind":"invoice","series_id":invoices.id,"customer_id":customer,"issue_date":day,"lines":[{"description":"Goods","quantity":"1","unit_price":amount}]})
    };
    let receipt = |customer: i64, day: &str, amount: &str, allocations: Value| {
        json!({"series_id":receipts.id,"customer_id":customer,"payment_date":day,"amount":amount,"deposit_account_id":bank,"allocations":allocations})
    };
    let balance = |code: &'static str| {
        let ledger = ledger.clone();
        let manager = manager.clone();
        let id = account(code);
        async move {
            let trial = ledger
                .trial_balance(&manager, 1, date(2026, 12, 31), None)
                .await
                .expect("trial balance");
            assert_eq!(trial.total_debit, trial.total_credit);
            trial
                .rows
                .iter()
                .find(|r| r.account.id == id)
                .map_or(Decimal::ZERO, |r| {
                    r.debit_balance.amount() - r.credit_balance.amount()
                })
        }
    };
    let tied = |day| {
        let ar = ar.clone();
        let manager = manager.clone();
        async move {
            let report = ar
                .tie_out_control_accounts(&manager, 1, day, &Default::default())
                .await
                .expect("tie-out");
            assert!(report.tied_out, "{report:?}");
            report
        }
    };

    // Before the company says where customer credits are held, a receipt
    // still has to be applied in full.
    refused(
        ar.record_payment(&manager, 1, &receipt(buyer.id, "2026-03-01", "500", json!([])))
            .await,
        "allocations",
        "first choose the customer credit account",
    );
    refused(
        ar.configure(&manager, 1, &json!({"customer_credit_account_id":bank}))
            .await,
        "customer_credit_account_id",
        "must be a ledger liability account",
    );
    let setup = ar
        .configure(&manager, 1, &json!({"customer_credit_account_id":held}))
        .await?;
    assert_eq!(setup.customer_credit_account_id, Some(held));
    assert_eq!(
        ar.list_setup_changes(&manager, 1, &Default::default())
            .await?
            .last()
            .map(|c| (c.field.clone(), c.old_value, c.new_value)),
        Some(("customer_credit_account_id".to_owned(), None, held))
    );
    refused(
        ar.configure(&manager, 1, &json!({"customer_credit_account_id":null}))
            .await,
        "customer_credit_account_id",
        "can't be blank once it has been chosen",
    );
    println!("PASS: the customer credit account is a liability account, chosen once and kept in the setup history");

    // 1. A deposit before any invoice: the whole receipt is the credit.
    let deposit = ar
        .record_payment(&manager, 1, &receipt(buyer.id, "2026-03-01", "500", json!([])))
        .await?;
    let kept = deposit.credit.clone().expect("deposit keeps a credit");
    assert!(deposit.allocations.is_empty());
    assert_eq!((kept.amount, kept.outstanding), (money("500"), money("500")));
    let credit = ar.get_document(&manager, 1, kept.document_id).await?;
    assert_eq!(
        (
            credit.kind.as_str(),
            credit.status.as_str(),
            credit.document_reference.as_deref(),
            credit.receivable_account_id,
            credit.ledger_entry_id,
            credit.issue_date.as_deref(),
        ),
        (
            "customer_credit",
            "issued",
            Some(deposit.payment_reference.as_str()),
            Some(held),
            Some(deposit.ledger_entry_id),
            Some("2026-03-01"),
        )
    );
    assert_eq!(balance("1000").await, Decimal::from(500));
    assert_eq!(balance("2150").await, Decimal::from(-500));
    assert_eq!(balance("1100").await, Decimal::ZERO);
    let aging = ar
        .aged_receivables(&manager, 1, date(2026, 3, 31), &AgingOptions::default())
        .await?;
    assert_eq!(
        (aging.total, aging.gross_credits, aging.gross_debits),
        (Decimal::from(-500), Decimal::from(500), Decimal::ZERO)
    );
    assert_eq!(aging.customers[0].credits[0].document_id, credit.id);
    tied(date(2026, 3, 31)).await;
    println!(
        "PASS: deposit {} before any invoice: bank 500, customer credits 500, aging shows a 500 credit and the controls tie out",
        deposit.payment_reference
    );

    // 2. An overpayment: 1,500 against a 1,200 invoice leaves 300 as credit.
    let first = ar
        .issue_document(
            &manager,
            1,
            ar.create_draft(&manager, 1, &invoice(buyer.id, "2026-03-10", "1200"))
                .await?
                .id,
        )
        .await?;
    refused(
        ar.record_payment(
            &manager,
            1,
            &receipt(
                buyer.id,
                "2026-03-15",
                "1000",
                json!([{"document_id":first.id,"amount":"1200"}]),
            ),
        )
        .await,
        "allocations",
        "add up to more than the payment amount",
    );
    let over = ar
        .record_payment(
            &manager,
            1,
            &receipt(
                buyer.id,
                "2026-03-15",
                "1500",
                json!([{"document_id":first.id,"amount":"1200"}]),
            ),
        )
        .await?;
    let over_credit = over.credit.clone().expect("overpayment keeps a credit");
    assert_eq!(over_credit.amount, money("300"));
    assert_eq!(
        ar.get_document(&manager, 1, first.id).await?.outstanding,
        Some(Money::ZERO)
    );
    let entry = ledger.get_entry(&manager, 1, over.ledger_entry_id).await?;
    let mut lines: Vec<(i64, Decimal, Decimal)> = entry
        .lines
        .iter()
        .map(|l| (l.account_id, l.debit.amount(), l.credit.amount()))
        .collect();
    lines.sort();
    let mut expected = vec![
        (bank, Decimal::from(1500), Decimal::ZERO),
        (control, Decimal::ZERO, Decimal::from(1200)),
        (held, Decimal::ZERO, Decimal::from(300)),
    ];
    expected.sort();
    assert_eq!(lines, expected);
    assert_eq!(balance("2150").await, Decimal::from(-800));
    tied(date(2026, 3, 31)).await;
    println!(
        "PASS: overpayment {}: the bank rises 1,500, the receivable falls 1,200 and 300 is owed back to the customer",
        over.payment_reference
    );

    // 3. Apply: the deposit pays a later invoice. The person who recorded the
    // receipt applies its credit; that is an allocation, not an approval.
    let second = ar
        .issue_document(
            &manager,
            1,
            ar.create_draft(&manager, 1, &invoice(buyer.id, "2026-04-01", "600"))
                .await?
                .id,
        )
        .await?;
    let apply = |credit: i64, target: i64, amount: &str, day: &str| {
        json!({"credit_document_id":credit,"target_document_id":target,"amount":amount,"effective_date":day})
    };
    refused(
        ar.apply_credit(&manager, 1, &apply(credit.id, second.id, "500", "2026-02-28"))
            .await,
        "effective_date",
        "cannot precede the date the credit was received",
    );
    refused(
        ar.apply_credit(&manager, 1, &apply(credit.id, second.id, "501", "2026-04-02"))
            .await,
        "amount",
        "is more than the credit has left",
    );
    let exceptions = ledger
        .list_approval_exceptions(&manager, 1, &Default::default())
        .await?
        .len();
    let applied = ar
        .apply_credit(&manager, 1, &apply(credit.id, second.id, "500", "2026-04-02"))
        .await?;
    assert_eq!(applied["reference"], json!(format!("{}/1", deposit.payment_reference)));
    assert!(applied["entry_id"].as_i64().is_some());
    assert_eq!(
        ledger
            .list_approval_exceptions(&manager, 1, &Default::default())
            .await?
            .len(),
        exceptions
    );
    assert_eq!(
        ar.get_document(&manager, 1, second.id).await?.outstanding,
        Some(money("100"))
    );
    assert_eq!(
        ar.get_payment(&manager, 1, deposit.id)
            .await?
            .credit
            .expect("credit")
            .outstanding,
        Money::ZERO
    );
    assert_eq!(balance("2150").await, Decimal::from(-300));
    assert_eq!(balance("1100").await, Decimal::from(100));
    refused(
        ar.apply_credit(
            &manager,
            1,
            &apply(over_credit.document_id, second.id, "200", "2026-04-03"),
        )
        .await,
        "amount",
        "is more than the invoice still owes",
    );
    ar.apply_credit(
        &manager,
        1,
        &apply(over_credit.document_id, second.id, "100", "2026-04-03"),
    )
    .await?;
    assert_eq!(
        ar.get_document(&manager, 1, second.id).await?.outstanding,
        Some(Money::ZERO)
    );
    tied(date(2026, 4, 30)).await;
    // A dated report still reads the credit as it stood before it was used.
    let march = ar
        .aged_receivables(&manager, 1, date(2026, 3, 31), &AgingOptions::default())
        .await?;
    assert_eq!(march.gross_credits, Decimal::from(800));
    println!("PASS: credit applied to a later invoice moves the amount from customer credits to the receivable; over-application is refused; March still reads 800 of credit");

    // 4. Refund: money out clears what is left. Whoever recorded the receipt
    // cannot also refund it while the company requires separation of duties.
    let refund = json!({"credit_document_id":over_credit.document_id,"cash_account_id":bank,"amount":"200","effective_date":"2026-04-10"});
    assert!(matches!(
        ar.refund_credit(&manager, 1, &refund).await,
        Err(ReceivableError::Ledger(LedgerError::SamePerson))
    ));
    refused(
        ar.refund_credit(
            &checker,
            1,
            &json!({"credit_document_id":over_credit.document_id,"cash_account_id":control,"amount":"200","effective_date":"2026-04-10"}),
        )
        .await,
        "cash_account_id",
        "must be an active posting asset account",
    );
    let refunded = ar.refund_credit(&checker, 1, &refund).await?;
    let entry = ledger
        .get_entry(&manager, 1, refunded["entry_id"].as_i64().expect("refund entry"))
        .await?;
    let mut lines: Vec<(i64, Decimal, Decimal)> = entry
        .lines
        .iter()
        .map(|l| (l.account_id, l.debit.amount(), l.credit.amount()))
        .collect();
    lines.sort();
    let mut expected = vec![
        (bank, Decimal::ZERO, Decimal::from(200)),
        (held, Decimal::from(200), Decimal::ZERO),
    ];
    expected.sort();
    assert_eq!(lines, expected);
    assert_eq!(balance("2150").await, Decimal::ZERO);
    assert_eq!(balance("1000").await, Decimal::from(1800));
    refused(
        ar.refund_credit(&checker, 1, &refund).await,
        "amount",
        "is more than the credit has left",
    );
    tied(date(2026, 4, 30)).await;
    println!("PASS: refund by a second person takes 200 out of the bank and clears the credit; the recorder is refused, and a spent credit cannot be refunded again");

    // 5. Two people apply the same credit at once: one wins, the credit never
    // goes below zero.
    let racing = ar
        .record_payment(&manager, 1, &receipt(other_buyer.id, "2026-05-01", "400", json!([])))
        .await?;
    let racing_credit = racing.credit.clone().expect("credit").document_id;
    let mut targets = vec![];
    for _ in 0..2 {
        targets.push(
            ar.issue_document(
                &manager,
                1,
                ar.create_draft(&manager, 1, &invoice(other_buyer.id, "2026-05-02", "400"))
                    .await?
                    .id,
            )
            .await?
            .id,
        );
    }
    let (left, right) = tokio::join!(
        ar.apply_credit(&manager, 1, &apply(racing_credit, targets[0], "400", "2026-05-03")),
        ar.apply_credit(&checker, 1, &apply(racing_credit, targets[1], "400", "2026-05-03")),
    );
    assert_eq!(left.is_ok() as u8 + right.is_ok() as u8, 1, "{left:?} {right:?}");
    refused(
        if left.is_err() { left } else { right },
        "amount",
        "is more than the credit has left",
    );
    assert_eq!(
        ar.get_document(&manager, 1, racing_credit).await?.outstanding,
        Some(Money::ZERO)
    );
    let client = db.get().await?;
    // The database refuses an over-application even past the facade.
    let direct = client
        .execute(
            "INSERT INTO finance_receivables_settlements(tenant_id,company_id,settlement_number,kind,credit_document_id,target_document_id,amount,effective_date,reference,review,actor_user_id,audit_trace_id,audit_action) SELECT 1,1,max(settlement_number)+1,'apply',$1,$2,1,'2026-05-04','direct','{}',1,'t','a' FROM finance_receivables_settlements WHERE company_id=1",
            &[&racing_credit, &targets[1]],
        )
        .await;
    assert!(direct
        .expect_err("the database refuses it")
        .as_db_error()
        .is_some_and(|e| e.message().contains("uses more than credit")));
    drop(client);
    tied(date(2026, 5, 31)).await;
    println!("PASS: concurrent applications of one credit: exactly one succeeds, the other is refused, and the database refuses a direct over-application");

    // 6. Reversal. A receipt whose credit has been used is reversed only
    // after that use is; then its credit goes with it.
    let winner = ar.get_document(&manager, 1, racing_credit).await?.settlements[0].clone();
    refused(
        ar.reverse_payment(
            &manager,
            1,
            racing.id,
            &json!({"reversal_date":"2026-05-10","reason":"Recorded twice"}),
        )
        .await,
        "base",
        "has since been applied or refunded",
    );
    ar.reverse_settlement(
        &manager,
        1,
        &json!({"settlement_id":winner["id"],"effective_date":"2026-05-09"}),
    )
    .await?;
    refused(
        ar.reverse_payment(
            &manager,
            1,
            racing.id,
            &json!({"reversal_date":"2026-05-05","reason":"Recorded twice"}),
        )
        .await,
        "base",
        "has since been applied or refunded",
    );
    let reversed = ar
        .reverse_payment(
            &manager,
            1,
            racing.id,
            &json!({"reversal_date":"2026-05-10","reason":"Recorded twice"}),
        )
        .await?;
    assert_eq!(reversed.credit.expect("credit").outstanding, Money::ZERO);
    refused(
        ar.apply_credit(&manager, 1, &apply(racing_credit, targets[0], "400", "2026-05-11"))
            .await,
        "credit_document_id",
        "is the credit of a reversed receipt",
    );
    let before = ar
        .aged_receivables(&manager, 1, date(2026, 5, 9), &AgingOptions::default())
        .await?;
    let after = ar
        .aged_receivables(&manager, 1, date(2026, 5, 31), &AgingOptions::default())
        .await?;
    let other_row = |report: &bilibi_finance_receivables::AgedReceivables| {
        report
            .customers
            .iter()
            .find(|c| c.customer_id == other_buyer.id)
            .map(|c| (c.credit_total, c.total))
            .expect("customer row")
    };
    assert_eq!(other_row(&before), (Decimal::from(400), Decimal::from(400)));
    assert_eq!(other_row(&after), (Decimal::ZERO, Decimal::from(800)));
    tied(date(2026, 5, 9)).await;
    tied(date(2026, 5, 31)).await;
    assert_eq!(balance("2150").await, Decimal::ZERO);
    println!("PASS: a receipt with a used credit is refused reversal until the application is reversed; reversing it removes the credit from its date on");

    // 7. The agent doors: the same review a person confirms, then one commit.
    let write = |key: &'static str| {
        let Handler::SafeWrite(handler) = registry.operations[key].definition.handler else {
            panic!("{key} is not a write")
        };
        handler
    };
    let mut raw = db.get().await?;
    let record_key = "finance.receivables.payment.record";
    let mut input = receipt(
        other_buyer.id,
        "2026-06-01",
        "1000",
        json!([{"document_id":targets[0],"amount":"400"}]),
    );
    input["company_id"] = json!(1);
    let mut tx = mutations
        .begin(&mut raw, manager.mutation_context("walkthrough.credit.receipt"))
        .await?;
    let review = write(record_key)
        .preview(tx.sql(), &manager, &input)
        .await?
        .envelope(record_key);
    let decimal = |value: &Value| {
        value
            .as_str()
            .and_then(|text| text.parse::<Decimal>().ok())
            .expect("decimal text")
    };
    let kept = &review["result"]["changes"]["kept_as_credit"];
    assert_eq!(
        (decimal(&kept["amount"]), kept["account_id"].as_i64()),
        (Decimal::from(600), Some(held))
    );
    let answer = write(record_key)
        .commit(&mut tx, &manager, &input)
        .await?
        .envelope(record_key);
    tx.commit().await?;
    let agent_credit = answer["result"]["record"]["credit"]["document_id"]
        .as_i64()
        .expect("agent credit");
    let list_key = "finance.receivables.credit.list";
    let Handler::Read(list) = registry.operations[list_key].definition.handler else {
        panic!("credit list must be a read")
    };
    let offered = list
        .call(&**raw, &manager, &json!({"company_id":1,"customer_id":other_buyer.id}))
        .await?
        .envelope(list_key);
    assert_eq!(decimal(&offered["result"]["available"]), Decimal::from(600));
    assert_eq!(offered["result"]["credits"][0]["id"], agent_credit);
    for (key, mut input, scope) in [
        (
            "finance.receivables.credit.apply",
            apply(agent_credit, targets[1], "400", "2026-06-02"),
            &manager,
        ),
        (
            "finance.receivables.credit.refund",
            json!({"credit_document_id":agent_credit,"cash_account_id":bank,"amount":"200","effective_date":"2026-06-03"}),
            &checker,
        ),
    ] {
        input["company_id"] = json!(1);
        let mut tx = mutations
            .begin(&mut raw, scope.mutation_context("walkthrough.credit.settle"))
            .await?;
        let review = write(key).preview(tx.sql(), scope, &input).await?.envelope(key);
        let changes = &review["result"]["changes"];
        assert_eq!(changes["credit"]["receipt"]["id"], answer["result"]["record"]["id"]);
        assert!(changes["posts"] == true && !changes["posting"].is_null());
        let committed = write(key).commit(&mut tx, scope, &input).await?.envelope(key);
        tx.commit().await?;
        assert_eq!(
            committed["result"]["path"],
            json!(format!("/finance/receivables/invoices/{agent_credit}"))
        );
    }
    drop(raw);
    assert_eq!(
        ar.get_document(&manager, 1, agent_credit).await?.outstanding,
        Some(Money::ZERO)
    );
    tied(date(2026, 6, 30)).await;
    println!("PASS: payment.record, credit.apply and credit.refund preview the kept credit, its receipt and its posting, then commit once");

    // 8. Reach: a reader cannot settle, and another tenant sees nothing.
    assert!(matches!(
        ar.apply_credit(&viewer, 1, &apply(agent_credit, targets[1], "1", "2026-06-04"))
            .await,
        Err(ReceivableError::Unauthorized)
    ));
    assert!(ar
        .apply_credit(&other, 1, &apply(agent_credit, targets[1], "1", "2026-06-04"))
        .await
        .is_err());
    assert!(ar.get_payment(&other, 1, deposit.id).await.is_err());
    println!("PASS: a reader cannot apply or refund credit, and another tenant reaches neither the receipt nor its credit");

    // 9. Evidence and audit: every kept credit, its document and each
    // settlement is an audited row an auditor can export.
    let evidence = ar
        .evidence_page(&manager, 1, "receipt_credit", Some(500), None)
        .await?;
    assert_eq!(evidence.rows.len(), 4);
    let client = db.get().await?;
    let unaudited: i64 = client.query_one("SELECT count(*) FROM (SELECT 'finance.receivables.receipt_credit' kind,id FROM finance_receivables_receipt_credits UNION ALL SELECT 'finance.receivables.settlement',id FROM finance_receivables_settlements UNION ALL SELECT 'finance.receivables.document',id FROM finance_receivables_documents WHERE kind='customer_credit') r WHERE NOT EXISTS(SELECT 1 FROM base_audit_mutations m WHERE m.tenant_id=1 AND m.auditable_type=r.kind AND m.auditable_id=r.id::text)",&[]).await?.get(0);
    assert_eq!(unaudited, 0);
    // The receipt and its credit commit together or not at all.
    assert!(client
        .execute(
            "INSERT INTO finance_receivables_receipt_credits(tenant_id,company_id,payment_id,document_id,amount) VALUES(1,1,$1,$2,1)",
            &[&deposit.id, &first.id],
        )
        .await
        .is_err());
    assert!(client
        .execute(
            "UPDATE finance_receivables_receipt_credits SET amount=amount+1 WHERE payment_id=$1",
            &[&deposit.id],
        )
        .await
        .is_err());
    drop(client);
    println!("PASS: the receipt_credit evidence dataset exports every kept credit, each row is audited, and the link is append-only");

    // What the browser walkthrough starts from: a customer with an open
    // invoice and credit left to apply or refund.
    let open = ar
        .issue_document(
            &manager,
            1,
            ar.create_draft(&manager, 1, &invoice(buyer.id, "2026-06-10", "1200"))
                .await?
                .id,
        )
        .await?;
    let browser = ar
        .record_payment(&manager, 1, &receipt(buyer.id, "2026-06-12", "350", json!([])))
        .await?;
    println!(
        "Browser: {origin}/finance/receivables/receipts ; manager@example.test records, admin@example.test refunds; walkthrough password. Invoice {} owes 1,200.00; credit {} has 350.00 left.",
        open.document_reference.unwrap_or_default(),
        browser.payment_reference
    );
    if std::env::args().any(|a| a == "serve") {
        axum::serve(
            tokio::net::TcpListener::bind(
                std::env::var("BIND_ADDR").unwrap_or("127.0.0.1:43998".into()),
            )
            .await?,
            bilibi_web::router(app),
        )
        .await?;
    }
    Ok(())
}
