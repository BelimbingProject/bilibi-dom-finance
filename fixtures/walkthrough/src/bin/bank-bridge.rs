//! Disposable conversion bridge proof; run around the MCP operation walkthrough.
use bilibi_database::pool;
use bilibi_finance_attachments::{Attachments, NewFile};
use bilibi_finance_reconciliation::{
    NewReconciliation, Pins, Reconciliation, OPENING_BRIDGE_SOURCE,
};
use bilibi_finance_walkthrough_fixture::sign_in;
use jiff::civil::date;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
        return Err("disposable proof required".into());
    }
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    let manager = sign_in(&db, &services, 1, 1)
        .await
        .map_err(|e| format!("manager sign in: {e}"))?;
    let reviewer = sign_in(&db, &services, 1, 7)
        .await
        .map_err(|e| format!("reviewer sign in: {e}"))?;
    let admin = sign_in(&db, &services, 1, 4)
        .await
        .map_err(|e| format!("admin sign in: {e}"))?;
    let bank = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let attachments = services.get::<Attachments>(bilibi_finance_attachments::SERVICE)?;
    let account = bank
        .list_bank_accounts(&reviewer, 2)
        .await
        .map_err(|e| format!("list bank accounts: {e}"))?
        .into_iter()
        .find(|a| a.name == "Conversion bank")
        .ok_or("missing account")?;
    let request = |day, balance: &str| NewReconciliation {
        end_date: Some(day),
        statement_balance: balance.into(),
    };
    match std::env::args().nth(1).as_deref() {
        Some("prepare") => {
            let preview = bank
                .preview_reconciliation(
                    &reviewer,
                    2,
                    account.id,
                    &request(date(2026, 7, 1), "10200"),
                    &Pins::default(),
                )
                .await
                .map_err(|e| format!("preview opening reconciliation: {e}"))?;
            assert!(preview.figures.difference.is_zero());
            assert_eq!(preview.figures.items.len(), 2);
            let root = std::env::var("FINANCE_UI_STORAGE")?;
            std::fs::create_dir_all(&root)?;
            bilibi_finance_walkthrough_fixture::attachments::configure_storage(
                &db, &services, &admin, &root,
            )
            .await
            .map_err(|e| format!("configure storage: {e}"))?;
            let bridge_id = db
                .get()
                .await?
                .query_one(
                    "SELECT b.id FROM finance_reconciliation_opening_bridges b WHERE b.company_id=2 AND b.bank_account_id=$1 AND NOT EXISTS (SELECT 1 FROM finance_reconciliation_opening_bridges r WHERE r.replaces_bridge_id=b.id)",
                    &[&account.id],
                )
                .await?
                .get::<_, i64>(0);
            let source_bytes = b"Source bank statement at 2026-07-01: balance MYR 10,200.00";
            let source = attachments
                .attach(
                    &reviewer,
                    2,
                    OPENING_BRIDGE_SOURCE,
                    bridge_id,
                    &NewFile {
                        file_name: "source-statement.txt".into(),
                        content_type: Some("text/plain".into()),
                        content: source_bytes.to_vec(),
                    },
                )
                .await?;
            let kept = attachments.read_attachment(&reviewer, 2, source.id).await?;
            assert_eq!(kept.bytes.as_slice(), source_bytes);
            assert_eq!(source.document_id, bridge_id);
            assert_eq!(source.document_date, date(2026, 7, 1));
            assert_eq!(source.source, OPENING_BRIDGE_SOURCE);
            let before_conversion = NewFile {
                file_name: "before-conversion.csv".into(),
                content_type: Some("text/csv".into()),
                content: b"Date,Details,Ref,Amount,Balance\n2026-06-30,Too early,PAY-1,-500,9700\n"
                    .to_vec(),
            };
            let refusal = bank
                .import_statement(&reviewer, 2, account.id, &before_conversion)
                .await
                .expect_err("a statement before conversion must be refused");
            assert!(format!("{refusal:?}").contains(
                "statement lines must be on or after the opening conversion date 2026-07-01"
            ));
            let file = NewFile { file_name:"clearing.csv".into(), content_type:Some("text/csv".into()), content:b"Date,Details,Ref,Amount,Balance\n2026-07-01,Payment cleared,PAY-1,-500,9700\n2026-07-03,Deposit cleared,DEP-1,300,10000\n".to_vec() };
            let imported = bank
                .import_statement(&reviewer, 2, account.id, &file)
                .await
                .map_err(|e| format!("import statement: {e}"))?;
            assert_eq!(imported.statement.line_count, 2);
            assert!(bank
                .import_statement(&reviewer, 2, account.id, &file)
                .await
                .is_err());
            println!("Opening report balances with two source items; source statement retained; pre-conversion statement refused, conversion-day statement imported and duplicate refused");
        }
        Some("report") => {
            let input = request(date(2026, 7, 3), "10000");
            let preview = bank
                .preview_reconciliation(&reviewer, 2, account.id, &input, &Pins::default())
                .await?;
            assert!(preview.figures.difference.is_zero());
            assert!(preview.figures.items.is_empty());
            let opened = bank
                .open_reconciliation(&manager, 2, account.id, &input)
                .await?;
            let finalised = bank
                .finalise_reconciliation(&reviewer, 2, opened.id, None)
                .await?;
            let report = bank.get_reconciliation(&reviewer, 2, finalised.id).await?;
            assert!(report
                .figures
                .as_ref()
                .ok_or("missing figures")?
                .difference
                .is_zero());
            println!(
                "Independent reviewer finalised the zero-difference reconciliation: {}",
                serde_json::to_string(&report)?
            );
        }
        _ => return Err("choose prepare or report".into()),
    }
    Ok(())
}
