use bilibi_agent_api::{Handler, Registry};
use bilibi_base::{mail, queue};
use bilibi_database::{Db, Error, MutationBoundary};
use bilibi_module_registry::Services;
use bilibi_party::OrganisationId;
use bilibi_templates::{Origin, Templates};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::time::Duration;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
const KEY: &str = "finance.receivables.statement_cover";
const PUBLISH: &str = "base.templates.template.publish";
const RESTORE: &str = "base.templates.template.restore";
const DRAFT: &str = "finance.receivables.statement.draft";
pub const SEND: &str = "finance.receivables.statement.send";

async fn change(
    db: &Db,
    services: &Services,
    scope: &Scope,
    operation: &str,
    input: &Value,
) -> Outcome<Value> {
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let Handler::SafeWrite(handler) = registry.operations[operation].definition.handler else {
        panic!("{operation} must be a reviewed write");
    };
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(&mut client, scope.mutation_context(operation))
        .await?;
    handler.preview(tx.sql(), scope, input).await?;
    let committed = handler
        .commit(&mut tx, scope, input)
        .await?
        .envelope(operation);
    tx.commit().await?;
    Ok(committed["result"].clone())
}

pub async fn draft(
    db: &Db,
    services: &Services,
    admin: &Scope,
    clerk: &Scope,
    customer_id: i64,
) -> Outcome<Value> {
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let templates = services.get::<Templates>(bilibi_templates::SERVICE)?;
    let request = json!({"company_id":1,"customer_id":customer_id,"as_of":"2026-11-15"});
    let default = registry.read(db, clerk, DRAFT, &request).await?;
    assert_eq!(default["result"]["slots"]["subject"], "Your statement");
    assert_eq!(default["result"]["template"]["origin"]["scope"], "packaged");
    println!("PASS: an untouched statement inherits the packaged wording");

    let publish = json!({
        "key":KEY, "company_id":1, "language":"en", "revision":0,
        "slots":{"subject":"Statement for {{customer_name}}", "body":"Balance details as of {{statement_date}}.\n\nPlease see the attached statement."},
    });
    let revision = change(db, services, admin, PUBLISH, &publish).await?["revision"]
        .as_i64()
        .expect("published revision");

    let stale = change(db, services, admin, PUBLISH, &publish)
        .await
        .unwrap_err();
    assert!(matches!(
        stale.downcast_ref(),
        Some(bilibi_agent_api::Error::Backend(Error::Conflict))
    ));

    let a = registry.read(db, clerk, DRAFT, &request).await?;
    let slots = &a["result"]["slots"];
    assert_eq!(slots["subject"], "Statement for Orchid Atelier");
    assert_eq!(
        slots["body"],
        "Balance details as of 15 Nov 2026.\n\nPlease see the attached statement."
    );
    assert_eq!(a["result"]["template"]["origin"]["revision"], revision);

    let client = db.get().await?;
    // Both companies are in tenant 1. Keep the current company as 1 while
    // looking up the canonical wording for company 2's business context.
    let b = templates
        .resolve(
            &**client,
            admin,
            KEY,
            Some(OrganisationId::new(2)?),
            "en",
            None,
        )
        .await?;
    assert!(matches!(b.origin, Origin::Packaged { .. }));
    assert_eq!(b.slots["subject"], "Your statement");
    let audit: i64 = client.query_one(
        "SELECT count(*) FROM base_audit_mutations WHERE tenant_id=$1 AND auditable_type='base/templates.revision' AND auditable_id=$2::bigint::text",
        &[&admin.tenant(), &revision],
    ).await?.get(0);
    assert_eq!(audit, 1, "publish must be audited");
    drop(client);
    println!(
        "PASS: company 1 publishes with audit; company 2 inherits; a stale publish is refused"
    );

    let restore = json!({"key":KEY,"company_id":1,"language":"en","revision":revision});
    change(db, services, admin, RESTORE, &restore).await?;
    let inherited = registry.read(db, clerk, DRAFT, &request).await?;
    assert_eq!(inherited["result"]["slots"], default["result"]["slots"]);
    println!("PASS: restoring company 1 returns it to inherited wording");

    Ok(json!({
        "company_id":1, "customer_id":customer_id, "as_of":"2026-11-15",
        "recipient":a["result"]["recipient"], "subject":slots["subject"], "body":slots["body"]
    }))
}

pub async fn delivered(db: &Db, services: &Services, input: &Value) -> Outcome<()> {
    bilibi_walkthrough_fixture::set_instance_role(db, "development").await?;
    let mailer = services.get::<mail::Mailer>(mail::SERVICE)?;
    let mailbox = mailer
        .dev_mailbox()
        .ok_or("statement proof needs the development mailbox")?;
    std::fs::create_dir_all(mailbox.dir())?;
    let limits = mailer.parse_limits().await?;
    let executor = queue::Executor::from_services(services, queue::Config::default())?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(stopped));
    let sent = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            for entry in mailbox.entries()? {
                let message = mail::parse(mailbox.read(&entry.id)?.as_bytes(), &limits)?;
                if message.subject.as_deref() == input["subject"].as_str() {
                    return Ok::<_, Box<dyn std::error::Error>>(message);
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    let _ = stop.send(true);
    worker.await??;
    let sent = sent??;
    assert_eq!(sent.text.as_deref(), input["body"].as_str());
    assert_eq!(
        sent.to.first().map(|address| address.email.as_str()),
        input["recipient"].as_str()
    );
    assert!(sent.html.is_none());
    assert!(sent
        .attachments
        .iter()
        .any(|part| part.declared_type == "application/pdf" && part.bytes.starts_with(b"%PDF-")));
    println!("PASS: the actual plain-text MIME message matches the approved statement subject, body and recipient and carries its PDF");
    Ok(())
}
