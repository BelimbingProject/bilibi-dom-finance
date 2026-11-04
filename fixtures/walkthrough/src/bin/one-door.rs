//! Provision real OAuth credentials for the HTTP/MCP ledger walkthrough on
//! an explicitly disposable database. No book is opened here: the wire proof
//! starts from an empty ledger. Never print credentials.
use bilibi_core::agent::{
    code_challenge, AgentAccess, AuthorizeParams, ClientRegistration, CodeExchange, Decision,
    CLIENT_ID,
};
use bilibi_database::{pool, Error};
use bilibi_finance_walkthrough_fixture::sign_in;
use serde_json::json;
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
        return Err(Error::Invalid.into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN")?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    let services = &app.services;
    let client = db.get().await?;
    let admin = client
        .query_one(
            "SELECT id,tenant_id FROM users WHERE email='ai@agent.my'",
            &[],
        )
        .await?;
    let admin_id: i64 = admin.get(0);
    let tenant: i64 = admin.get(1);
    // Fixture identities only, on the launcher's expressly named disposable database.
    let name: String = client
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    if name != "onedoor_finance_ledger" {
        return Err(Error::Invalid.into());
    }
    let company: i64 = client
        .query_one(
            "SELECT company_id FROM tenant_primary_companies WHERE tenant_id=$1",
            &[&tenant],
        )
        .await?
        .get(0);
    if std::env::args().nth(1).as_deref() == Some("policies") {
        drop(client);
        let scope = sign_in(&db, services, tenant, 10001).await?;
        for target in [company, 10001] {
            bilibi_finance_walkthrough_fixture::initialize_journal_approvals(
                &db, services, &scope, target,
            )
            .await?;
        }
        println!("One-door proof: explicit journal approval policies ready.");
        return Ok(());
    }
    client.execute("INSERT INTO companies(id,tenant_id,name,code,status) VALUES(10001,$1,'One door second company','onedoor_second','active') ON CONFLICT DO NOTHING", &[&tenant]).await?;
    client.execute("INSERT INTO companies(id,tenant_id,name,code,status) VALUES(10003,$1,'One door browser company','onedoor_browser','active') ON CONFLICT DO NOTHING", &[&tenant]).await?;
    for (id, label, email) in [
        (
            10001_i64,
            "One door recorder",
            "onedoor-recorder@example.test",
        ),
        (
            10002_i64,
            "One door reviewer",
            "onedoor-reviewer@example.test",
        ),
    ] {
        client.execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES($1,$2,$3,$4,'human',$5) ON CONFLICT DO NOTHING", &[&id,&tenant,&format!("onedoor_{id}"),&label,&email]).await?;
        client.execute("INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES($1,$2,$1,$3,'Proof accountant') ON CONFLICT DO NOTHING", &[&id,&tenant,&company]).await?;
        client.execute("INSERT INTO users(id,tenant_id,name,email,password_hash,person_id,affiliation_id,organisation_id) SELECT $1,$2,$3,$4,password_hash,$1,$1,$5 FROM users WHERE id=$6 ON CONFLICT DO NOTHING", &[&id,&tenant,&label,&email,&company,&admin_id]).await?;
        client.execute("INSERT INTO principal_roles(tenant_id,user_id,role_id) SELECT tenant_id,$1,role_id FROM principal_roles WHERE tenant_id=$2 AND user_id=$3 ON CONFLICT DO NOTHING", &[&id,&tenant,&admin_id]).await?;
    }
    drop(client);
    let path = std::env::var("BILIBI_AGENT_CREDENTIALS")?;
    let access = services.get::<AgentAccess>("core/agent.grants")?;
    // MCP uses a public, dynamically registered PKCE client (Platform Agent Access).
    let callback = "https://example.test/ledger-proof/callback";
    let registration = access
        .register_client(&serde_json::from_value::<ClientRegistration>(json!({
            "client_name": "Ledger walkthrough", "redirect_uris": [callback],
            "token_endpoint_auth_method": "none"
        }))?)
        .await?;
    let client_id = registration["client_id"]
        .as_str()
        .ok_or("registered client ID")?;
    let resource = format!("{}/mcp", origin.trim_end_matches('/'));
    let mut credentials = serde_json::Map::new();
    for (name, scope) in [
        ("manager", sign_in(&db, services, tenant, 10001).await?),
        ("approver", sign_in(&db, services, tenant, 10002).await?),
    ] {
        let device = access.start_device(CLIENT_ID, "ledger-walkthrough").await?;
        access
            .decide(&scope, &device.user_code, Decision::Approve)
            .await?;
        let api = access.exchange(CLIENT_ID, &device.device_code).await?;
        let verifier = "ledger-walkthrough-verifier-with-at-least-forty-three-characters";
        let params = AuthorizeParams {
            client_id: client_id.into(),
            redirect_uri: callback.into(),
            response_type: "code".into(),
            resource: resource.clone(),
            scope: Some("bilibi".into()),
            code_challenge: code_challenge(verifier),
            code_challenge_method: "S256".into(),
            state: Some("ledger-walkthrough".into()),
        };
        let code = access.authorize(&scope, &params, &resource).await?;
        let mcp = access
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
        credentials.insert(
            name.into(),
            json!({"api":api.access_token,"mcp":mcp.access_token}),
        );
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(serde_json::to_string(&credentials)?.as_bytes())?;
    println!("One-door proof identities ready; OAuth credentials saved privately.");
    Ok(())
}
