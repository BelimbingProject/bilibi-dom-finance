//! Disposable opening-state conversion: browser first on Company 001, MCP on
//! clean Company 002, then facade controls on Company 003. No product seeds.
#[path = "../conversion_credits.rs"]
mod conversion_credits;
#[path = "../conversion_lifecycle.rs"]
mod conversion_lifecycle;
#[path = "../conversion_report.rs"]
mod conversion_report;
use bilibi_agent_api::{Handler, Registry};
use bilibi_authz::{RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{pool, Db};
use bilibi_finance_fixed_assets::{
    Assumptions, DisposalInput, EventInput, FixedAssets, Method, ReportPins, RevisionInput,
};
use bilibi_finance_ledger::{
    AccountType, Ledger, Money, NewAccount, NewBook, NewFiscalYear, OpeningBalances, Reversal,
};
use bilibi_finance_payables::{
    DocumentLineInput, NewDocument, Payables, PayablesError, Settings, SettingsInput, VendorInput,
};
use bilibi_finance_receivables::{AgingOptions, Receivables};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use bilibi_module_registry::{Contribution, Provenance, Services};
use jiff::civil::date;
use serde_json::{json, Value};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();
macro_rules! roles {($key:literal,$module:literal,$caps:path)=>{inventory::submit!{Contribution::new(VOCABULARY,$key,Provenance{module:$module,source:file!(),line:line!()},&VocabularyContribution{domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[RoleDefinition{code:"manager",name:"manager",description:"Disposable conversion fixture",grant_all:false,capabilities:&$caps}]})}};}
roles!(
    "walkthrough.conversion.receivables",
    "finance/receivables",
    bilibi_finance_receivables::CAPABILITIES
);
roles!(
    "walkthrough.conversion.assets",
    "finance/fixed_assets",
    bilibi_finance_fixed_assets::CAPABILITIES
);
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
const SOURCE: &str = "source_id,account_code,debit,credit\nbank,1000,10000,\nreceivables,1100,3000,\nasset-cost,1200,12000,\nytd-cost,5000,2000,\npayables,2000,,1500\naccumulation,1290,,1800\ncapital,3000,,18200\nytd-sales,4000,,5500\n";
const AR: &str = "source_id,customer_code,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\ninvoice-17,company_010,OLD-INV-17,2026-05-15,2026-06-30,5000,3000,1100,company,invoice,invoice,source-system,MYR,unknown\n";
const AP: &str = "source_id,vendor_code,vendor_name,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\nbill-29,company_010,Conversion vendor,OLD-BILL-29,2026-05-01,2026-06-15,2500,1500,2000,company,bill,bill,source-system,MYR,unknown\n";
const MULTI_SOURCE: &str = "source_id,account_code,debit,credit\nbank,1000,10000,\nreceivables,1100,2500,\nhistorical-receivables,1110,500,\nasset-cost,1200,12000,\nytd-cost,5000,2000,\npayables,2000,,1200\nhistorical-payables,2010,,300\naccumulation,1290,,1800\ncapital,3000,,18200\nytd-sales,4000,,5500\n";
const MULTI_AR: &str = "source_id,customer_code,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\ninvoice-17,company_010,OLD-INV-17,2026-05-15,2026-06-30,5000,2500,1100,company,invoice,invoice,source-system,MYR,unknown\ninvoice-18,company_010,OLD-INV-18,2026-04-15,2026-05-31,1000,500,1110,company,invoice,invoice,source-system,MYR,unknown\n";
const MULTI_AP: &str = "source_id,vendor_code,vendor_name,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\nbill-29,company_010,Conversion vendor,OLD-BILL-29,2026-05-01,2026-06-15,2500,1200,2000,company,bill,bill,source-system,MYR,unknown\nbill-30,company_010,Conversion vendor,OLD-BILL-30,2026-04-01,2026-05-15,900,300,2010,company,bill,bill,source-system,MYR,unknown\n";
async fn in_company(
    db: &Db,
    services: &Services,
    user: i64,
    company: i64,
) -> Outcome<bilibi_tenancy::Scope> {
    let authentication =
        services.get::<bilibi_tenancy::Authentication>("base/tenancy.authentication")?;
    let c = db.get().await?;
    Ok(authentication
        .authenticate_session(&**c, 1, user, None, Some(company), String::new())
        .await?)
}
async fn setup(db: &Db, services: &Services, company: i64) -> Outcome<Value> {
    let admin = sign_in(db, services, 1, 4).await?;
    // Workplace reach is explicit on the pinned Platform. Capability grants
    // alone do not let a fixture preparer or reviewer work in another company.
    let users = services.get::<bilibi_core::user::UserService>("core/user.accounts")?;
    for person in [1, 4, 5, 6] {
        let assigned = db
            .get()
            .await?
            .query_opt(
                "SELECT id FROM user_desks WHERE tenant_id=1 AND company_id=$1 AND person_id=$2",
                &[&company, &person],
            )
            .await?
            .is_some();
        if !assigned {
            users.set_desk(&admin, company, person, true).await?;
        }
    }
    let admin = in_company(db, services, 4, company).await?;
    let manager = in_company(db, services, 1, company).await?;
    let credit_controls = matches!(company, 6 | 8 | 9);
    let multiple_controls = matches!(company, 4 | 6 | 8 | 9);
    let l = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let assets = services.get::<FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    if l.get_book(&admin, company).await.is_err() {
        l.open_book(
            &admin,
            company,
            &NewBook {
                currency_code: "MYR".into(),
                minor_units: 2,
            },
        )
        .await?;
    }
    let chart = l.list_accounts(&admin, company).await?;
    for (code, name, kind, retained) in bilibi_finance_walkthrough_fixture::CHART {
        if !chart.iter().any(|a| a.code == code) {
            let mut new = NewAccount::new(code, name, kind);
            new.retained_earnings = retained;
            l.create_account(&admin, company, &new).await?;
        }
    }
    let chart = l.list_accounts(&admin, company).await?;
    for (code, name, kind) in [
        ("1200", "Asset cost", AccountType::Asset),
        ("1290", "Accumulated depreciation", AccountType::Asset),
        ("6100", "Depreciation expense", AccountType::Expense),
        ("4100", "Asset disposal gain", AccountType::Income),
    ] {
        if !chart.iter().any(|a| a.code == code) {
            l.create_account(&admin, company, &NewAccount::new(code, name, kind))
                .await?;
        }
    }
    if multiple_controls {
        for (code, name, kind) in [
            ("1110", "Historical receivables", AccountType::Asset),
            ("2010", "Historical payables", AccountType::Liability),
        ] {
            if !chart.iter().any(|a| a.code == code) {
                l.create_account(&admin, company, &NewAccount::new(code, name, kind))
                    .await?;
            }
        }
    }
    if credit_controls {
        for (code, name, kind) in [
            ("1120", "Zero receivables control", AccountType::Asset),
            ("2020", "Zero payables control", AccountType::Liability),
        ] {
            l.create_account(&admin, company, &NewAccount::new(code, name, kind))
                .await?;
        }
    }
    if l.list_fiscal_years(&admin, company).await?.is_empty() {
        l.open_fiscal_year(
            &admin,
            company,
            &NewFiscalYear {
                start_date: Some(date(2026, 1, 1)),
                end_date: None,
            },
        )
        .await?;
    }
    let chart = l.list_accounts(&admin, company).await?;
    let account = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .expect("fixture account")
            .id
    };
    ar.configure(
        &admin,
        company,
        &json!({"receivable_account_id":account("1100"),"income_account_id":account("4000")}),
    )
    .await?;
    let series = ar.list_series(&admin, company).await?;
    let invoice = if let Some(s) = series.iter().find(|s| s.code == "OPENING") {
        s.clone()
    } else {
        ar.create_series(&admin,company,&json!({"code":"OPENING","name":"Opening invoices","kind":"invoice","prefix":"OPEN-","number_width":4})).await?
    };
    let payment = if let Some(s) = series.iter().find(|s| s.code == "RECEIPT") {
        s.clone()
    } else {
        ar.create_series(&admin,company,&json!({"code":"RECEIPT","name":"Receipts","kind":"payment","prefix":"REC-","number_width":4})).await?
    };
    let customers = ar.list_customers(&admin, company).await?;
    let customer = if let Some(c) = customers.iter().find(|c| c.code == "company_010") {
        c.clone()
    } else {
        ar.create_customer(
            &admin,
            company,
            &json!({"record_kind":"company","record_id":10,"payment_terms_days":30}),
        )
        .await?
    };
    ap.put_settings(
        &admin,
        company,
        &SettingsInput {
            settings: Settings {
                payable_account_id: account("2000"),
                quantity_tolerance: None,
                price_tolerance: None,
            },
            reason: Some("Conversion setup".into()),
        },
    )
    .await?;
    let vendors = ap.list_vendors(&admin, company, 0, 500).await?;
    let vendor = if let Some(vendor) = vendors.iter().find(|v| v.code == "company_010") {
        vendor.clone()
    } else {
        ap.create_vendor(
            &admin,
            company,
            &VendorInput {
                record_kind: Some("company".into()),
                record_id: Some(10),
                new_company: None,
                new_person: None,
                payment_terms_days: 30,
                payable_account_id: None,
                expense_account_id: None,
                tax_code_id: None,
                active: true,
            },
        )
        .await?
    };
    if credit_controls {
        ar.create_customer(
            &admin,
            company,
            &json!({"record_kind":"company","record_id":11,"payment_terms_days":30}),
        )
        .await?;
        ap.create_vendor(
            &admin,
            company,
            &VendorInput {
                record_kind: Some("company".into()),
                record_id: Some(11),
                new_company: None,
                new_person: None,
                payment_terms_days: 30,
                payable_account_id: None,
                expense_account_id: None,
                tax_code_id: None,
                active: true,
            },
        )
        .await?;
    }
    let categories = assets.list_categories(&admin, company).await?;
    let revision = if let Some(c) = categories.iter().find(|c| c.name == "Conversion assets") {
        c.revision.id
    } else {
        assets
            .create_category(
                &admin,
                company,
                "Conversion assets",
                &RevisionInput {
                    cost_account_id: account("1200"),
                    accumulated_account_id: account("1290"),
                    expense_account_id: account("6100"),
                    gain_account_id: account("4100"),
                    loss_account_id: account("6100"),
                    assumptions: Assumptions {
                        method: Method::StraightLine,
                        life_months: 120,
                        annual_rate: None,
                    },
                    reason: "Source assumptions; opening import uses remaining life".into(),
                },
            )
            .await?
            .id
    };
    bilibi_finance_walkthrough_fixture::initialize_journal_approvals(
        db, services, &manager, company,
    )
    .await?;
    if l.opening_context_on(&**db.get().await?, &admin, company)
        .await
        .is_err()
    {
        let j = l
            .submit_opening_balances(
                &manager,
                company,
                &OpeningBalances {
                    conversion_date: date(2026, 7, 1),
                    currency_code: "MYR".into(),
                    source_reference: format!("CONVERSION-{company}"),
                    csv: if credit_controls {
                        conversion_credits::SOURCE
                    } else if company == 7 {
                        conversion_credits::CREDIT_ONLY_SOURCE
                    } else if multiple_controls {
                        MULTI_SOURCE
                    } else {
                        SOURCE
                    }
                    .into(),
                    replaces_journal_id: None,
                },
            )
            .await?;
        l.approve_journal(&admin, company, j.id).await?;
    }
    let reconciliation = services.get::<bilibi_finance_reconciliation::Reconciliation>(
        bilibi_finance_reconciliation::SERVICE,
    )?;
    let bank_account = match reconciliation.list_bank_accounts(&admin, company).await?.into_iter().find(|a| a.ledger_account_id == account("1000")) {
        Some(a) => a,
        None => reconciliation.create_bank_account(&admin, company, &bilibi_finance_reconciliation::NewBankAccount {
            name: "Conversion bank".into(), ledger_account_id: account("1000"),
            import_mapping: json!({"date":"Date","description":"Details","reference":"Ref","amount":"Amount","balance":"Balance"}),
            charges_account_id: None, interest_account_id: None, match_window_days: None, active: None, reason: None,
        }).await?,
    };
    let asset_csv = format!(
        "source_id,description,category_revision_id,acquisition_date,in_service_date,cost,accumulated_depreciation,remaining_life_months,residual_value\nasset-3,Source register asset,{revision},2024-06-01,2024-07-01,12000,1800,96,0\n"
    );
    let mut classifications = serde_json::Map::new();
    for (code, classification) in [
        ("1000", "not_a_control"),
        ("1100", "receivables_control"),
        ("1200", "not_a_control"),
        ("1290", "not_a_control"),
        ("2000", "payables_control"),
    ] {
        classifications.insert(account(code).to_string(), json!(classification));
    }
    if multiple_controls {
        classifications.insert(account("1110").to_string(), json!("receivables_control"));
        classifications.insert(account("2010").to_string(), json!("payables_control"));
    }
    if credit_controls {
        classifications.insert(account("1120").to_string(), json!("receivables_control"));
        classifications.insert(account("2020").to_string(), json!("payables_control"));
    }
    Ok(
        json!({"company_id":company,"vendor_id":vendor.id,"vendor_key":format!("vendor:{}", vendor.id),"customer_id":customer.id,"invoice_series":invoice.id,"payment_series":payment.id,"bank":account("1000"),"bank_account_id":bank_account.id,"ar":if credit_controls { conversion_credits::csv(false,false) } else if multiple_controls { MULTI_AR.into() } else { AR.into() },"ap":if credit_controls { conversion_credits::csv(true,false) } else if multiple_controls { MULTI_AP.into() } else { AP.into() },"expense":account("5000"),"assets":asset_csv,"revision":revision,"opening_account_classifications":classifications,"historical_ar":if multiple_controls { json!(account("1110")) } else { Value::Null },"historical_ap":if multiple_controls { json!(account("2010")) } else { Value::Null }}),
    )
}
async fn prove_opening_invoice_page_scale(db: &Db, services: &Services) -> Outcome<()> {
    const HEADER: &str = "source_id,customer_code,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\n";
    let company = 5;
    let config = setup(db, services, company).await?;
    let admin = in_company(db, services, 4, company).await?;
    let manager = in_company(db, services, 1, company).await?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let mut rows = vec![
        "scale-nonmatching,company_010,OUTSIDE-QUERY,2026-05-15,2026-06-30,0.50,0.50,1100,company,invoice,invoice,source-system,MYR,unknown"
            .to_owned(),
    ];
    for n in 1..=2100 {
        let amount = if n == 2100 { "0.50" } else { "1" };
        rows.push(format!("scale-{n},company_010,SCALE-ROW-{n},2026-05-15,2026-06-30,{amount},{amount},1100,company,invoice,invoice,source-system,MYR,unknown"));
    }
    rows.push(
        "scale-0,company_010,SCALE-ROW-0,2026-05-15,2026-06-30,900,900,1100,company,invoice,invoice,source-system,MYR,unknown".to_owned(),
    );
    let part_count = rows.len().div_ceil(500);
    for (index, chunk) in rows.chunks(500).enumerate() {
        ar.stage_opening_import(
            &manager,
            company,
            &json!({
                "source_reference":"AR-SCALE",
                "currency_code":"MYR",
                "series_id":config["invoice_series"],
                "csv":format!("{HEADER}{}\n", chunk.join("\n")),
                "opening_account_classifications":config["opening_account_classifications"],
                "part":index + 1,
                "part_count":part_count,
            }),
        )
        .await?;
    }
    let reconciliation = json!({
        "source_reference":"AR-SCALE",
        "currency_code":"MYR",
        "series_id":config["invoice_series"],
        "csv":"",
        "opening_account_classifications":config["opening_account_classifications"],
        "staged":true,
    });
    assert!(ar
        .import_opening_invoices(&manager, company, &reconciliation)
        .await
        .is_err());
    ar.import_opening_invoices(&admin, company, &reconciliation)
        .await?;
    let ordinary = ar
        .create_draft(
            &manager,
            company,
            &json!({
                "kind":"invoice",
                "series_id":config["invoice_series"],
                "customer_id":config["customer_id"],
                "issue_date":"2026-07-02",
                "description":"SCALE-ROW ordinary invoice",
                "lines":[{"description":"SCALE-ROW ordinary invoice","quantity":"1","unit_price":"1"}],
            }),
        )
        .await?;
    ar.issue_document(&manager, company, ordinary.id).await?;
    let current_draft = ar
        .create_draft(
            &manager,
            company,
            &json!({
                "kind":"invoice",
                "series_id":config["invoice_series"],
                "customer_id":config["customer_id"],
                "description":"Current customer search proof",
            }),
        )
        .await?;
    let client = db.get().await?;
    let page = registry
        .call(
            &**client,
            &admin,
            "finance.receivables.document.page",
            &json!({"company_id":company,"source":"opening","search":"SCALE-ROW","sort":"total","direction":"asc","page":999,"page_size":25}),
        )
        .await?;
    assert_eq!(page["result"]["total"], 2101);
    assert_eq!(page["result"]["page"], 85);
    let documents = page["result"]["documents"]
        .as_array()
        .ok_or("opening invoice page rows missing")?;
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["description"], "Opening invoice SCALE-ROW-0");
    assert_eq!(documents[0]["document_reference"], "OPEN-2102");
    assert_eq!(documents[0]["total"], "900");
    let snapshot_page = registry
        .call(
            &**client,
            &admin,
            "finance.receivables.document.page",
            &json!({"company_id":company,"source":"opening","search":"company_010","page":1,"page_size":25}),
        )
        .await?;
    assert_eq!(snapshot_page["result"]["total"], 2102);
    assert!(snapshot_page["result"]["documents"]
        .as_array()
        .is_some_and(|rows| rows.iter().all(|row| row["party_code"] == "company_010")));
    let current_page = registry
        .call(
            &**client,
            &admin,
            "finance.receivables.document.page",
            &json!({"company_id":company,"source":"ordinary","status":"draft","search":"company_010","page":1,"page_size":25}),
        )
        .await?;
    assert_eq!(current_page["result"]["total"], 1);
    assert_eq!(
        current_page["result"]["documents"][0]["id"],
        current_draft.id
    );
    println!("Opening invoice scale proof: staged reconciliation, SQL source/search filtering, sorting, count and clamping passed.");
    Ok(())
}
async fn prove(db: &Db, services: &Services) -> Outcome<()> {
    let company = 3;
    let config = setup(db, services, company).await?;
    let s = in_company(db, services, 4, company).await?;
    let manager = in_company(db, services, 1, company).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let l = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let assets = services.get::<FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    let trial = l.trial_balance(&s, company, date(2026, 7, 1), None).await?;
    assert_eq!(trial.total_debit.shown(trial.minor_units), "27000.00");
    let before_ar = ar
        .aged_receivables(&s, company, date(2026, 7, 1), &Default::default())
        .await?;
    let before_ap = ap
        .aged_payables(&s, company, date(2026, 7, 1), &Default::default())
        .await?;
    let before_ar_pins = before_ar.pins();
    let before_ap_pins = before_ap.pins();
    let before_ar_tie = ar
        .tie_out_control_accounts(&s, company, date(2026, 7, 1), &Default::default())
        .await?;
    let before_ap_tie = ap
        .tie_out_control_accounts(&s, company, date(2026, 7, 1), &Default::default())
        .await?;
    let before_assets = assets
        .register_report(&s, company, date(2026, 7, 1), &Default::default())
        .await?;
    let ar_input = json!({"source_reference":"AR-SOURCE","currency_code":"MYR","series_id":config["invoice_series"],"csv":AR,"opening_account_classifications":config["opening_account_classifications"]});
    let ap_input = json!({"source_reference":"AP-SOURCE","currency_code":"MYR","csv":AP,"opening_account_classifications":config["opening_account_classifications"]});
    let defaults = assets.list_categories(&s, company).await?[0]
        .revision
        .defaults
        .clone();
    let zero_class = assets
        .create_category(&s, company, "Fully depreciated source class", &defaults)
        .await?;
    let mut asset_input = json!({"source_reference":"ASSET-SOURCE","currency_code":"MYR","csv":config["assets"].as_str().unwrap().replace(",12000,1800,96,0", ",11000,800,96,0") + &format!("asset-4,Fully depreciated,{revision},2024-06-01,2024-07-01,1000,1000,0,0\n", revision=zero_class.id)});
    asset_input["source_evidence"] = json!([{"source_id":"asset-3",
        "history":[
            {"kind":"impairment","effective_date":"2025-12-31","amount":"200.00","reference":"Source impairment","note":"Historical source assessment"},
            {"kind":"impairment_reversal","effective_date":"2026-01-31","amount":"200.00","reference":"Source reversal","note":"Source assessment reversed before cut-off"},
            {"kind":"disposal","effective_date":"2026-02-28","amount":"1000.00","reference":"Source partial disposal","note":"CSV cost is the remaining source asset"},
            {"kind":"residual_value_change","effective_date":"2026-06-30","residual_value":"0.00","reference":"Source estimate","note":"Residual reassessed before cut-off"},
            {"kind":"estimate_change","effective_date":"2026-06-30","remaining_life_months":96,"method":"Source method","reference":"Source estimate","note":"CSV freezes the current prospective estimate"}
        ],
        "source_period_expense":[{"from":"2026-01-01","through":"2026-06-30","amount":"600.00","reference":"Source YTD bridge"}],
        "tax_capital_allowances":[{"from":"2025-01-01","through":"2025-12-31","qualifying_expenditure":"12000.00","allowances":"2400.00","reference":"Source tax computation"}]
    }]);
    for bad_evidence in [
        json!([{"source_id":"unknown"}]),
        json!([{"source_id":"asset-3","unknown":true}]),
        json!([{"source_id":"asset-3","history":[{"kind":"impairment","effective_date":"2026-07-01","amount":"1.00","reference":"Future","note":"Refused"}]}]),
        json!([{"source_id":"asset-3","history":[{"kind":"impairment","effective_date":"2026-06-30","amount":"1.001","reference":"Precision","note":"Refused"}]}]),
        json!([{"source_id":"asset-3","history":[{"kind":"residual_value_change","effective_date":"2026-06-30","reference":"Missing estimate","note":"Refused"}]}]),
        json!([{"source_id":"asset-3","source_period_expense":[{"from":"2026-06-30","through":"2026-01-01","amount":"1","reference":"Backwards period"}]}]),
        json!([{"source_id":"asset-3","tax_capital_allowances":[{"from":"2025-01-01","through":"2025-12-31","qualifying_expenditure":"1","allowances":"-1","reference":"Negative tax allowance"}]}]),
    ] {
        let mut wrong = asset_input.clone();
        wrong["source_evidence"] = bad_evidence;
        let refusal = assets
            .import_opening_register(&s, company, &wrong)
            .await
            .unwrap_err();
        assert!(
            matches!(refusal, bilibi_finance_fixed_assets::FixedAssetsError::Invalid(ref fields) if fields[0].field == "source_evidence"),
            "{refusal:?}"
        );
    }
    for input in [&ar_input, &ap_input, &asset_input] {
        assert!(input["csv"].as_str().is_some());
    }
    let mut wrong = ar_input.clone();
    wrong["csv"] = json!(AR.replace(",3000,1100", ",3001,1100"));
    assert!(ar
        .import_opening_invoices(&s, company, &wrong)
        .await
        .is_err());
    let mut wrong_ap = ap_input.clone();
    wrong_ap["csv"] = json!(AP.replace(",1500,2000", ",1501,2000"));
    assert!(ap
        .import_opening_bills(&s, company, &wrong_ap)
        .await
        .is_err());
    let mut wrong_asset = asset_input.clone();
    wrong_asset["csv"] = json!(asset_input["csv"]
        .as_str()
        .unwrap()
        .replace(",1000,1000,0,0", ",1000,999,0,0"));
    assert!(assets
        .import_opening_register(&s, company, &wrong_asset)
        .await
        .is_err());
    assert!(ar
        .import_opening_invoices(&manager, company, &ar_input)
        .await
        .is_err());
    assert!(ar
        .import_opening_invoices(&viewer, company, &ar_input)
        .await
        .is_err());
    // A blank, unknown or wrong-kind Core code is a refusal of its row, never a
    // match to a customer or supplier whose Core party cannot be read.
    for (csv, words) in [
        (
            AR.replace("invoice-17,company_010", "invoice-17,"),
            "customer_code is required",
        ),
        (
            AR.replace("company_010", "MISSING-CUSTOMER"),
            "customer_code and party_kind",
        ),
        (
            AR.replace(",company,", ",person,"),
            "customer_code and party_kind",
        ),
    ] {
        let mut bad = ar_input.clone();
        bad["csv"] = json!(csv);
        let refusal = format!(
            "{:?}",
            ar.import_opening_invoices(&s, company, &bad)
                .await
                .unwrap_err()
        );
        assert!(refusal.contains(words), "{refusal}");
    }
    for csv in [
        AP.replace("bill-29,company_010", "bill-29,"),
        AP.replace(",company,", ",person,"),
    ] {
        let mut bad = ap_input.clone();
        bad["csv"] = json!(csv);
        assert!(ap.import_opening_bills(&s, company, &bad).await.is_err());
    }
    println!("PASS: blank, unknown and wrong-kind Core codes refuse opening invoices and bills");
    let a = ar.import_opening_invoices(&s, company, &ar_input).await?;
    assert_eq!(a, ar.import_opening_invoices(&s, company, &ar_input).await?);
    let mut unknown_vendor = ap_input.clone();
    unknown_vendor["csv"] = json!(AP.replace("company_010", "MISSING-VENDOR"));
    assert!(ap
        .import_opening_bills(&s, company, &unknown_vendor)
        .await
        .is_err());
    let vendor = ap
        .get_vendor(&s, company, config["vendor_id"].as_i64().unwrap())
        .await?;
    let mut vendor_input = VendorInput {
        record_kind: Some("company".into()),
        record_id: Some(vendor.party.id),
        new_company: None,
        new_person: None,
        payment_terms_days: vendor.payment_terms_days,
        payable_account_id: vendor.payable_account_id,
        expense_account_id: vendor.expense_account_id,
        tax_code_id: vendor.tax_code_id,
        active: false,
    };
    ap.update_vendor(&s, company, vendor.id, &vendor_input)
        .await?;
    match ap.import_opening_bills(&s, company, &ap_input).await {
        Err(PayablesError::Invalid(fields)) => assert!(fields.iter().all(|f| f.field == "csv")),
        other => panic!("inactive opening vendor was not refused at the CSV field: {other:?}"),
    }
    vendor_input.active = true;
    ap.update_vendor(&s, company, vendor.id, &vendor_input)
        .await?;
    let b = ap.import_opening_bills(&s, company, &ap_input).await?;
    assert_eq!(b, ap.import_opening_bills(&s, company, &ap_input).await?);
    // The residuals are opening balances, not source-period transactions.
    assert!(ar
        .tax_totals(&s, company, &Default::default())
        .await?
        .is_empty());
    assert!(ap
        .tax_totals(&s, company, &Default::default())
        .await?
        .is_empty());
    assert!(ar
        .received_tax_totals(
            &s,
            company,
            &bilibi_finance_receivables::ReceivedTaxOptions::new(
                date(2026, 1, 1),
                date(2027, 12, 31)
            )
        )
        .await?
        .is_empty());
    assert!(ar
        .list_payments(&s, company, &Default::default())
        .await?
        .is_empty());
    assert!(ap
        .list_payments(&s, company, &Default::default())
        .await?
        .is_empty());
    let sst = services.get::<bilibi_finance_sst::Sst>(bilibi_finance_sst::SERVICE)?;
    assert!(sst
        .list_returns(&s, company, None, None, None)
        .await?
        .is_empty());
    // Retain source summaries through the shared reviewed operation. They are
    // evidence and cannot create a posting, return, payment or tax line.
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let key = "finance.reports.source_period.import";
    let Handler::SafeWrite(import) = registry.operations[key].definition.handler else {
        panic!("source schedule import must be a reviewed write");
    };
    for (kind, from, to, rows) in [
        (
            "comparative",
            "2025-01-01",
            "2025-12-31",
            json!([{"source_id":"cash","account_id":config["bank"],"side":"debit","amount":"100.00"}]),
        ),
        (
            "movement",
            "2026-06-01",
            "2026-06-30",
            json!([{"source_id":"sales","account_id":config["expense"],"side":"debit","amount":"20.00"}]),
        ),
        (
            "sst",
            "2026-03-01",
            "2026-04-30",
            json!([{"source_id":"return","authority":"Tax authority","reference":"FILED-1","event_date":"2026-05-15","amount":"10.00"}]),
        ),
        (
            "payroll",
            "2026-05-01",
            "2026-05-31",
            json!([{"source_id":"remittance","authority":"Payroll authority","reference":"PAID-1","event_date":"2026-06-15","amount":"8.00"}]),
        ),
    ] {
        let input = json!({"company_id":company,"kind":kind,"source_reference":format!("HISTORY-{kind}"),"source_system":"Source system","currency_code":"MYR","accounting_timezone":"Asia/Kuala_Lumpur","period_start":from,"period_end":to,"rows":rows});
        let mut raw = db.get().await?;
        let mut tx = l
            .write(
                &mut raw,
                &s,
                company,
                &[bilibi_finance_ledger::MANAGE_ACCOUNTS],
                "walkthrough.source-period",
            )
            .await?;
        let preview = import.preview(tx.sql(), &s, &input).await?.envelope(key);
        assert_eq!(
            preview["result"]["changes"]["rows"][0]["source_id"],
            rows[0]["source_id"]
        );
        assert_eq!(
            preview["result"]["changes"]["rows"][0]["amount"],
            rows[0]["amount"]
        );
        import.commit(&mut tx, &s, &input).await?;
        tx.commit().await?;
        let mut retry = db.get().await?;
        let tx = l
            .write(
                &mut retry,
                &s,
                company,
                &[bilibi_finance_ledger::MANAGE_ACCOUNTS],
                "walkthrough.source-period-refusal",
            )
            .await?;
        assert!(import.preview(tx.sql(), &s, &input).await.is_err());
        tx.rollback().await?;
    }
    assert_eq!(
        trial,
        l.trial_balance(&s, company, date(2026, 7, 1), None).await?
    );
    assert!(ar
        .tax_totals(&s, company, &Default::default())
        .await?
        .is_empty());
    assert!(ap
        .tax_totals(&s, company, &Default::default())
        .await?
        .is_empty());
    assert!(sst
        .list_returns(&s, company, None, None, None)
        .await?
        .is_empty());
    // Independent staged approval carries each asset's evidence with its CSV.
    let lines: Vec<_> = asset_input["csv"].as_str().unwrap().lines().collect();
    for (part, line) in lines.iter().skip(1).enumerate() {
        let mut staged = asset_input.clone();
        staged["part"] = json!(part + 1);
        staged["part_count"] = json!(2);
        staged["csv"] = json!(format!("{}\n{line}\n", lines[0]));
        if part == 1 {
            staged["source_evidence"] = json!([]);
        }
        assets
            .stage_opening_import(&manager, company, &staged)
            .await?;
        assets
            .stage_opening_import(&manager, company, &staged)
            .await?;
        let mut changed = staged.clone();
        if part == 0 {
            changed["source_evidence"][0]["history"][0]["note"] = json!("Changed source bytes");
            assert!(assets
                .stage_opening_import(&manager, company, &changed)
                .await
                .is_err());
        }
    }
    let reconcile = json!({"company_id":company,"source_reference":"ASSET-SOURCE","currency_code":"MYR","csv":"","staged":true});
    assert!(assets
        .import_opening_register(&manager, company, &reconcile)
        .await
        .is_err());
    let Handler::SafeWrite(asset_import) = registry.operations
        ["finance.fixed_assets.opening.import"]
        .definition
        .handler
    else {
        panic!("opening import must be reviewed");
    };
    let mut raw = db.get().await?;
    let mut tx = l
        .write(
            &mut raw,
            &s,
            company,
            &[bilibi_finance_fixed_assets::ACQUIRE],
            "walkthrough.opening-evidence",
        )
        .await?;
    let review = asset_import
        .preview(tx.sql(), &s, &reconcile)
        .await?
        .envelope("finance.fixed_assets.opening.import");
    let facts = &review["result"]["changes"];
    assert_eq!(facts["class_totals"].as_array().unwrap().len(), 2);
    assert_eq!(facts["class_totals"][0]["cost"], "11000");
    assert_eq!(facts["class_totals"][0]["accumulated_depreciation"], "800");
    assert!(facts["month_convention"]
        .as_str()
        .unwrap()
        .contains("mid-month"));
    assert_eq!(
        facts["rows"][0]["source_evidence"]["history"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    asset_import.commit(&mut tx, &s, &reconcile).await?;
    tx.commit().await?;
    drop(raw);
    let asset_batch = assets
        .import_opening_register(&s, company, &reconcile)
        .await?;
    let mut retry_input = asset_input.clone();
    retry_input["csv"] = asset_batch["source_csv"].clone();
    let mut changed = retry_input.clone();
    changed["source_evidence"][0]["history"][0]["note"] = json!("Different retained evidence");
    assert!(assets
        .import_opening_register(&s, company, &changed)
        .await
        .is_err());
    assert_eq!(
        asset_batch,
        assets
            .import_opening_register(&s, company, &retry_input)
            .await?
    );
    let reproduced_ar = ar
        .aged_receivables(
            &s,
            company,
            date(2026, 7, 1),
            &AgingOptions {
                through_entry_number: before_ar_pins.through_entry_number,
                through_opening_batch_id: before_ar_pins.through_opening_batch_id,
                through_settlement_number: before_ar_pins.through_settlement_number,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&before_ar)?,
        serde_json::to_value(&reproduced_ar)?
    );
    let reproduced_ap = ap
        .aged_payables(
            &s,
            company,
            date(2026, 7, 1),
            &bilibi_finance_payables::AgingQuery {
                through_entry_number: before_ap_pins.through_entry_number,
                through_opening_batch_id: before_ap_pins.through_opening_batch_id,
                through_settlement_number: before_ap_pins.through_settlement_number,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&before_ap)?,
        serde_json::to_value(&reproduced_ap)?
    );
    assert_eq!(
        before_ar_tie.total_difference,
        ar.tie_out_control_accounts(&s, company, date(2026, 7, 1), &before_ar_tie.pins())
            .await?
            .total_difference
    );
    assert_eq!(
        before_ap_tie.total_difference,
        ap.tie_out_control_accounts(&s, company, date(2026, 7, 1), &before_ap_tie.pins())
            .await?
            .total_difference
    );
    let reproduced_assets = assets
        .register_report(&s, company, date(2026, 7, 1), &before_assets.pins())
        .await?;
    assert!(reproduced_assets.rows.is_empty());
    assert_eq!(
        before_assets.reconciliation,
        reproduced_assets.reconciliation
    );
    assert_eq!(
        trial,
        l.trial_balance(&s, company, date(2026, 7, 1), None).await?
    );
    assert_eq!(
        ar.aged_receivables(&s, company, date(2026, 6, 30), &AgingOptions::default())
            .await?
            .total,
        rust_decimal::Decimal::ZERO
    );
    assert_eq!(
        ap.aged_payables(&s, company, date(2026, 6, 30), &Default::default())
            .await?
            .total,
        Money::ZERO
    );
    assert!(
        ar.tie_out_control_accounts(&s, company, date(2026, 7, 1), &Default::default())
            .await?
            .tied_out
    );
    assert!(
        ap.tie_out_control_accounts(&s, company, date(2026, 7, 1), &Default::default())
            .await?
            .tied_out
    );
    let aged = ar
        .aged_receivables(&s, company, date(2026, 7, 1), &Default::default())
        .await?;
    assert_eq!(aged.total.to_string(), "3000.0000");
    assert_eq!(aged.customers[0].documents[0].due_date, "2026-06-30");
    let register = assets
        .register_report(&s, company, date(2026, 7, 1), &ReportPins::default())
        .await?;
    assert!(register
        .reconciliation
        .iter()
        .all(|r| r.difference.is_zero()));
    assert_eq!(
        register.rows[0].accumulated.shown(trial.minor_units),
        "800.00"
    );
    let retained = assets
        .get_asset(&s, company, register.rows[0].asset.id)
        .await?;
    let evidence = &retained.opening.as_ref().unwrap()["source_evidence"];
    assert_eq!(evidence["history"].as_array().unwrap().len(), 5);
    assert_eq!(evidence["source_period_expense"][0]["amount"], "600.00");
    assert_eq!(
        evidence["tax_capital_allowances"][0]["allowances"],
        "2400.00"
    );
    assert_eq!(
        assets
            .evidence_page(&s, company, "opening_item", Some(500), None)
            .await?
            .rows
            .len(),
        2
    );
    assert!(register.rows[1].schedule.is_empty());
    assert_eq!(
        register.rows[1].accumulated.shown(trial.minor_units),
        "1000.00"
    );
    assert_eq!(
        register.rows[0].schedule[0].amount.shown(trial.minor_units),
        "106.25"
    );
    let run = assets.depreciate(&s, company, date(2026, 7, 1)).await?;
    assert_eq!(run.total.shown(trial.minor_units), "106.25");
    let register = assets
        .register_report(&s, company, date(2026, 7, 31), &ReportPins::default())
        .await?;
    assert!(register
        .reconciliation
        .iter()
        .all(|r| r.difference.is_zero()));
    assert_eq!(
        register.rows[0].accumulated.shown(trial.minor_units),
        "906.25"
    );
    let fully_depreciated = register.rows[1].asset.id;
    assets
        .dispose(
            &s,
            company,
            fully_depreciated,
            &DisposalInput {
                event: EventInput {
                    effective_date: date(2026, 7, 31),
                    reason: "Retire fully depreciated asset".into(),
                },
                proceeds: Money::ZERO,
                settlement_account_id: None,
            },
        )
        .await?;
    let disposed = assets
        .register_report(&s, company, date(2026, 7, 31), &ReportPins::default())
        .await?;
    assert!(disposed.rows[1].disposed);
    assert!(disposed
        .reconciliation
        .iter()
        .all(|r| r.difference.is_zero()));
    assets
        .reverse_disposal(
            &s,
            company,
            fully_depreciated,
            &EventInput {
                effective_date: date(2026, 7, 31),
                reason: "Correct retirement".into(),
            },
        )
        .await?;
    let restored = assets
        .register_report(&s, company, date(2026, 7, 31), &ReportPins::default())
        .await?;
    assert!(!restored.rows[1].disposed);
    assert!(restored
        .reconciliation
        .iter()
        .all(|r| r.difference.is_zero()));
    let pinned = assets
        .register_report(&s, company, date(2026, 7, 31), &disposed.pins())
        .await?;
    assert!(pinned.rows[1].disposed);
    assert_eq!(pinned.reconciliation, disposed.reconciliation);
    let docs = ar.list_documents(&s, company, &Default::default()).await?;
    let invoice = docs[0].id;
    let payment=ar.record_payment(&s,company,&json!({"series_id":config["payment_series"],"customer_id":config["customer_id"],"payment_date":"2026-07-15","amount":"1000","deposit_account_id":config["bank"],"allocations":[{"document_id":invoice,"amount":"1000"}]})).await?;
    // A later receipt of an opening residual also creates no source tax share.
    assert!(ar
        .received_tax_totals(
            &s,
            company,
            &bilibi_finance_receivables::ReceivedTaxOptions::new(
                date(2026, 1, 1),
                date(2027, 12, 31)
            )
        )
        .await?
        .is_empty());
    assert_eq!(
        ar.get_document(&s, company, invoice)
            .await?
            .outstanding
            .unwrap()
            .shown(trial.minor_units),
        "2000.00"
    );
    ar.reverse_payment(
        &s,
        company,
        payment.id,
        &json!({"reversal_date":"2026-07-16","reason":"Conversion settlement correction"}),
    )
    .await?;
    assert_eq!(
        ar.get_document(&s, company, invoice)
            .await?
            .outstanding
            .unwrap()
            .shown(trial.minor_units),
        "3000.00"
    );
    assert!(
        ar.tie_out_control_accounts(&s, company, date(2026, 7, 31), &Default::default())
            .await?
            .tied_out
    );
    let bill = ap.list_documents(&s, company, &Default::default()).await?[0].id;
    let imported_bill = ap.get_document(&s, company, bill).await?;
    assert_eq!(imported_bill.vendor_id, config["vendor_id"].as_i64());
    assert_eq!(
        imported_bill.party.key,
        config["vendor_key"].as_str().unwrap()
    );
    assert_eq!(
        imported_bill.opening.as_ref().unwrap()["vendor_name"],
        "Conversion vendor"
    );
    let note_on = |day| NewDocument {
        vendor_id: config["vendor_id"].as_i64(),
        party_document_number: Some("DEBIT-PROOF".into()),
        document_date: day,
        due_date: None,
        description: Some("Debit note conversion-date proof".into()),
        lines: vec![DocumentLineInput {
            description: "Supplier credit".into(),
            account_id: config["expense"].as_i64().expect("expense account"),
            amount: Money::parse("100", trial.minor_units).expect("fixture money"),
            tax_code_id: None,
            external_order_line_id: None,
            order_line_id: None,
            quantity: None,
        }],
        tax_lines: None,
    };
    match ap
        .create_debit_note(&manager, company, bill, &note_on(date(2026, 6, 15)))
        .await
    {
        Err(PayablesError::Invalid(fields)) => {
            let text = format!("{fields:?}");
            assert!(text.contains("document_date"), "{text}");
            assert!(
                text.contains("cannot be settled before the conversion date"),
                "{text}"
            );
        }
        other => panic!("debit note before conversion was not refused: {other:?}"),
    }
    assert!(matches!(
        ap.create_debit_note(&manager, company, 0, &note_on(date(2026, 7, 15)))
            .await,
        Err(PayablesError::DocumentNotFound)
    ));
    let note = ap
        .create_debit_note(&manager, company, bill, &note_on(date(2026, 7, 15)))
        .await?;
    ap.void_document(&manager, company, note.id, "Conversion-date proof only")
        .await?;
    let amount = Money::parse("500", trial.minor_units)?;
    let request = ap
        .submit_payment(
            &manager,
            company,
            &bilibi_finance_payables::NewPayment {
                payment_date: date(2026, 7, 15),
                vendor_id: config["vendor_id"].as_i64(),
                supplier_key: None,
                paid_from_account_id: config["bank"].as_i64().unwrap(),
                reference: Some("Opening settlement proof".into()),
                description: None,
                allocations: vec![bilibi_finance_payables::AllocationInput {
                    document_id: bill,
                    amount,
                }],
            },
        )
        .await?;
    let paid = ap.approve_payment(&s, company, request.id).await?;
    assert_eq!(
        ap.get_document(&s, company, bill)
            .await?
            .outstanding
            .unwrap()
            .shown(trial.minor_units),
        "1000.00"
    );
    assert!(
        ap.tie_out_control_accounts(&s, company, date(2026, 7, 31), &Default::default())
            .await?
            .tied_out
    );
    ap.reverse_payment(
        &s,
        company,
        paid.id,
        date(2026, 7, 16),
        Some("Conversion settlement correction".into()),
    )
    .await?;
    assert_eq!(
        ap.get_document(&s, company, bill)
            .await?
            .outstanding
            .unwrap()
            .shown(trial.minor_units),
        "1500.00"
    );
    assert!(
        ap.tie_out_control_accounts(&s, company, date(2026, 7, 31), &Default::default())
            .await?
            .tied_out
    );
    let context = l
        .opening_context_on(&**db.get().await?, &s, company)
        .await?;
    assert!(l
        .reverse_entry(
            &s,
            company,
            context.entry.id,
            &Reversal {
                entry_date: date(2026, 7, 31),
                description: "Refused while imports depend on opening".into(),
                reference: None
            }
        )
        .await
        .is_err());
    let c = db.get().await?;
    let entries: i64 = c
        .query_one(
            "SELECT count(*) FROM finance_ledger_entries WHERE tenant_id=1 AND company_id=$1",
            &[&company],
        )
        .await?
        .get(0);
    assert_eq!(entries, 8);
    for table in [
        "finance_receivables_opening_batches",
        "finance_payables_opening_batches",
        "finance_fixed_assets_opening_batches",
        "finance_ledger_opening_recognitions",
    ] {
        assert!(c
            .execute(
                &format!(
                    "UPDATE {table} SET company_id=company_id WHERE tenant_id=1 AND company_id=$1"
                ),
                &[&company]
            )
            .await
            .is_err());
    }
    println!(
        "Conversion facade proof: exact source balances, partial residuals, all three mismatch refusals, corrected preview, identical retries, independent approval, pre-cutoff exclusion, aged dates, control tie-outs, source accumulation differing from historical projection, prospective 106.25 depreciation, receipt, supplier payment and their reversals, guarded opening reversal, audit evidence and retained rows passed."
    );
    drop(c);
    if std::env::args().any(|arg| arg == "assets-evidence") {
        assets
            .reverse_run(
                &s,
                company,
                run.id,
                &EventInput {
                    effective_date: date(2026, 7, 31),
                    reason: "Correct the opening asset run".into(),
                },
            )
            .await?;
        let corrected = assets
            .register_report(&s, company, date(2026, 7, 31), &Default::default())
            .await?;
        assert_eq!(
            corrected.rows[0].accumulated.shown(trial.minor_units),
            "800.00"
        );
        assert!(corrected
            .reconciliation
            .iter()
            .all(|row| row.difference.is_zero()));
        let reproduced = assets
            .register_report(&s, company, date(2026, 7, 31), &register.pins())
            .await?;
        assert_eq!(
            serde_json::to_value(&reproduced)?,
            serde_json::to_value(&register)?
        );
        let next = assets.depreciate(&s, company, date(2026, 8, 1)).await?;
        assert_eq!(next.total.shown(trial.minor_units), "212.50");
        let report = assets
            .register_report(&s, company, date(2026, 8, 31), &Default::default())
            .await?;
        assert!(report
            .reconciliation
            .iter()
            .all(|row| row.difference.is_zero()));
        assert!(report.rows[1].schedule.is_empty());
        println!("PASS opening asset evidence: typed staged approval, source history/YTD/tax separation, shared-account class totals, zero-life import, future runs, disposal/reversal, run reversal/catch-up and reproduced report pins");
        return Ok(());
    }
    let report = conversion_report::before(db, services).await?;
    if std::env::args().any(|s| s == "report-browser") {
        println!("Conversion report browser fixture ready on Company 003; bulk correction proof was not run.");
        return Ok(());
    }
    conversion_lifecycle::prove(db, services, &config, [a, b, asset_batch], run.id).await?;
    conversion_report::after(db, services, report).await?;
    prove_opening_invoice_page_scale(db, services).await?;
    let client = db.get().await?;
    let bills = registry
        .call(
            &**client,
            &s,
            "finance.payables.document.page",
            &json!({"company_id":company,"source":"opening","page":1,"page_size":25}),
        )
        .await?;
    let bill = bills["result"]["documents"][0]
        .as_object()
        .ok_or("opening bill page row missing")?;
    assert!(bill.contains_key("party_code") && bill.contains_key("match_status"));
    assert!(!bill.contains_key("lines") && !bill.contains_key("match_events"));
    println!("Opening list contract proof: SQL filtering, sorting, count, clamping and lightweight rows passed.");
    Ok(())
}
/// The year-to-date bridge on its own company, so the conversion report's
/// four retained schedules on Company 003 stay as they are.
async fn prove_source_bridge(db: &Db, services: &Services) -> Outcome<()> {
    let company = 5;
    setup(db, services, company).await?;
    let s = in_company(db, services, 4, company).await?;
    bilibi_finance_walkthrough_fixture::source_bridge::prove(db, services, &s, company).await
}
async fn prove_historical_controls(db: &Db, services: &Services) -> Outcome<()> {
    let company = 4;
    let config = setup(db, services, company).await?;
    let reviewer = in_company(db, services, 4, company).await?;
    let manager = in_company(db, services, 1, company).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let trial = ledger
        .trial_balance(&reviewer, company, date(2026, 7, 1), None)
        .await?;
    let ar_input = json!({"source_reference":"MULTI-AR","currency_code":"MYR","series_id":config["invoice_series"],"csv":MULTI_AR,"opening_account_classifications":config["opening_account_classifications"]});
    let ap_input = json!({"source_reference":"MULTI-AP","currency_code":"MYR","csv":MULTI_AP,"opening_account_classifications":config["opening_account_classifications"]});
    let mut incomplete = ar_input.clone();
    incomplete["opening_account_classifications"]
        .as_object_mut()
        .expect("classifications")
        .remove(&config["historical_ar"].as_i64().unwrap().to_string());
    let refusal = ar
        .import_opening_invoices(&reviewer, company, &incomplete)
        .await
        .expect_err("an uncovered historical receivables control must be refused");
    let refusal = format!("{refusal:?}");
    assert!(refusal.contains("1110") && refusal.contains("500.00"));
    let mut incomplete = ap_input.clone();
    incomplete["opening_account_classifications"]
        .as_object_mut()
        .expect("classifications")
        .remove(&config["historical_ap"].as_i64().unwrap().to_string());
    let refusal = ap
        .import_opening_bills(&reviewer, company, &incomplete)
        .await
        .expect_err("an uncovered historical payables control must be refused");
    let refusal = format!("{refusal:?}");
    assert!(refusal.contains("2010") && refusal.contains("-300.00"));
    let mut wrong = ar_input.clone();
    wrong["csv"] = json!(MULTI_AR
        .replace(",2500,1100", ",2400,1100")
        .replace(",500,1110", ",600,1110"));
    assert!(ar
        .import_opening_invoices(&reviewer, company, &wrong)
        .await
        .is_err());
    let mut wrong = ap_input.clone();
    wrong["csv"] = json!(MULTI_AP
        .replace(",1200,2000", ",1100,2000")
        .replace(",300,2010", ",400,2010"));
    assert!(ap
        .import_opening_bills(&reviewer, company, &wrong)
        .await
        .is_err());
    ar.import_opening_invoices(&reviewer, company, &ar_input)
        .await?;
    ap.import_opening_bills(&reviewer, company, &ap_input)
        .await?;
    assert_eq!(
        trial,
        ledger
            .trial_balance(&reviewer, company, date(2026, 7, 1), None)
            .await?
    );
    let chart = ledger.list_accounts(&reviewer, company).await?;
    let account = |code: &str| chart.iter().find(|a| a.code == code).unwrap().id;
    let invoices = ar
        .list_documents(&reviewer, company, &Default::default())
        .await?;
    let invoice = invoices
        .iter()
        .find(|d| d.receivable_account_id == Some(account("1110")))
        .unwrap();
    let bills = ap
        .list_documents(&reviewer, company, &Default::default())
        .await?;
    let bill = bills
        .iter()
        .find(|d| d.payable_account_id == Some(account("2010")))
        .unwrap();
    assert_eq!(invoice.due_date.as_deref(), Some("2026-05-31"));
    assert_eq!(bill.due_date.to_string(), "2026-05-15");
    assert!(
        ar.tie_out_control_accounts(&reviewer, company, date(2026, 7, 1), &Default::default())
            .await?
            .tied_out
    );
    assert!(
        ap.tie_out_control_accounts(&reviewer, company, date(2026, 7, 1), &Default::default())
            .await?
            .tied_out
    );
    let receipt = ar.record_payment(&reviewer, company, &json!({"series_id":config["payment_series"],"customer_id":config["customer_id"],"payment_date":"2026-07-15","amount":"100","deposit_account_id":config["bank"],"allocations":[{"document_id":invoice.id,"amount":"100"}]})).await?;
    ar.reverse_payment(
        &reviewer,
        company,
        receipt.id,
        &json!({"reversal_date":"2026-07-16","reason":"Correction"}),
    )
    .await?;
    let request = ap
        .submit_payment(
            &manager,
            company,
            &bilibi_finance_payables::NewPayment {
                payment_date: date(2026, 7, 15),
                vendor_id: config["vendor_id"].as_i64(),
                supplier_key: None,
                paid_from_account_id: config["bank"].as_i64().unwrap(),
                reference: None,
                description: None,
                allocations: vec![bilibi_finance_payables::AllocationInput {
                    document_id: bill.id,
                    amount: Money::parse("100", trial.minor_units)?,
                }],
            },
        )
        .await?;
    let payment = ap.approve_payment(&reviewer, company, request.id).await?;
    ap.reverse_payment(
        &reviewer,
        company,
        payment.id,
        date(2026, 7, 16),
        Some("Correction".into()),
    )
    .await?;
    assert!(
        ar.tie_out_control_accounts(&reviewer, company, date(2026, 7, 31), &Default::default())
            .await?
            .tied_out
    );
    assert!(
        ap.tie_out_control_accounts(&reviewer, company, date(2026, 7, 31), &Default::default())
            .await?
            .tied_out
    );
    println!("PASS: historical controls refuse uncovered balances, reconcile per account, and settle and reverse on the saved controls");
    Ok(())
}
#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:8147".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    if std::env::args().any(|s| s == "launcher-ordinary") {
        launcher_ordinary(&db, &app.services).await?;
        return Ok(());
    }
    if std::env::args().any(|s| s == "launcher-credentials") {
        launcher_credentials(&db, &app.services).await?;
        return Ok(());
    }
    if std::env::args().any(|s| s == "launcher-asset-credentials") {
        if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
            return Err("disposable proof required".into());
        }
        let c = db.get().await?;
        let proof: bool = c.query_one("SELECT current_database()='fin_issue_110_rest_ui' AND EXISTS(SELECT 1 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=6 AND i.email='ai@agent.my') AND EXISTS(SELECT 1 FROM finance_ledger_entries WHERE company_id=2)",&[]).await?.get(0);
        if !proof {
            return Err(
                "prepare this task's disposable conversion database before restoring identities"
                    .into(),
            );
        }
        // Fixture identities only: the normal launcher retires the old demo users.
        c.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(1,1,'ada_manager','Ada Manager','human','manager@example.test') ON CONFLICT DO NOTHING;
        INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(1,1,1,1,'Manager') ON CONFLICT DO NOTHING;
        INSERT INTO user_identities(kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) SELECT 'staff','manager@example.test',i.password_hash,1,1,1 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=6 AND i.email='ai@agent.my' ON CONFLICT DO NOTHING; INSERT INTO users(id,tenant_id,name,identity_id,person_id) SELECT 1,1,'Ada Manager',id,1 FROM user_identities WHERE home_tenant_id=1 AND home_user_id=1 ON CONFLICT DO NOTHING;
        INSERT INTO user_desks(tenant_id,person_id,company_id) VALUES(1,1,1) ON CONFLICT DO NOTHING;").await?;
    }
    install(&db, &app.services).await?;
    // Identity-only fixture repair: the reviewer has a real Person affiliation,
    // so browser sessions can choose the current company. No business rows use SQL.
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Conversion reviewer','human','admin@example.test') ON CONFLICT DO NOTHING; INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','admin@example.test') ON CONFLICT DO NOTHING; UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=4 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=4 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;").await?;
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(5,1,'conversion_viewer','Conversion statement preparer','human','viewer@example.test'),(6,1,'conversion_staff','Conversion independent signer','human','staff@example.test') ON CONFLICT DO NOTHING;
    INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(4,1,5,1,'Statement preparer'),(5,1,6,1,'Independent signer') ON CONFLICT DO NOTHING;
    UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=2 AND person_id IS NULL;
    UPDATE users SET person_id=6 WHERE tenant_id=1 AND id=3 AND person_id IS NULL;
    INSERT INTO user_desks(tenant_id,person_id,company_id) VALUES(1,5,1),(1,6,1) ON CONFLICT DO NOTHING;
    SELECT setval(pg_get_serial_sequence('persons','id'),GREATEST((SELECT max(id) FROM persons),1));
    SELECT setval(pg_get_serial_sequence('person_affiliations','id'),GREATEST((SELECT max(id) FROM person_affiliations),1));").await?;
    let admin = sign_in(&db, &app.services, 1, 4).await?;
    let storage_root = std::env::var("BILIBI_ATTACHMENTS_DEMO_ROOT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join(format!("bilibi-finance-conversion-{}", std::process::id()))
            .display()
            .to_string()
    });
    std::fs::create_dir_all(&storage_root)?;
    std::fs::set_permissions(
        &storage_root,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    bilibi_finance_walkthrough_fixture::attachments::configure_storage(
        &db,
        &app.services,
        &admin,
        &storage_root,
    )
    .await?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants
        .set_direct(&admin, 1, "admin.company.tenant-wide.manage", Some(true))
        .await?;
    if std::env::args().any(|s| s == "launcher-asset-credentials") {
        let operator = sign_in(&db, &app.services, 1, 6).await?;
        let assigned = db
            .get()
            .await?
            .query_opt(
                "SELECT id FROM user_desks WHERE tenant_id=1 AND company_id=1 AND person_id=3",
                &[],
            )
            .await?
            .is_some();
        if !assigned {
            app.services
                .get::<bilibi_core::user::UserService>("core/user.accounts")?
                .set_desk(&operator, 1, 3, true)
                .await?;
        }
        bilibi_finance_walkthrough_fixture::assets_lifecycle::credentials(&db, &app.services)
            .await?;
        return Ok(());
    }
    if std::env::args().any(|s| s == "credits") {
        conversion_credits::prove(&db, &app.services).await?;
        return Ok(());
    }
    if std::env::args().any(|s| s == "opening-list-scale") {
        prove_opening_invoice_page_scale(&db, &app.services).await?;
        return Ok(());
    }
    if std::env::args().any(|s| s == "prepare-credits-launcher") {
        db.get().await?.execute("UPDATE companies SET name=CASE id WHEN 10 THEN 'Opening credit proof counterparty' ELSE 'Other opening credit counterparty' END WHERE tenant_id=1 AND id IN(10,11) AND name='Company '||lpad(id::text,3,'0')",&[]).await?;
        let mut sources = Vec::new();
        for company in [8, 9] {
            sources.push(setup(&db, &app.services, company).await?);
        }
        std::fs::write(
            std::env::var("BILIBI_OPENING_PROOF_CONFIG")?,
            serde_json::to_vec_pretty(&sources)?,
        )?;
        println!("Credit sources prepared for browser Company 008 and MCP Company 009.");
        return Ok(());
    }
    if std::env::args().any(|s| s == "prepare-launcher") {
        // Fixture identity only: the launcher's demo retirement matches the
        // original generated name. Keep the counterparty these records use.
        db.get().await?.execute("UPDATE companies SET name='Opening proof counterparty' WHERE tenant_id=1 AND id=10 AND name='Company 010'", &[]).await?;
        let mut sources = Vec::new();
        for company in [1, 2, 4] {
            sources.push(setup(&db, &app.services, company).await?);
        }
        bilibi_finance_walkthrough_fixture::source_bridge::seed(
            &db,
            &app.services,
            &in_company(&db, &app.services, 4, 1).await?,
            1,
        )
        .await?;
        std::fs::write(
            std::env::var("BILIBI_OPENING_PROOF_CONFIG")?,
            serde_json::to_vec_pretty(&sources)?,
        )?;
        println!("Opening list sources prepared. Start the Platform launcher on this disposable database.");
        return Ok(());
    }
    if std::env::args().any(|s| s == "serve") {
        for company in [1, 2, 4] {
            println!(
                "Disposable conversion source: {}",
                setup(&db, &app.services, company).await?
            );
        }
        // Monthly movements for the bridge on Company 001's Monthly movements tab.
        bilibi_finance_walkthrough_fixture::source_bridge::seed(
            &db,
            &app.services,
            &in_company(&db, &app.services, 4, 1).await?,
            1,
        )
        .await?;
        if std::env::args().any(|s| s == "assets-lifecycle") {
            bilibi_finance_walkthrough_fixture::assets_lifecycle::credentials(&db, &app.services)
                .await?;
        }
        println!(
            "Conversion browser fixture at {origin}; independent reviewer admin@example.test; walkthrough password. Company 001 UI first, Company 002 MCP second, Company 004 multi-control preview."
        );
        axum::serve(
            tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
            bilibi_web::router(app),
        )
        .await?;
    } else {
        prove(&db, &app.services).await?;
        if !std::env::args().any(|arg| arg == "assets-evidence") {
            prove_historical_controls(&db, &app.services).await?;
            prove_source_bridge(&db, &app.services).await?;
            conversion_credits::prove(&db, &app.services).await?;
        }
    }
    Ok(())
}

