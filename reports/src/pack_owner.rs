//! Bilimbi Reports.ArtifactOwner. A stored pack is as sensitive as every source.
use crate::{EXPORT, ISSUE, MODULE, VIEW};
use bilibi_artifacts::{Operation, Owner, Reference};
use bilibi_company::Directory;
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{OrganisationDirectory, ProofClient, ProofFuture, Scope};

pub(crate) struct ArtifactOwner;

pub(crate) const SOURCE_VIEWS: &[&str] = &[
    VIEW,
    bilibi_finance_attachments::VIEW,
    bilibi_finance_receivables::VIEW,
    bilibi_finance_payables::VIEW,
    bilibi_finance_reconciliation::VIEW,
    bilibi_finance_einvoice::VIEW,
    bilibi_finance_payroll::VIEW,
    bilibi_finance_sst::VIEW,
    bilibi_finance_fixed_assets::VIEW,
];

pub(crate) const CONVERSION_SOURCE_VIEWS: &[&str] = &[
    VIEW,
    bilibi_finance_receivables::VIEW,
    bilibi_finance_payables::VIEW,
    bilibi_finance_reconciliation::VIEW,
    bilibi_finance_payroll::VIEW,
    bilibi_finance_sst::VIEW,
    bilibi_finance_fixed_assets::VIEW,
];

pub(crate) async fn authorize_sources(
    client: &dyn ProofClient,
    scope: &Scope,
    company: OrganisationId,
) -> Result<(), Error> {
    for capability in SOURCE_VIEWS {
        bilibi_company::authorize_work_target(client, scope, company.get(), capability).await?;
    }
    Ok(())
}

pub(crate) async fn authorize_conversion_sources(
    client: &dyn ProofClient,
    scope: &Scope,
    company: OrganisationId,
) -> Result<(), Error> {
    for capability in CONVERSION_SOURCE_VIEWS {
        Directory.target(client, scope, company, capability).await?;
    }
    Ok(())
}

/// Coverage is persisted in the artifact reference, so losing a source grant
/// can never turn an existing full pack into a downloadable ledger-only pack.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    Ledger,
    All,
}
impl Coverage {
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Self::Ledger => "ledger-pack",
            Self::All => "auditor-pack",
        }
    }

    pub(crate) async fn select(
        client: &dyn ProofClient,
        scope: &Scope,
        company: OrganisationId,
    ) -> Result<Self, Error> {
        bilibi_company::authorize_work_target(client, scope, company.get(), VIEW).await?;
        match authorize_sources(client, scope, company).await {
            Ok(()) => Ok(Self::All),
            Err(Error::Forbidden) => Ok(Self::Ledger),
            Err(error) => Err(error),
        }
    }

    async fn authorize(
        self,
        client: &dyn ProofClient,
        scope: &Scope,
        company: OrganisationId,
    ) -> Result<(), Error> {
        match self {
            Self::Ledger => {
                bilibi_company::authorize_work_target(client, scope, company.get(), VIEW)
                    .await
                    .map(|_| ())
            }
            Self::All => authorize_sources(client, scope, company).await,
        }
    }
}

impl Owner for ArtifactOwner {
    fn owner_id(&self) -> &'static str {
        MODULE
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            if operation != Operation::Purge
                && !reference.is_some_and(|r| {
                    matches!(r.kind.as_str(), "auditor-pack" | "ledger-pack")
                        && uuid::Uuid::parse_str(&r.subject).is_ok()
                })
            {
                return Err(Error::Forbidden);
            }
            let exporting = matches!(operation, Operation::Create | Operation::Read);
            bilibi_company::authorize_work_target(
                client,
                scope,
                company.get(),
                if exporting { EXPORT } else { ISSUE },
            )
            .await?;
            let target = Directory
                .lookup(client, scope, company)
                .await?
                .ok_or(Error::NotFound)?;
            // An archived company's books stay available for audit, so its
            // packs still export and download; nothing else changes them.
            if target.archived && !exporting {
                return Err(Error::Forbidden);
            }
            if exporting {
                let coverage = match reference.map(|r| r.kind.as_str()) {
                    Some("ledger-pack") => Coverage::Ledger,
                    Some("auditor-pack") => Coverage::All,
                    _ => return Err(Error::Forbidden),
                };
                coverage.authorize(client, scope, company).await?;
            }
            Ok(())
        })
    }
}
