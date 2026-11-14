//! Fiscal documents (the tax document an authority clears, such as an
//! e-invoice) and the posted records they are made for. The ledger keeps
//! neither; it carries what two siblings need of each other so neither
//! depends on the other:
//!
//! - a module that posts sales documents (`finance/receivables`) contributes
//!   a [`FiscalSources`] reader: what one posted record says (its party, its
//!   lines, the entry it posted), for the module that issues fiscal documents;
//! - a module that issues fiscal documents (`finance/einvoice`) contributes a
//!   [`FiscalDocuments`] reader: where each record's fiscal document stands
//!   and the address that starts one, for the record's own list and page.
//!
//! Each reader works on the connection it is handed and authorizes the person
//! itself with its own capability: a person who may not view the other side
//! gets nothing, never a refusal. No posting rule reads either.
use crate::MODULE;
use bilibi_agent_api::OperationClient;
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError};
use bilibi_tenancy::Scope;
use bilibi_ui::{params, BadgeKind, Html};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, future::Future, pin::Pin, sync::OnceLock};

/// The catalog key readers of posted records are contributed to.
pub const FISCAL_SOURCES: &str = "finance/ledger.fiscal_sources";
/// The catalog key readers of fiscal documents are contributed to.
pub const FISCAL_DOCUMENTS: &str = "finance/ledger.fiscal_documents";

pub type FiscalFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, bilibi_database::Error>> + Send + 'a>>;

/// What a posted record is to its fiscal document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiscalSourceKind {
    Invoice,
    /// Corrects an earlier invoice ([`FiscalSource::corrects`]).
    CreditNote,
}

/// One posted record, as the module that issues its fiscal document reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiscalSource {
    /// The record in its owner's terms (`document:17`), as the ledger entry's
    /// owner names it.
    pub record: String,
    pub kind: FiscalSourceKind,
    /// The number the record was issued under.
    pub number: String,
    /// `YYYY-MM-DD`.
    pub issue_date: String,
    pub currency_code: String,
    pub total: Decimal,
    /// The entry the record posted.
    pub ledger_entry_id: i64,
    /// For a credit note, the record of the invoice it corrects.
    pub corrects: Option<String>,
    pub party: FiscalParty,
    pub lines: Vec<FiscalLine>,
}

/// The other party as the record froze it when it was issued.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FiscalParty {
    pub name: Option<String>,
    pub tax_id: Option<String>,
    pub registration_number: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address_lines: Vec<String>,
    pub city: Option<String>,
    pub postcode: Option<String>,
    /// The state or province code as the owner stores it.
    pub state: Option<String>,
    /// The country code as the owner stores it.
    pub country: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiscalLine {
    pub description: String,
    pub quantity: Decimal,
    pub unit_price: Decimal,
    /// A percentage; `None` for a line with no tax.
    pub tax_rate: Option<Decimal>,
}

/// A posted record offered as a choice: enough to name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiscalChoice {
    pub record: String,
    pub number: String,
    pub party_name: String,
    /// `YYYY-MM-DD`.
    pub issue_date: String,
}

/// Reads a module's posted records for the module that issues their fiscal
/// documents. The reader authorizes the person with its own view capability.
pub trait FiscalSources: Send + Sync {
    /// One posted record, or `None` when the company has none by that name,
    /// it is not posted, or the person may not view it.
    fn read<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        company_id: i64,
        record: &'a str,
    ) -> FiscalFuture<'a, Option<FiscalSource>>;

    /// The company's newest posted records, at most `limit`, newest first;
    /// none when the person may not view them.
    fn recent<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        company_id: i64,
        limit: i64,
    ) -> FiscalFuture<'a, Vec<FiscalChoice>>;
}

/// The contribution payload: `Contribution::new(FISCAL_SOURCES, "<module id>/fiscal_sources", provenance, &FiscalSourcesDefinition { sources: &Posted })`.
pub struct FiscalSourcesDefinition {
    pub sources: &'static dyn FiscalSources,
}

