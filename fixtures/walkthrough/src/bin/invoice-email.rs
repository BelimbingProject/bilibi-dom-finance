//! Emailing an issued invoice, on a fresh disposable database: the company
//! setting (off until turned on), the refusals a person or an agent gets, the
//! send recorded on the invoice, and Issue and send in one step. Every call
//! goes through the operation contracts the invoice page uses.
//!
//! With `INVOICE_EMAIL_PROOF_DIR` (a private folder for Base Artifacts; set
//! `INVOICE_LAYOUT_CHROMIUM` where Chromium is not on the default path) a
//! queue worker then delivers the mail into the Platform's development
//! mailbox (`BILIBI_MAIL_DIR`, no `MAIL_HOST`), and the proof reads the
//! message back: recipient, sender name, reply address, subject and the PDF.
use bilibi_agent_api::{Handler, Registry};
use bilibi_database::{pool, Db, MutationBoundary};
use bilibi_finance_receivables::Receivables;
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use bilibi_module_registry::Services;
use bilibi_queue::{Config, Executor};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::time::Duration;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.fixture.invoice-email",bilibi_module_registry::Provenance {module:"finance/receivables",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution {domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[bilibi_authz::RoleDefinition{code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_receivables::CAPABILITIES}]}) }

type Proof<T> = Result<T, Box<dyn std::error::Error>>;
const COMPANY: i64 = 1;

/// One reviewed write through its contract: previewed, then committed.
async fn write(db: &Db, services: &Services, scope: &Scope, key: &str, mut input: Value) -> Proof<Value> {
    input["company_id"] = json!(COMPANY);
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let Handler::SafeWrite(handler) = registry.operations[key].definition.handler else {
        return Err(format!("{key} must be a reviewed write").into());
    };
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(&mut client, scope.mutation_context("walkthrough.invoice-email"))
        .await?;
    let review = handler.preview(tx.sql(), scope, &input).await?.envelope(key);
    let committed = handler.commit(&mut tx, scope, &input).await?.envelope(key);
    tx.commit().await?;
    Ok(json!({"review": review["result"], "record": committed["result"]["record"]}))
}

async fn read(db: &Db, services: &Services, scope: &Scope, key: &str, mut input: Value) -> Proof<Value> {
    input["company_id"] = json!(COMPANY);
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let Handler::Read(handler) = registry.operations[key].definition.handler else {
        return Err(format!("{key} must be a read").into());
    };
    let client = db.get().await?;
    Ok(handler.call(&**client, scope, &input).await?.envelope(key)["result"].clone())
}

