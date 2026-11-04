//! Real OAuth identities for the disposable Fixed Assets wire proof.
use crate::sign_in;
use bilibi_database::{Db, Error, MutationBoundary};
use bilibi_module_registry::Services;
use bilibi_settings::{Settings, Target};
use serde_json::json;
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
pub async fn credentials(db: &Db, services: &Services) -> Result<(), Box<dyn std::error::Error>> {
    use bilibi_core::agent::{
        code_challenge, AgentAccess, AuthorizeParams, ClientRegistration, CodeExchange,
    };
    if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
        return Err(Error::Invalid.into());
    }
    let admin = sign_in(db, services, 1, 4).await?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let mut c = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut c,
            admin.mutation_context("walkthrough.assets.credentials"),
        )
        .await?;
    for key in [
        "agent_api.mcp_requests_per_minute",
        "agent_access.oauth_requests_per_minute",
    ] {
        settings
            .put(&mut tx, &admin, Target::Global, key, json!(10000))
            .await?;
    }
    settings
        .put(
            &mut tx,
            &admin,
            Target::Global,
            "instance.role",
            json!("development"),
        )
        .await?;
    tx.commit().await?;
    drop(c);
    let access = services.get::<AgentAccess>("core/agent.grants")?;
    let callback = "https://example.test/asset-proof/callback";
    let registration=access.register_client(&serde_json::from_value::<ClientRegistration>(json!({"client_name":"Fixed Assets lifecycle proof","redirect_uris":[callback],"token_endpoint_auth_method":"none"}))?).await?;
    let client_id = registration["client_id"].as_str().ok_or(Error::Invalid)?;
    let resource = format!("{}/mcp", std::env::var("APP_ORIGIN")?.trim_end_matches('/'));
    let mut saved = serde_json::Map::new();
    for (name, tenant, user) in [
        ("manager", 1, 1),
        ("approver", 1, 4),
        ("viewer", 1, 2),
        ("other", 2, 5),
    ] {
        let scope = sign_in(db, services, tenant, user).await?;
        let verifier = "fixed-assets-proof-verifier-with-at-least-forty-three-characters";
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
        saved.insert(name.into(), json!(token.access_token));
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(std::env::var("BILIBI_AGENT_CREDENTIALS")?)?;
    file.write_all(serde_json::to_string(&saved)?.as_bytes())?;
    println!("Fixed Assets lifecycle OAuth credentials saved privately.");
    Ok(())
}