/// How a fiscal document stands, for the colour of its badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiscalTone {
    /// The authority accepted it.
    Good,
    /// On its way: recorded or sent, not answered yet.
    Waiting,
    /// The authority or the buyer turned it back; a person must act.
    Problem,
    /// It no longer stands (cancelled).
    Ended,
}

/// Where the fiscal document of one record stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiscalStanding {
    /// The record it is the fiscal document of (`document:17`).
    pub record: String,
    /// The issuing module's own status word (`valid`, `submitted`).
    pub status: String,
    /// The status as a person reads it ("Valid").
    pub label: String,
    pub tone: FiscalTone,
    /// The fiscal document's number.
    pub number: String,
    /// The fiscal document's page.
    pub href: String,
    /// Whether a new fiscal document may be started for the record: this one
    /// does not stand.
    pub replaceable: bool,
}

#[derive(askama::Template)]
#[template(
    source = r#"<a href="{{ href }}" title="E-invoice {{ number }}" class="hover:underline">{{ badge|safe }}</a>"#,
    ext = "html"
)]
struct StandingLink<'a> {
    href: &'a str,
    number: &'a str,
    badge: Html,
}

impl FiscalStanding {
    /// The status as a badge that opens the fiscal document, the same on the
    /// record's list and page.
    pub fn badge(&self) -> Html {
        let kind = match self.tone {
            FiscalTone::Good => BadgeKind::Success,
            FiscalTone::Waiting => BadgeKind::Warning,
            FiscalTone::Problem => BadgeKind::Danger,
            FiscalTone::Ended => BadgeKind::Neutral,
        };
        let badge = bilibi_ui::status_badge(kind, &self.label);
        // Only escaped text is written, so rendering cannot fail.
        Html::render(&StandingLink {
            href: &self.href,
            number: &self.number,
            badge: badge.clone(),
        })
        .unwrap_or(badge)
    }
}

/// The fiscal documents of the records asked about, and the page that starts
/// one when this person may.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FiscalStandings {
    /// Whether a mounted reader let this person read fiscal documents at all;
    /// when not, a page leaves the subject out instead of showing blanks.
    pub readable: bool,
    pub documents: Vec<FiscalStanding>,
    /// The path of the page that starts a fiscal document
    /// ([`FiscalStart::href`]); `None` when the person may not start one.
    pub start: Option<String>,
}

/// Reads the fiscal documents of a module's records, for that module's list
/// and page. The reader authorizes the person with its own view capability.
pub trait FiscalDocuments: Send + Sync {
    fn standing<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        company_id: i64,
        module: &'a str,
        records: &'a [String],
    ) -> FiscalFuture<'a, FiscalStandings>;
}

/// The contribution payload: `Contribution::new(FISCAL_DOCUMENTS, "<module id>/fiscal_documents", provenance, &FiscalDocumentsDefinition { documents: &Issued })`.
pub struct FiscalDocumentsDefinition {
    pub documents: &'static dyn FiscalDocuments,
}

const SOURCE: &str = "source";
const RECORD: &str = "record";

/// The address that starts a fiscal document for a record: built by the
/// record's page, read by the page that starts it. An address is a request,
/// never proof: the starting page reads the record again through
/// [`FiscalSources`] under the person's own scope.
pub struct FiscalStart;

impl FiscalStart {
    /// `path` with the record to start from (`module` and its `record`).
    pub fn href(path: &str, module: &str, record: &str) -> String {
        params::address(
            path,
            &HashMap::from([
                (SOURCE.to_owned(), module.to_owned()),
                (RECORD.to_owned(), record.to_owned()),
            ]),
        )
    }