/// A write that must be refused, and say `words` to the person.
async fn refused(db: &Db, services: &Services, scope: &Scope, key: &str, input: Value, words: &str) -> Proof<()> {
    match write(db, services, scope, key, input).await {
        Ok(answer) => Err(format!("{key} should be refused ({words}), but answered {answer}").into()),
        Err(error) => {
            let said = format!("{error:?}");
            if said.contains(words) {
                Ok(())
            } else {
                Err(format!("{key} was refused with {said}, not with: {words}").into())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Proof<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or("http://127.0.0.1:45481".into());
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let installed = install(&db, &app.services).await?;
    let services = &app.services;
    let scope = sign_in(&db, services, 1, 1).await?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let account = |code: &str| installed.accounts.iter().find(|a| a.code == code).unwrap().id;
    ar.configure(&scope, COMPANY, &json!({"receivable_account_id":account("1100"),"income_account_id":account("4000")})).await?;
    let series = ar.create_series(&scope, COMPANY, &json!({"code":"MAIL","name":"Invoices","kind":"invoice","prefix":"INV-","number_width":5})).await?;
    let customer = |name: &str, email: Option<&str>| {
        let mut person = json!({"name": name, "kind": "human"});
        if let Some(email) = email {
            person["email"] = json!(email);
        }
        json!({"new_person": person, "payment_terms_days": 30})
    };
    let with_email = write(&db, services, &scope, "finance.receivables.customer.create", customer("Jamie Mail Buyer", Some("jamie@example.test"))).await?["record"]["id"].as_i64().unwrap();
    let without_email = write(&db, services, &scope, "finance.receivables.customer.create", customer("Nomail Buyer", None)).await?["record"]["id"].as_i64().unwrap();
    let draft = |customer: i64| json!({"kind":"invoice","series_id":series.id,"customer_id":customer,"issue_date":"2026-11-02","lines":[{"description":"Consulting, October","quantity":"10","unit_price":"125.00"}]});
    let first = ar.create_draft(&scope, COMPANY, &draft(with_email)).await?;
    let issued = ar.issue_document(&scope, COMPANY, first.id).await?;
    let reference = issued.document_reference.clone().unwrap();
    let send = "finance.receivables.document.send";

    // Off by default: nothing is offered and nothing is sent.
    let setup = ar.get_setup(&scope, COMPANY).await?;
    assert!(!setup.invoice_email.enabled && !setup.invoice_email.issue_and_send);
    let offer = read(&db, services, &scope, "finance.receivables.document.email", json!({"document_id": issued.id})).await?;
    assert_eq!(offer["enabled"], false);
    assert!(offer["refusal"].as_str().unwrap().contains("turned off"));
    refused(&db, services, &scope, send, json!({"document_id": issued.id}), "Invoice email is turned off for this company").await?;
    refused(&db, services, &scope, "finance.receivables.document.issue", json!({"document_id": ar.create_draft(&scope, COMPANY, &draft(with_email)).await?.id, "send_email": true}), "is not offered").await?;
    println!("PASS: invoice email is off until turned on; send and issue-and-send are refused in words");

    // The setting: refused values, then on with the company's own identity.
    let configure = "finance.receivables.setup.configure";
    refused(&db, services, &scope, configure, json!({"invoice_email":{"reply_to":"not an address"}}), "is not an email address").await?;
    refused(&db, services, &scope, configure, json!({"invoice_email":{"subject":"Invoice {numbr}"}}), "which Bilibi cannot fill in").await?;
    refused(&db, services, &scope, configure, json!({"invoice_email":{"from_name":"Two\nlines"}}), "must be one line").await?;
    refused(&db, services, &scope, configure, json!({"invoice_email":{"sender":"x"}}), "is invalid").await?;
    write(&db, services, &scope, configure, json!({"invoice_email":{"enabled":true,"from_name":"Example Trading Accounts","reply_to":"accounts@example.test"}})).await?;
    let setup = ar.get_setup(&scope, COMPANY).await?;
    assert!(setup.invoice_email.enabled && setup.invoice_email.subject.is_empty());
    assert_eq!(setup.receivable_account_id, account("1100"));
    println!("PASS: invoice email settings validate the reply address, the words Bilibi fills in and the sender name; turning it on leaves the accounts alone");

    // What would go out, in the built-in words, before anything is sent.
    let offer = read(&db, services, &scope, "finance.receivables.document.email", json!({"document_id": issued.id})).await?;
    let email = &offer["email"];
    assert_eq!(email["to"], "jamie@example.test");
    assert_eq!(email["from_name"], "Example Trading Accounts");
    assert_eq!(email["reply_to"], "accounts@example.test");
    assert_eq!(email["subject"].as_str().unwrap().split(" from ").next(), Some(format!("Invoice {reference}").as_str()));
    let message = email["message"].as_str().unwrap();
    assert!(message.contains("Hello Jamie Mail Buyer") && message.contains("1,250.00") && message.contains("2 Dec 2026"), "{message}");
    assert_eq!(email["attachment"], format!("invoice-{reference}.pdf"));

    // Refusals: a draft, a customer with no address, an address that is not one.
    let waiting = ar.create_draft(&scope, COMPANY, &draft(with_email)).await?;
    refused(&db, services, &scope, send, json!({"document_id": waiting.id}), "Only an issued invoice can be sent").await?;
    let orphan = ar.create_draft(&scope, COMPANY, &draft(without_email)).await?;
    let orphan = ar.issue_document(&scope, COMPANY, orphan.id).await?;
    refused(&db, services, &scope, send, json!({"document_id": orphan.id}), "This customer has no email address").await?;
    refused(&db, services, &scope, send, json!({"document_id": orphan.id, "to": "nobody"}), "is not an email address").await?;
    let offer = read(&db, services, &scope, "finance.receivables.document.email", json!({"document_id": orphan.id})).await?;
    assert!(offer["no_address"].as_str().unwrap().contains("no email address"));
    assert_eq!(offer["email"]["to"], "");
    println!("PASS: a draft, a customer with no email address and a bad address are refused before anything is recorded");

    // A send is one recorded row on the invoice; a second to the same address waits for the first.
    let sent = write(&db, services, &scope, send, json!({"document_id": issued.id})).await?;
    assert_eq!(sent["review"]["changes"]["email"]["to"], "jamie@example.test");
    assert_eq!(sent["record"]["state"], "queued");
    assert_eq!(sent["record"]["requested_by_user_id"], scope.user());
    refused(&db, services, &scope, send, json!({"document_id": issued.id}), "already on its way to jamie@example.test").await?;
    let typed = write(&db, services, &scope, send, json!({"document_id": orphan.id, "to": "ap@buyer.example.test", "subject": "Your invoice", "message": "Hello,\n\nHere it is.\n"})).await?;
    assert_eq!(typed["record"]["to_email"], "ap@buyer.example.test");
    assert_eq!(typed["record"]["subject"], "Your invoice");
    let history = ar.get_document(&scope, COMPANY, issued.id).await?.emails;
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].to_email.as_str(), history[0].state.as_str()), ("jamie@example.test", "queued"));
    assert_eq!(history[0].attachment_name, format!("invoice-{reference}.pdf"));
    println!("PASS: a send is recorded on the invoice with who asked, to whom and which PDF; a typed address and words are kept as sent");

    // Issue and send: offered by a second setting, one reviewed step.
    write(&db, services, &scope, configure, json!({"invoice_email":{"issue_and_send":true,"subject":"{number} from {company}, due {due_date}"}})).await?;
    let issue = "finance.receivables.document.issue";
    refused(&db, services, &scope, issue, json!({"document_id": ar.create_draft(&scope, COMPANY, &draft(without_email)).await?.id, "send_email": true}), "This customer has no email address").await?;
    let both = write(&db, services, &scope, issue, json!({"document_id": waiting.id, "send_email": true})).await?;
    assert_eq!(both["review"]["changes"]["email"]["send"], true);
    assert_eq!(both["review"]["changes"]["email"]["to"], "jamie@example.test");
    let both = ar.get_document(&scope, COMPANY, waiting.id).await?;
    assert_eq!(both.status, "issued");
    assert_eq!(both.emails.len(), 1);
    assert!(both.emails[0].subject.ends_with("due 2 Dec 2026"), "{}", both.emails[0].subject);
    let plain = ar.create_draft(&scope, COMPANY, &draft(with_email)).await?;
    let plain = write(&db, services, &scope, issue, json!({"document_id": plain.id})).await?;
    assert_eq!(plain["review"]["changes"]["email"]["send"], false);
    assert!(ar.get_document(&scope, COMPANY, plain["record"]["id"].as_i64().unwrap()).await?.emails.is_empty());
    println!("PASS: Issue and send issues and queues the email in one reviewed step; Issue alone sends nothing");

    // Someone without the issue permission cannot send; another tenant cannot read.
    let viewer = sign_in(&db, services, 1, 3).await?;
    assert!(write(&db, services, &viewer, send, json!({"document_id": plain["record"]["id"]})).await.is_err());
    let other = sign_in(&db, services, 2, 5).await?;
    assert!(read(&db, services, &other, "finance.receivables.document.email", json!({"document_id": issued.id})).await.is_err());
    let evidence = ar.evidence_page(&scope, COMPANY, "document_email", None, None).await?;
    assert_eq!(evidence.rows.len(), 3);
    println!("PASS: sending needs the issue permission, another tenant reads nothing, and the sends are auditor evidence");

    let Ok(root) = std::env::var("INVOICE_EMAIL_PROOF_DIR") else {
        println!("Delivery not proved: set INVOICE_EMAIL_PROOF_DIR to run the queue worker and read the development mailbox.");
        return Ok(());
    };
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let admin = sign_in(&db, services, 1, 4).await?;
    bilibi_finance_walkthrough_fixture::attachments::configure_storage(&db, services, &admin, &root).await?;
    if let Ok(binary) = std::env::var("INVOICE_LAYOUT_CHROMIUM") {
        let settings = services.get::<bilibi_settings::Settings>("base/settings.store")?;
        let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
        let mut client = db.get().await?;
        let mut tx = boundary.begin(&mut client, admin.mutation_context("walkthrough.invoice-email.configure")).await?;
        settings.put(&mut tx, &admin, bilibi_settings::Target::Global, "pdf.chromium.binary", json!(binary)).await?;
        tx.commit().await?;
    }
    let mailer = services.get::<bilibi_mail::Mailer>(bilibi_mail::SERVICE)?;
    let mailbox = mailer.dev_mailbox().ok_or("this proof needs the development mailbox: unset MAIL_HOST")?;
    let before = mailbox.entries()?.len();
    let executor = Executor::from_services(services, Config { concurrency: 2, poll_interval: Duration::from_millis(100), lease_seconds: 120, max_runtime: Duration::from_secs(120) })?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    let delivered = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let mut waiting = false;
            for id in [issued.id, orphan.id, both.id] {
                waiting |= ar.get_document(&scope, COMPANY, id).await?.emails.iter().any(|e| e.state == "queued");
            }
            if !waiting {
                return Ok::<_, bilibi_finance_receivables::ReceivableError>(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    let _ = stop.send(true);
    worker.await??;
    delivered??;
    let record = &ar.get_document(&scope, COMPANY, issued.id).await?.emails[0];
    assert_eq!(record.state, "sent", "{:?}", record.reason);
    assert!(record.attachment_bytes.unwrap() > 1000 && record.attachment_sha256.as_deref().unwrap().len() == 64);
    assert!(record.settled_at.is_some());
    let entries = mailbox.entries()?;
    assert_eq!(entries.len(), before + 3, "three sends, three messages");
    let mut found = false;
    for entry in &entries {
        let raw = mailbox.read(&entry.id)?;
        let parsed = bilibi_mail::parse(raw.as_bytes(), &mailer.parse_limits().await?)?;
        if parsed.to.iter().any(|to| to.email == "jamie@example.test") && parsed.subject.as_deref().is_some_and(|s| s.starts_with(&format!("Invoice {reference}"))) {
            assert_eq!(parsed.from.as_ref().and_then(|from| from.name.as_deref()), Some("Example Trading Accounts"));
            assert_eq!(parsed.reply_to.first().map(|reply| reply.email.as_str()), Some("accounts@example.test"));
            assert!(parsed.text.as_deref().unwrap().contains("Hello Jamie Mail Buyer"));
            let pdf = &parsed.attachments[0];
            assert_eq!(pdf.filename.as_deref(), Some(format!("invoice-{reference}.pdf").as_str()));
            assert!(pdf.bytes.starts_with(b"%PDF-"));
            assert_eq!(i64::try_from(pdf.bytes.len())?, record.attachment_bytes.unwrap());
            std::fs::write(format!("{root}/sent.eml"), &raw)?;
            std::fs::write(format!("{root}/sent.pdf"), &pdf.bytes)?;
            found = true;
        }
    }
    assert!(found, "the invoice's message is in the development mailbox");

    // Turned off after a send was asked for: the delivery is cancelled, not sent.
    write(&db, services, &scope, send, json!({"document_id": plain["record"]["id"]})).await?;
    write(&db, services, &scope, configure, json!({"invoice_email":{"enabled":false}})).await?;
    let executor = Executor::from_services(services, Config { concurrency: 1, poll_interval: Duration::from_millis(100), lease_seconds: 60, max_runtime: Duration::from_secs(60) })?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    let plain_id = plain["record"]["id"].as_i64().unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let emails = ar.get_document(&scope, COMPANY, plain_id).await?.emails;
            if emails[0].state != "queued" {
                return Ok::<_, bilibi_finance_receivables::ReceivableError>(emails);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    let _ = stop.send(true);
    worker.await??;
    let settled = settled??;
    assert_eq!((settled[0].state.as_str(), settled[0].reason.as_deref()), ("cancelled", Some("turned_off")));
    assert_eq!(mailbox.entries()?.len(), before + 3, "nothing more was sent");
    println!("PASS: the worker delivered three messages into the development mailbox with the PDF attached (size and SHA-256 recorded), and cancelled a send once invoice email was turned off; files in {root}");
    Ok(())
}
