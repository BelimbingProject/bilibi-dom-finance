//! The attachments walkthrough: `finance/attachments` on records of the
//! fixture's specimen sources, in a disposable database. Every line is the
//! facade's actual answer, Base Artifacts' own, the queue job's outcome or the
//! database's refusal; nothing here is a test double.
use crate::{
    records::{self, OPEN_SOURCE, RESTRICTED_SOURCE},
    sign_in, COMPANY,
};
use bilibi_authz::grants::Grants;
use bilibi_database::{Change, Db, MutationBoundary};
use bilibi_finance_attachments::{
    Attachment, AttachmentError, AttachmentQuery, Attachments, NewFile, Refresh, MANAGE_RETENTION,
    REFRESH_PRINCIPAL, SERVICE,
};
use bilibi_module_registry::Services;
use bilibi_queue::{Config, Executor};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use jiff::civil::date;
use serde_json::{json, Value};
use std::{fmt::Debug, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use tokio_postgres::Client;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn file(name: &str, content: &[u8], content_type: Option<&str>) -> NewFile {
    NewFile {
        file_name: name.into(),
        content: content.to_vec(),
        content_type: content_type.map(str::to_owned),
    }
}

fn brief(a: &Attachment) -> String {
    format!(
        "#{} {} {} {:?} {} {}B sha256={}.. kept {}y until {} status={} hold={} events=[{}]",
        a.attachment_number,
        a.source_label.as_deref().unwrap_or("-"),
        a.document_label,
        a.file_name,
        a.content_type,
        a.byte_size,
        &a.sha256[..8],
        a.retention_years,
        a.retain_until,
        a.status.as_str(),
        a.on_hold,
        a.events
            .iter()
            .map(|e| {
                let who = e
                    .actor_system_principal
                    .as_deref()
                    .map_or(format!("user {}", e.actor_user_id), |p| {
                        format!("{p} for user {}", e.actor_user_id)
                    });
                format!(
                    "{}{} by {who}{}",
                    e.event,
                    e.retain_until.map_or(String::new(), |u| format!(" to {u}")),
                    e.reason
                        .as_deref()
                        .map_or(String::new(), |r| format!(" ({r})"))
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    )
}

fn show<T: Debug>(step: &str, result: Result<T, AttachmentError>) {
    match result {
        Ok(value) => println!("{step}: {value:?}"),
        Err(AttachmentError::Invalid(errors)) => {
            let errors: Vec<String> = errors
                .iter()
                .map(|e| format!("{} {}", e.field, e.message))
                .collect();
            println!("{step}: refused ({})", errors.join("; "));
        }
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}
fn show_attachment(step: &str, result: Result<Attachment, AttachmentError>) -> Option<Attachment> {
    match result {
        Ok(attachment) => {
            println!("{step}: {}", brief(&attachment));
            Some(attachment)
        }
        Err(error) => {
            show::<()>(step, Err(error));
            None
        }
    }
}

/// Raw SQL from "another client": the database's guards hold without the facade.
async fn raw(client: &Client, label: &str, sql: &str) {
    match client.batch_execute(sql).await {
        Ok(()) => println!("{label}: ok (changed)"),
        Err(error) => println!(
            "{label}: refused by database — {}",
            error.as_db_error().map_or("unavailable", |d| d.message())
        ),
    }
}

async fn job_states(client: &Client) -> Outcome<String> {
    let rows = client
        .query(
            "SELECT id,state,attempt,actor_kind,system_principal,COALESCE(failure_code,'-') \
             FROM base_queue_jobs WHERE worker_id='finance/attachments-retention-refresh' ORDER BY id",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            format!(
                "job {} {} attempt {} as {} {} code={}",
                r.get::<_, i64>(0),
                r.get::<_, String>(1),
                r.get::<_, i32>(2),
                r.get::<_, String>(3),
                r.get::<_, Option<String>>(4).unwrap_or_default(),
                r.get::<_, String>(5)
            )
        })
        .collect::<Vec<_>>()
        .join("\n  "))
}

/// What an operator sets in Operator Settings before Base Artifacts keeps a
/// file: the private storage root and a 30-day ordinary-document policy.
pub async fn configure_storage(
    db: &Db,
    services: &Services,
    admin: &Scope,
    root: &str,
) -> Outcome<()> {
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let settings = services.get::<Settings>("base/settings.store")?;
    for (key, value) in [
        ("artifacts.storage_root", json!(root)),
        ("artifacts.retention_days", json!(30)),
    ] {
        let mut client = db.get().await?;
        let mut tx = boundary
            .begin(
                &mut client,
                admin.mutation_context("attachments.walkthrough.configure"),
            )
            .await?;
        settings
            .put(&mut tx, admin, Target::Global, key, value)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let attachments = services.get::<Attachments>(SERVICE)?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    // Only the manager is affiliated with Company 001; staff and the viewer
    // join it here (company reach is the facade's rule for everyone else).
    db.get()
        .await?
        .batch_execute(
            "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Lee Viewer','human','lee@example.test'),(5,1,'person_5','Sam Staff','human','sam@example.test') ON CONFLICT DO NOTHING; \
             INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','viewer@example.test'),(4,1,5,1,'Clerk','staff@example.test') ON CONFLICT DO NOTHING; \
             UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING; \
             UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=3 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=3 AND person_id IS NOT NULL ON CONFLICT DO NOTHING; \
             SELECT setval(pg_get_serial_sequence('persons','id'),GREATEST((SELECT max(id) FROM persons),1)); \
             SELECT setval(pg_get_serial_sequence('person_affiliations','id'),GREATEST((SELECT max(id) FROM person_affiliations),1))",
        )
        .await?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let other = sign_in(db, services, 2, 5).await?;

    println!("\n== Attachments: storage not configured yet");
    let draft = records::add(db, 1, COMPANY, "open", "Bill 12", date(2026, 3, 15), false).await?;
    show(
        "manager attaches before an operator configured Base Artifacts' storage",
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                draft,
                &file(
                    "bill-12.pdf",
                    b"%PDF-1.4 invoice scan",
                    Some("application/pdf"),
                ),
            )
            .await,
    );

    let root = std::env::var("BILIBI_ATTACHMENTS_DEMO_ROOT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join(format!("bilibi-finance-attachments-{}", std::process::id()))
            .display()
            .to_string()
    });
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    configure_storage(db, services, &admin, &root).await?;
    println!("operator configured storage root {root} and a 30-day ordinary-document policy");

    println!("\n== Retention setting");
    show(
        "default retention",
        attachments.get_retention(&manager, COMPANY).await,
    );
    show(
        "viewer lengthens retention",
        attachments
            .set_retention(&viewer, COMPANY, 10, "Group policy")
            .await,
    );
    show(
        "manager sets 5 years (shorter than the default 7)",
        attachments
            .set_retention(&manager, COMPANY, 5, "Shorter")
            .await,
    );
    show(
        "manager sets 0 years and a blank reason",
        attachments.set_retention(&manager, COMPANY, 0, "  ").await,
    );
    show(
        "manager sets 101 years",
        attachments
            .set_retention(&manager, COMPANY, 101, "Too long")
            .await,
    );
    // Bilimbi lists the changeset's errors by field: reason, then years.
    show(
        "manager sets 101 years and a blank reason",
        attachments.set_retention(&manager, COMPANY, 101, " ").await,
    );
    show(
        "manager lengthens to 8 years",
        attachments
            .set_retention(&manager, COMPANY, 8, "Group policy")
            .await,
    );
    show(
        "manager lengthens to 8 again",
        attachments
            .set_retention(&manager, COMPANY, 8, "Again")
            .await,
    );
    show(
        "manager reads the other tenant's company 5",
        attachments.get_retention(&manager, 5).await,
    );
    show(
        "changes",
        attachments.list_retention_changes(&staff, COMPANY).await,
    );
    show(
        "retain-until for a document dated 2026-03-15 (needs ledger view too)",
        attachments
            .retain_until(&viewer, COMPANY, date(2026, 3, 15))
            .await,
    );
    raw(
        &**db.get().await?,
        "UPDATE of a retention change",
        "UPDATE finance_attachments_retention_changes SET years = 99",
    )
    .await;
    raw(
        &**db.get().await?,
        "a shorter retention change inserted raw",
        "INSERT INTO finance_attachments_retention_changes(tenant_id,company_id,change_number,years,reason,changed_by_user_id,changed_at) VALUES(1,1,NULL,3,'raw',1,NULL)",
    )
    .await;

    println!("\n== Attaching to a draft record");
    let pdf = file(
        "bill-12.pdf",
        b"%PDF-1.4 invoice scan",
        Some("application/pdf"),
    );
    let first = show_attachment(
        "manager attaches an invoice scan",
        attachments
            .attach(&manager, COMPANY, OPEN_SOURCE, draft, &pdf)
            .await,
    );
    let second = show_attachment(
        "staff attaches a remittance advice (no content type given)",
        attachments
            .attach(
                &staff,
                COMPANY,
                OPEN_SOURCE,
                draft,
                &file("  remittance.txt ", b"paid 100.00", None),
            )
            .await,
    );
    show(
        "viewer attaches",
        attachments
            .attach(&viewer, COMPANY, OPEN_SOURCE, draft, &pdf)
            .await,
    );
    show(
        "other tenant attaches",
        attachments
            .attach(&other, COMPANY, OPEN_SOURCE, draft, &pdf)
            .await,
    );
    show(
        "unknown source",
        attachments
            .attach(&manager, COMPANY, "finance/payables/bill", draft, &pdf)
            .await,
    );
    show(
        "record that does not exist",
        attachments
            .attach(&manager, COMPANY, OPEN_SOURCE, 9999, &pdf)
            .await,
    );
    show(
        "record id 0",
        attachments
            .attach(&manager, COMPANY, OPEN_SOURCE, 0, &pdf)
            .await,
    );
    show(
        "empty file",
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                draft,
                &file("a.txt", b"", None),
            )
            .await,
    );
    show(
        "blank file name",
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                draft,
                &file("   ", b"x", None),
            )
            .await,
    );
    show(
        "content type with a space",
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                draft,
                &file("a.txt", b"x", Some("text plain")),
            )
            .await,
    );
    show(
        "file over 25 MiB",
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                draft,
                &file("big.bin", &vec![7u8; 25 * 1024 * 1024 + 1], None),
            )
            .await,
    );
    let old = records::add(db, 1, COMPANY, "open", "Bill 1", date(2015, 1, 5), false).await?;
    show(
        "document dated 2015 (retention would already have passed)",
        attachments
            .attach(&manager, COMPANY, OPEN_SOURCE, old, &pdf)
            .await,
    );
    show(
        "attachments of the record, as the viewer reads them",
        attachments
            .list_attachments(&viewer, COMPANY, OPEN_SOURCE, draft)
            .await
            .map(|l| l.iter().map(brief).collect::<Vec<_>>()),
    );

    println!("\n== Reading");
    if let Some(first) = &first {
        show(
            "get_attachment",
            attachments
                .get_attachment(&viewer, COMPANY, first.id)
                .await
                .map(|a| brief(&a)),
        );
        match attachments
            .read_attachment(&viewer, COMPANY, first.id)
            .await
        {
            Ok(f) => println!(
                "read_attachment: {} {} {}B sha256 ok={} {:?}",
                f.file_name,
                f.content_type,
                f.byte_size,
                f.sha256 == first.sha256,
                String::from_utf8_lossy(&f.bytes)
            ),
            Err(e) => println!("read_attachment: refused ({e:?})"),
        }
        show(
            "get an attachment of another company's number",
            attachments
                .get_attachment(&manager, COMPANY, 9999)
                .await
                .map(|a| brief(&a)),
        );
        show(
            "other tenant reads it",
            attachments
                .get_attachment(&other, COMPANY, first.id)
                .await
                .map(|a| brief(&a)),
        );
    }
    let restricted = records::add(
        db,
        1,
        COMPANY,
        "restricted",
        "Journal 3",
        date(2026, 4, 2),
        false,
    )
    .await?;
    show_attachment(
        "manager attaches to a restricted record",
        attachments
            .attach(
                &manager,
                COMPANY,
                RESTRICTED_SOURCE,
                restricted,
                &file("voucher.pdf", b"%PDF-1.4 voucher", Some("application/pdf")),
            )
            .await,
    );
    show(
        "staff lists the restricted record's attachments",
        attachments
            .list_attachments(&staff, COMPANY, RESTRICTED_SOURCE, restricted)
            .await
            .map(|l| l.len()),
    );
    show(
        "staff lists the company's attachments (the restricted source is left out)",
        attachments
            .list_company_attachments(&staff, COMPANY, &AttachmentQuery::default())
            .await
            .map(|l| l.iter().map(|a| a.attachment_number).collect::<Vec<_>>()),
    );
    show(
        "manager lists the company's attachments",
        attachments
            .list_company_attachments(&manager, COMPANY, &AttachmentQuery::default())
            .await
            .map(|l| l.iter().map(|a| a.attachment_number).collect::<Vec<_>>()),
    );
    show(
        "kinds of record (staff)",
        attachments.list_sources(&staff, COMPANY).await,
    );
    show(
        "kinds of record (manager)",
        attachments.list_sources(&manager, COMPANY).await,
    );
    show(
        "limit 0",
        attachments
            .list_company_attachments(
                &manager,
                COMPANY,
                &AttachmentQuery {
                    limit: Some(0),
                    ..Default::default()
                },
            )
            .await
            .map(|l| l.len()),
    );
    show(
        "status 'bogus'",
        attachments
            .list_company_attachments(
                &manager,
                COMPANY,
                &AttachmentQuery {
                    status: Some("bogus".into()),
                    ..Default::default()
                },
            )
            .await
            .map(|l| l.len()),
    );
    show(
        "page of 2 after number 1",
        attachments
            .list_company_attachments(
                &manager,
                COMPANY,
                &AttachmentQuery {
                    limit: Some(2),
                    after: Some(1),
                    ..Default::default()
                },
            )
            .await
            .map(|l| l.iter().map(|a| a.attachment_number).collect::<Vec<_>>()),
    );
    show(
        "viewer opens the restricted attachment directly",
        attachments
            .get_attachment(&viewer, COMPANY, 3)
            .await
            .map(|a| brief(&a)),
    );

    println!("\n== Replace and remove on a draft record");
    if let Some(first) = &first {
        let replaced = show_attachment(
            "manager replaces the scan",
            attachments
                .replace(
                    &manager,
                    COMPANY,
                    first.id,
                    &file(
                        "bill-12-v2.pdf",
                        b"%PDF-1.4 corrected scan",
                        Some("application/pdf"),
                    ),
                )
                .await,
        );
        show(
            "replace the replaced one again",
            attachments
                .replace(&manager, COMPANY, first.id, &pdf)
                .await
                .map(|a| brief(&a)),
        );
        show(
            "viewer replaces",
            attachments
                .replace(
                    &viewer,
                    COMPANY,
                    replaced.as_ref().map_or(0, |a| a.id),
                    &pdf,
                )
                .await
                .map(|a| brief(&a)),
        );
        show_attachment(
            "the old scan after replacement",
            attachments
                .get_attachment(&manager, COMPANY, first.id)
                .await,
        );
        match attachments
            .read_attachment(&manager, COMPANY, first.id)
            .await
        {
            Ok(f) => println!(
                "replaced scan's bytes are still readable: {:?}",
                String::from_utf8_lossy(&f.bytes)
            ),
            Err(e) => println!("replaced scan's bytes: refused ({e:?})"),
        }
    }
    if let Some(second) = &second {
        show(
            "staff removes without the delete grant",
            attachments
                .remove(&staff, COMPANY, second.id, "Duplicate")
                .await
                .map(|a| brief(&a)),
        );
        show(
            "manager removes with a blank reason",
            attachments
                .remove(&manager, COMPANY, second.id, "  ")
                .await
                .map(|a| brief(&a)),
        );
        show(
            "manager removes with a 501-character reason",
            attachments
                .remove(&manager, COMPANY, second.id, &"x".repeat(501))
                .await
                .map(|a| brief(&a)),
        );
        show_attachment(
            "manager removes the remittance",
            attachments
                .remove(&manager, COMPANY, second.id, "Uploaded to the wrong bill")
                .await,
        );
        show(
            "remove it again",
            attachments
                .remove(&manager, COMPANY, second.id, "Again")
                .await
                .map(|a| brief(&a)),
        );
        match attachments
            .read_attachment(&manager, COMPANY, second.id)
            .await
        {
            Ok(f) => println!(
                "removed remittance's bytes are still readable: {:?}",
                String::from_utf8_lossy(&f.bytes)
            ),
            Err(e) => println!("removed remittance's bytes: refused ({e:?})"),
        }
    }
    show(
        "standing attachments of the bill",
        attachments
            .list_attachments(&manager, COMPANY, OPEN_SOURCE, draft)
            .await
            .map(|l| l.iter().map(|a| a.attachment_number).collect::<Vec<_>>()),
    );
    show(
        "replaced attachments of the company",
        attachments
            .list_company_attachments(
                &manager,
                COMPANY,
                &AttachmentQuery {
                    status: Some("replaced".into()),
                    ..Default::default()
                },
            )
            .await
            .map(|l| l.iter().map(|a| a.attachment_number).collect::<Vec<_>>()),
    );

    println!("\n== A final record keeps its attachments");
    let posted = records::add(
        db,
        1,
        COMPANY,
        "open",
        "Invoice 40",
        date(2026, 5, 20),
        true,
    )
    .await?;
    let on_posted = show_attachment(
        "manager attaches to a final record",
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                posted,
                &file(
                    "contract.pdf",
                    b"%PDF-1.4 contract",
                    Some("application/pdf"),
                ),
            )
            .await,
    );
    if let Some(on_posted) = &on_posted {
        show(
            "replace it",
            attachments
                .replace(&manager, COMPANY, on_posted.id, &pdf)
                .await
                .map(|a| brief(&a)),
        );
        show(
            "remove it",
            attachments
                .remove(&manager, COMPANY, on_posted.id, "Wrong file")
                .await
                .map(|a| brief(&a)),
        );
    }
    show_attachment(
        "another attachment is still added",
        attachments
            .attach(
                &staff,
                COMPANY,
                OPEN_SOURCE,
                posted,
                &file(
                    "delivery-note.pdf",
                    b"%PDF-1.4 note",
                    Some("application/pdf"),
                ),
            )
            .await,
    );

    println!("\n== Capture before the record exists, attach inside the caller's transaction");
    let capture = attachments
        .capture_file(
            &manager,
            COMPANY,
            OPEN_SOURCE,
            &file(
                "statement.csv",
                b"date,amount\n2026-06-30,100.00\n",
                Some("text/csv"),
            ),
            date(2026, 6, 30),
        )
        .await?;
    println!("captured: {capture:?}");
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("attachments.walkthrough.statement"),
        )
        .await?;
    let statement = tx
        .mutate(|sql| {
            Box::pin(async move {
                let id: i64 = sql
                    .query_one("INSERT INTO finance_walkthrough_records(tenant_id,company_id,kind,label,record_date,is_final) VALUES(1,1,'open','Statement 6',DATE '2026-06-30',true) RETURNING id", &[])
                    .await?
                    .get(0);
                Ok((id, vec![Change { kind: "finance.walkthrough.record".into(), id, old: Value::Null, new: json!({"label": "Statement 6"}) }]))
            })
        })
        .await?;
    show_attachment(
        "attach_capture inside the transaction that inserts the record",
        attachments
            .attach_capture(&mut tx, &manager, COMPANY, OPEN_SOURCE, statement, &capture)
            .await,
    );
    tx.commit().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("attachments.walkthrough.statement"),
        )
        .await?;
    let mismatched = tx
        .mutate(|sql| {
            Box::pin(async move {
                let id: i64 = sql
                    .query_one("INSERT INTO finance_walkthrough_records(tenant_id,company_id,kind,label,record_date,is_final) VALUES(1,1,'open','Statement 7',DATE '2026-07-31',true) RETURNING id", &[])
                    .await?
                    .get(0);
                Ok((id, vec![Change { kind: "finance.walkthrough.record".into(), id, old: Value::Null, new: json!({"label": "Statement 7"}) }]))
            })
        })
        .await?;
    show_attachment(
        "the same capture for a record dated another day",
        attachments
            .attach_capture(
                &mut tx,
                &manager,
                COMPANY,
                OPEN_SOURCE,
                mismatched,
                &capture,
            )
            .await,
    );
    tx.rollback().await?;
    show("the capture for another source", {
        let mut tx = boundary
            .begin(
                &mut client,
                manager.mutation_context("attachments.walkthrough.statement"),
            )
            .await?;
        let result = attachments
            .attach_capture(
                &mut tx,
                &manager,
                COMPANY,
                RESTRICTED_SOURCE,
                restricted,
                &capture,
            )
            .await
            .map(|a| brief(&a));
        tx.rollback().await?;
        result
    });

    drop(client);
    println!("\n== Retention of one attachment: extension and legal hold");
    if let Some(on_posted) = &on_posted {
        show(
            "staff extends (no retention grant)",
            attachments
                .extend_retention(&staff, COMPANY, on_posted.id, date(2040, 1, 1), "Audit")
                .await
                .map(|a| brief(&a)),
        );
        show(
            "manager extends with a blank reason",
            attachments
                .extend_retention(&manager, COMPANY, on_posted.id, date(2040, 1, 1), " ")
                .await
                .map(|a| brief(&a)),
        );
        show(
            "manager extends to an earlier date",
            attachments
                .extend_retention(&manager, COMPANY, on_posted.id, date(2030, 1, 1), "Audit")
                .await
                .map(|a| brief(&a)),
        );
        show_attachment(
            "manager extends to 2040-01-01",
            attachments
                .extend_retention(
                    &manager,
                    COMPANY,
                    on_posted.id,
                    date(2040, 1, 1),
                    "Tax audit 2026",
                )
                .await,
        );
        show(
            "manager extends to the same date",
            attachments
                .extend_retention(&manager, COMPANY, on_posted.id, date(2040, 1, 1), "Again")
                .await
                .map(|a| brief(&a)),
        );
        show(
            "release a hold that is not there",
            attachments
                .release_hold(&manager, COMPANY, on_posted.id, "No hold")
                .await
                .map(|a| brief(&a)),
        );
        show(
            "place a hold without a reason",
            attachments
                .place_hold(&manager, COMPANY, on_posted.id, "")
                .await
                .map(|a| brief(&a)),
        );
        show_attachment(
            "manager places a legal hold",
            attachments
                .place_hold(&manager, COMPANY, on_posted.id, "Case 12/2026")
                .await,
        );
        show(
            "a second hold",
            attachments
                .place_hold(&manager, COMPANY, on_posted.id, "Again")
                .await
                .map(|a| brief(&a)),
        );
        raw(
            &**db.get().await?,
            "the artifact's retain-until moved earlier raw",
            &format!("UPDATE base_artifacts SET retain_until = retain_until - interval '1 year' WHERE public_id = '{}'", on_posted.artifact_id),
        )
        .await;
        raw(
            &**db.get().await?,
            "the held artifact's row deleted raw",
            &format!(
                "DELETE FROM base_artifacts WHERE public_id = '{}'",
                on_posted.artifact_id
            ),
        )
        .await;
        show_attachment(
            "manager releases the hold",
            attachments
                .release_hold(&manager, COMPANY, on_posted.id, "Case closed")
                .await,
        );
        println!(
            "-- Base Artifacts changed out of band (an operator extends and holds; the module has no event yet)"
        );
        raw(
            &**db.get().await?,
            "operator extends the artifact in Base and holds it",
            &format!(
                "UPDATE base_artifacts SET retain_until = retain_until + interval '2 years', legal_hold_at = clock_timestamp() WHERE public_id = '{id}'; \
                 INSERT INTO base_artifact_retention_events(artifact_id,tenant_id,company_id,owner_id,event,retain_until,legal_hold,reason,actor_type,actor_id,trace_id) \
                 SELECT id,tenant_id,company_id,owner_id,'hold_placed',retain_until,true,'Regulator request','user',4,'walkthrough' FROM base_artifacts WHERE public_id = '{id}'",
                id = on_posted.artifact_id
            ),
        )
        .await;
        show_attachment(
            "the next read brings the events up to Base",
            attachments
                .get_attachment(&manager, COMPANY, on_posted.id)
                .await,
        );
        raw(
            &**db.get().await?,
            "release it again so the walkthrough's artifacts can age out",
            &format!(
                "UPDATE base_artifacts SET legal_hold_at = NULL WHERE public_id = '{}'",
                on_posted.artifact_id
            ),
        )
        .await;
    }

    println!("\n== Retention follows the record's final date (queue job as the system principal)");
    let moving = records::add(
        db,
        1,
        COMPANY,
        "open",
        "Invoice 41 (draft)",
        date(2025, 11, 30),
        false,
    )
    .await?;
    show_attachment(
        "manager attaches to a draft dated 2025-11-30",
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                moving,
                &file("quote.pdf", b"%PDF-1.4 quote", Some("application/pdf")),
            )
            .await,
    );
    show(
        "refresh while the record is still a draft",
        finalise(db, &boundary, &attachments, &manager, moving).await?,
    );
    records::set(db, moving, date(2026, 10, 1), true).await?;
    show(
        "viewer, who finalised the record and holds no attachments grant but reads the ledger, queues the refresh",
        finalise(db, &boundary, &attachments, &viewer, moving).await?,
    );
    println!("jobs:\n  {}", job_states(&**db.get().await?).await?);
    let executor = Executor::from_services(
        services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(60),
        },
    )?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    tokio::time::sleep(Duration::from_secs(3)).await;
    println!(
        "before the tenant granted the principal anything:\n  {}",
        job_states(&**db.get().await?).await?
    );
    let grants = services.get::<Grants>("base/authz.grants")?;
    grants
        .set_system(&admin, REFRESH_PRINCIPAL, MANAGE_RETENTION, Some(true))
        .await?;
    println!("tenant administrator granted {REFRESH_PRINCIPAL} {MANAGE_RETENTION}");
    show(
        "refresh again (the earlier job was cancelled)",
        finalise(db, &boundary, &attachments, &viewer, moving).await?,
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    println!(
        "after the grant:\n  {}",
        job_states(&**db.get().await?).await?
    );
    let _ = stop.send(true);
    let _ = worker.await;
    show(
        "the draft's scan now follows the final date 2026-10-01",
        attachments
            .list_attachments(&manager, COMPANY, OPEN_SOURCE, moving)
            .await
            .map(|l| l.iter().map(brief).collect::<Vec<_>>()),
    );
    show(
        "refresh once more (already kept long enough)",
        finalise(db, &boundary, &attachments, &viewer, moving).await?,
    );
    show(
        "refresh by someone who cannot read the record's source",
        finalise(db, &boundary, &attachments, &other, moving).await?,
    );

    println!("\n== Revoked grant, archived company, evidence");
    raw(
        &**db.get().await?,
        "a direct deny of finance.attachments.create for staff",
        "INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,3,'finance.attachments.create',false) ON CONFLICT DO NOTHING",
    )
    .await;
    show(
        "staff attaches after the deny",
        attachments
            .attach(&staff, COMPANY, OPEN_SOURCE, draft, &pdf)
            .await
            .map(|a| brief(&a)),
    );
    raw(&**db.get().await?, "the deny removed", "DELETE FROM principal_capabilities WHERE tenant_id=1 AND user_id=3 AND capability='finance.attachments.create'").await;
    let archived_record = records::add(
        db,
        1,
        2,
        "open",
        "Bill 9 (company 002)",
        date(2026, 2, 2),
        false,
    )
    .await?;
    show_attachment(
        "administrator attaches to company 002 (Base Artifacts binds a person affiliated with Company 001 to it)",
        attachments
            .attach(&admin, 2, OPEN_SOURCE, archived_record, &pdf)
            .await,
    );
    raw(
        &**db.get().await?,
        "company 002 archived",
        "UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=2",
    )
    .await;
    show(
        "administrator attaches to the archived company",
        attachments
            .attach(&admin, 2, OPEN_SOURCE, archived_record, &pdf)
            .await
            .map(|a| brief(&a)),
    );
    show(
        "administrator sets its retention",
        attachments
            .set_retention(&admin, 2, 12, "Group policy")
            .await
            .map(|r| r.years),
    );
    show(
        "administrator still reads the archived company's attachments",
        attachments
            .list_company_attachments(&admin, 2, &AttachmentQuery::default())
            .await
            .map(|l| l.len()),
    );
    raw(
        &**db.get().await?,
        "company 002 restored",
        "UPDATE companies SET status='active' WHERE tenant_id=1 AND id=2",
    )
    .await;
    for dataset in Attachments::evidence_datasets() {
        match attachments
            .evidence_page(&manager, COMPANY, dataset, Some(2), None)
            .await
        {
            Ok(page) => println!(
                "evidence {dataset}: {} fields, {} rows, next={:?}, first row {}",
                page.fields.len(),
                page.rows.len(),
                page.next,
                page.rows.first().map_or(String::new(), |r| r.to_string())
            ),
            Err(e) => println!("evidence {dataset}: refused ({e:?})"),
        }
    }
    show(
        "evidence for staff leaves out the restricted source",
        attachments
            .evidence_page(&staff, COMPANY, "attachment", None, None)
            .await
            .map(|p| {
                p.rows
                    .iter()
                    .filter_map(|r| r["source"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            }),
    );
    show(
        "evidence of an unknown dataset",
        attachments
            .evidence_page(&manager, COMPANY, "journal", None, None)
            .await
            .map(|p| p.rows.len()),
    );
    show(
        "evidence for the viewer's page after the last id",
        attachments
            .evidence_page(&viewer, COMPANY, "retention_change", Some(1), Some(1))
            .await
            .map(|p| (p.rows.len(), p.next)),
    );

    println!("\n== The database's own refusals");
    let raw_client = db.get().await?;
    raw(
        &raw_client,
        "UPDATE of an attachment",
        "UPDATE finance_attachments SET file_name = 'edited'",
    )
    .await;
    raw(
        &raw_client,
        "DELETE of an attachment",
        "DELETE FROM finance_attachments",
    )
    .await;
    raw(
        &raw_client,
        "TRUNCATE of the events",
        "TRUNCATE finance_attachment_events",
    )
    .await;
    raw(
        &raw_client,
        "DELETE of an event",
        "DELETE FROM finance_attachment_events",
    )
    .await;
    raw(
        &raw_client,
        "a second removal of one attachment",
        "INSERT INTO finance_attachment_events(tenant_id,company_id,attachment_id,event,reason,actor_user_id,occurred_at) SELECT tenant_id,company_id,id,'removed','raw',1,NULL FROM finance_attachments WHERE id = (SELECT attachment_id FROM finance_attachment_events WHERE event='removed' LIMIT 1)",
    )
    .await;
    raw(
        &raw_client,
        "an attachment replaced by one of another record",
        "INSERT INTO finance_attachment_events(tenant_id,company_id,attachment_id,event,replacement_id,actor_user_id,occurred_at) VALUES(1,1,(SELECT min(id) FROM finance_attachments WHERE document_id <> (SELECT document_id FROM finance_attachments ORDER BY id DESC LIMIT 1)),'replaced',(SELECT max(id) FROM finance_attachments),1,NULL)",
    )
    .await;
    raw(&raw_client, "a hold event without a reason", "INSERT INTO finance_attachment_events(tenant_id,company_id,attachment_id,event,actor_user_id,occurred_at) VALUES(1,1,(SELECT min(id) FROM finance_attachments),'hold_placed',1,NULL)").await;
    raw(&raw_client, "an attachment with a bad digest", "INSERT INTO finance_attachments(tenant_id,company_id,attachment_number,source,source_module,document_id,document_label,document_date,artifact_id,file_name,content_type,byte_size,sha256,retention_years,retain_until,captured_by_user_id,captured_at) VALUES(1,1,NULL,'s','m',1,'l',DATE '2026-01-01',gen_random_uuid(),'f','t/t',1,'xyz',7,TIMESTAMP '2040-01-01',1,NULL)").await;
    let numbers = raw_client
        .query_one("SELECT count(*), max(attachment_number), count(DISTINCT attachment_number) FROM finance_attachments WHERE company_id = 1", &[])
        .await?;
    println!(
        "company 1 numbered {} attachments up to #{} with {} distinct numbers (no gaps)",
        numbers.get::<_, i64>(0),
        numbers.get::<_, Option<i64>>(1).unwrap_or(0),
        numbers.get::<_, i64>(2)
    );
    let _ = Arc::strong_count(&attachments);
    Ok(())
}

/// What an owning module does when it finalises a record: queue the retention
/// refresh inside the finalising transaction.
async fn finalise(
    db: &Db,
    boundary: &MutationBoundary,
    attachments: &Attachments,
    scope: &Scope,
    record: i64,
) -> Outcome<Result<Refresh, AttachmentError>> {
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            scope.mutation_context("attachments.walkthrough.finalise"),
        )
        .await?;
    let outcome = attachments
        .refresh_retention(&mut tx, scope, COMPANY, OPEN_SOURCE, record)
        .await;
    match &outcome {
        Ok(_) => tx.commit().await?,
        Err(_) => tx.rollback().await?,
    }
    Ok(outcome)
}
