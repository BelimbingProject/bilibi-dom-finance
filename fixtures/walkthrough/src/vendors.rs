//! Reusable linked vendor setup for disposable Finance walkthroughs.
use bilibi_database::Db;
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;

pub async fn vendor(
    db: &Db,
    services: &Services,
    scope: &Scope,
    company: i64,
    code: &str,
    name: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let p = services.get::<bilibi_finance_payables::Payables>(bilibi_finance_payables::SERVICE)?;
    if let Some(v) = p
        .list_vendors(scope, company, 0, 500)
        .await?
        .into_iter()
        .find(|v| v.code == code)
    {
        return Ok(v.id);
    }
    let core = services.get::<bilibi_company::CompanyService>(bilibi_company::SERVICE)?;
    let name = format!("{name} ({code})");
    let existing = {
        let mut client = db.get().await?;
        bilibi_company::list(
            &mut client,
            scope,
            &bilibi_company::Params {
                q: name.clone(),
                ..bilibi_company::Params::default()
            },
        )
        .await?
        .rows
        .into_iter()
        .find(|c| c.name == name)
    };
    let core_company_id = if let Some(existing) = existing {
        existing.id
    } else {
        match core
            .create_company(
                scope,
                &bilibi_company::NewCompany {
                    name,
                    code: Some(code.into()),
                    status: Some("active".into()),
                    ..bilibi_company::NewCompany::default()
                },
            )
            .await?
        {
            bilibi_company::CreateOutcome::Created(c) => c.id,
            bilibi_company::CreateOutcome::Refused(e) => {
                return Err(format!("fixture company refused: {e:?}").into());
            }
        }
    };
    Ok(p.create_vendor(
        scope,
        company,
        &bilibi_finance_payables::VendorInput {
            record_kind: Some("company".into()),
            record_id: Some(core_company_id),
            new_company: None,
            new_person: None,
            payment_terms_days: 0,
            payable_account_id: None,
            expense_account_id: None,
            tax_code_id: None,
            active: true,
        },
    )
    .await?
    .id)
}