// One-door proof: keep the normal Platform launcher and its administrator.
// This is called after launch-only retirement of the old walkthrough users.
async fn launcher_credentials(db: &Db, services: &Services) -> Outcome<()> {
    use bilibi_core::agent::{
        code_challenge, AgentAccess, AuthorizeParams, ClientRegistration, CodeExchange,
    };
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
    if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
        return Err("disposable proof required".into());
    }
    let client = db.get().await?;
    let row = client
        .query_one(
            "SELECT u.tenant_id,u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE i.email='ai@agent.my'",
            &[],
        )
        .await?;
    let (tenant, user): (i64, i64) = (row.get(0), row.get(1));
    drop(client);
    let scope = sign_in(db, services, tenant, user).await?;
    if std::env::var("BILIBI_CREDITS_PROOF").as_deref() == Ok("disposable") {
        let person = scope
            .actor()
            .person()
            .ok_or("launcher administrator has no Person")?
            .get();
        let users = services.get::<bilibi_core::user::UserService>("core/user.accounts")?;
        for company in [8, 9] {
            let client = db.get().await?;
            let assigned = bilibi_tenancy::reaches_work_company(
                &**client,
                &scope,
                bilibi_party_ids::OrganisationId::new(company)?,
            )
            .await?;
            drop(client);
            if !assigned {
                users.set_desk(&scope, company, person, true).await?;
            }
            services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?
                .set_duty_separation(&scope,company,false,Some("Disposable single-person browser/MCP parity fixture; retain every same-person approval exception")).await?;
        }
    }
    let access = services.get::<AgentAccess>("core/agent.grants")?;
    let callback = "https://example.test/opening-list-proof/callback";
    let registration=access.register_client(&serde_json::from_value::<ClientRegistration>(json!({"client_name":"Opening list proof","redirect_uris":[callback],"token_endpoint_auth_method":"none"}))?).await?;
    let client_id = registration["client_id"]
        .as_str()
        .ok_or("missing client ID")?;
    let origin = std::env::var("APP_ORIGIN")?;
    let mut tokens = serde_json::Map::new();
    let resource = format!("{}/mcp", origin.trim_end_matches('/'));
    let verifier = "opening-list-proof-verifier-with-at-least-forty-three-characters";
    let params = AuthorizeParams {
        client_id: client_id.into(),
        redirect_uri: callback.into(),
        response_type: "code".into(),
        resource: resource.clone(),
        scope: Some("bilibi".into()),
        code_challenge: code_challenge(verifier),
        code_challenge_method: "S256".into(),
        state: None,
    };
    let code = access.authorize(&scope, &params, &resource).await?;
    let token = access
        .exchange_code(
            &CodeExchange {
                code,
                client_id: params.client_id,
                redirect_uri: params.redirect_uri,
                code_verifier: verifier.into(),
                resource: resource.clone(),
            },
            &resource,
        )
        .await?;
    tokens.insert("mcp".into(), json!(token.access_token));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(std::env::var("BILIBI_AGENT_CREDENTIALS")?)?;
    file.write_all(&serde_json::to_vec(&tokens)?)?;
    println!("Opening list OAuth credentials saved privately.");
    Ok(())
}