    /// The module and record an address asks to start from, if it names one.
    pub fn read(query: &HashMap<String, String>) -> Option<(String, String)> {
        let text = |key: &str| {
            query
                .get(key)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty() && value.len() <= 100)
                .map(str::to_owned)
        };
        Some((text(SOURCE)?, text(RECORD)?))
    }
}

fn owned(entry: &Entry, what: &str) -> bilibi_module_registry::Result<()> {
    if entry.id.starts_with(entry.provenance.module) {
        return Ok(());
    }
    Err(RegistryError::Contribution(format!(
        "invalid {what} reader {} at {}:{}",
        entry.id, entry.provenance.source, entry.provenance.line
    )))
}

fn validate_sources(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        entry.get::<FiscalSourcesDefinition>()?;
        owned(entry, "fiscal sources")?;
    }
    Ok(())
}
fn validate_documents(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        entry.get::<FiscalDocumentsDefinition>()?;
        owned(entry, "fiscal documents")?;
    }
    Ok(())
}
inventory::submit! { Consumer::new::<FiscalSourcesDefinition>(FISCAL_SOURCES, MODULE, validate_sources) }
inventory::submit! { Consumer::new::<FiscalDocumentsDefinition>(FISCAL_DOCUMENTS, MODULE, validate_documents) }

/// Each sources reader under the module that contributed it.
static SOURCES: OnceLock<Vec<(&'static str, &'static dyn FiscalSources)>> = OnceLock::new();
static DOCUMENTS: OnceLock<Vec<&'static dyn FiscalDocuments>> = OnceLock::new();

pub(crate) fn bind() -> bilibi_module_registry::Result<()> {
    let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
    let mut sources = Vec::new();
    for entry in catalog.entries::<FiscalSourcesDefinition>(FISCAL_SOURCES)? {
        sources.push((
            entry.provenance.module,
            entry.get::<FiscalSourcesDefinition>()?.sources,
        ));
    }
    let mut documents = Vec::new();
    for entry in catalog.entries::<FiscalDocumentsDefinition>(FISCAL_DOCUMENTS)? {
        documents.push(entry.get::<FiscalDocumentsDefinition>()?.documents);
    }
    let duplicate = || RegistryError::Service("duplicate ledger fiscal binding".into());
    SOURCES.set(sources).map_err(|_| duplicate())?;
    DOCUMENTS.set(documents).map_err(|_| duplicate())
}

/// One posted record of `module`, as its own reader describes it.
pub(crate) async fn source(
    client: &dyn OperationClient,
    scope: &Scope,
    company_id: i64,
    module: &str,
    record: &str,
) -> Result<Option<FiscalSource>, bilibi_database::Error> {
    for (owner, reader) in SOURCES.get().into_iter().flatten() {
        if *owner == module {
            return reader.read(client, scope, company_id, record).await;
        }
    }
    Ok(None)
}

/// Every mounted reader's newest posted records, each under its module.
pub(crate) async fn recent(
    client: &dyn OperationClient,
    scope: &Scope,
    company_id: i64,
    limit: i64,
) -> Result<Vec<(&'static str, FiscalChoice)>, bilibi_database::Error> {
    let mut choices = Vec::new();
    for (owner, reader) in SOURCES.get().into_iter().flatten() {
        for choice in reader.recent(client, scope, company_id, limit).await? {
            choices.push((*owner, choice));
        }
    }
    Ok(choices)
}

/// Where the fiscal documents of `module`'s records stand; nothing when no
/// module issues them.
pub(crate) async fn standing(
    client: &dyn OperationClient,
    scope: &Scope,
    company_id: i64,
    module: &str,
    records: &[String],
) -> Result<FiscalStandings, bilibi_database::Error> {
    let mut all = FiscalStandings::default();
    for reader in DOCUMENTS.get().into_iter().flatten() {
        let found = reader
            .standing(client, scope, company_id, module, records)
            .await?;
        all.readable |= found.readable;
        all.documents.extend(found.documents);
        all.start = all.start.or(found.start);
    }
    Ok(all)
}