// Create a normal acquisition after the opening register was approved in the
// browser. Both kinds must be visible together on the same record list.
async fn launcher_ordinary(db: &Db, services: &Services) -> Outcome<()> {
    use bilibi_finance_ledger::{NewEntry, NewLine, PostOptions};
    if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
        return Err("disposable proof required".into());
    }
    let client = db.get().await?;
    let row = client
        .query_one(
            "SELECT u.tenant_id,u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE i.email='ai@agent.my'",
            &[],
        )
        .await?;
    let (tenant, user): (i64, i64) = (row.get(0), row.get(1));
    drop(client);
    let scope = sign_in(db, services, tenant, user).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let assets = services.get::<FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    let book = ledger.get_book(&scope, 1).await?;
    let accounts = ledger.list_accounts(&scope, 1).await?;
    let account = |code| {
        accounts
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or("proof account missing")
    };
    let category = assets
        .list_categories(&scope, 1)
        .await?
        .into_iter()
        .find(|c| c.name == "Conversion assets")
        .ok_or("proof category missing")?;
    let amount = Money::parse("500.00", book.minor_units)?;
    let entry = ledger
        .post_entry(
            &scope,
            1,
            &NewEntry {
                entry_date: date(2026, 10, 1),
                description: "Ordinary acquisition for list proof".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(account("1200")?, amount),
                    NewLine::credit(account("1000")?, amount),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    assets
        .acquire_from_entry(
            &scope,
            1,
            entry.id,
            1,
            &bilibi_finance_fixed_assets::AcquisitionInput {
                category_revision_id: category.revision.id,
                description: "Ordinary acquired asset".into(),
                in_service_date: date(2026, 10, 1),
                residual_value: None,
                method: None,
                life_months: None,
                annual_rate: None,
            },
        )
        .await?;
    println!("Ordinary asset prepared through the ledger and Fixed Assets facades.");
    Ok(())
}
