//! Bilimbi `Finance.Reconciliation.Web`: bank accounts and statement import,
//! matching, and one reconciliation.
mod account;
mod format;
mod index;
mod matching;
mod opening;
mod opening_match;
mod reconciliation;

use crate::{Reconciliation, MODULE};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy};
use bilibi_finance_attachments::Attachments;
use bilibi_finance_ledger::{Ledger, PostingSubscription};
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{Access, Contribution, Method, Route, ROUTES},
    Provenance,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{page::*, Html};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::{sync::broadcast, task::JoinHandle};

pub(crate) const PAGES: &str = "finance/reconciliation.pages";
const DENIED: &str = "You do not have permission for that action.";

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub reconciliation: Arc<Reconciliation>,
    pub ledger: Arc<Ledger>,
    pub attachments: Arc<Attachments>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub contracts: Arc<bilibi_agent_api::page::PageContracts>,
    pub plans: Arc<bilibi_agent_api::plans::Plans>,
}

// Menu group (a bookkeeper's section of the Finance menu); its pages sit under it.
bilibi_menu::contribute!(
    "finance/reconciliation",
    bilibi_menu::MenuItem::container("finance.group.bank", "Bank")
        .under("finance")
        .icon("gmdi-money-r")
        .order(30)
);

bilibi_menu::contribute!(
    "finance/reconciliation",
    bilibi_menu::MenuItem::page(
        "finance.reconciliation",
        "Bank accounts",
        "/finance/reconciliation"
    )
    .company_scoped()
    .icon("gmdi-money-r")
    .under("finance.group.bank")
    .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
    .order(1)
);

macro_rules! route {
    ($id:literal, $path:literal, $op:literal) => {
        inventory::submit! { Contribution::new(ROUTES, $id,
        Provenance { module: MODULE, source: file!(), line: line!() },
        &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company, method: Method::Get, path: $path, service: PAGES, operation: $op,
            access: Access::Capability(crate::VIEW), framed: true }) }
    };
}
route!(
    "finance/reconciliation.index",
    "/finance/reconciliation",
    "index"
);
route!(
    "finance/reconciliation.account",
    "/finance/reconciliation/accounts/{id}",
    "account"
);
route!(
    "finance/reconciliation.matching",
    "/finance/reconciliation/accounts/{id}/matching",
    "matching"
);
route!(
    "finance/reconciliation.reconciliation",
    "/finance/reconciliation/reconciliations/{id}",
    "reconciliation"
);

fn company(scope: &Scope) -> Result<i64> {
    scope
        .current_company()
        .map(|id| id.get())
        .ok_or(Error::NotFound)
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
impl Pages {
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.db.get().await?;
        match bilibi_company::authorize_work_target(&**client, scope, company(scope)?, capability)
            .await
        {
            Ok(_) => Ok(true),
            Err(Error::Forbidden | Error::Unauthorized | Error::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display.display(&**client, scope, None, locale).await
    }
    fn state(&self, request: &PageRequest<'_, Scope>) -> Result<Box<dyn LiveState<Scope, Error>>> {
        match request.operation {
            "index" => Ok(Box::new(index::Index::new(
                self,
                request.query,
                request.framed,
            )?)),
            "account" => Ok(Box::new(account::Account::new(
                self,
                path_id(request)?,
                request.query,
                request.framed,
            )?)),
            "matching" => Ok(Box::new(matching::Matching::new(self, path_id(request)?)?)),
            "reconciliation" => Ok(Box::new(reconciliation::Detail::new(
                self,
                path_id(request)?,
            )?)),
            _ => Err(Error::NotFound),
        }
    }
    /// The page's title naming the record it shows, as Bilimbi's
    /// `page_title`, or `None` when the record is out of reach.
    async fn record_title(
        &self,
        scope: &Scope,
        operation: &str,
        id: i64,
    ) -> Result<Option<String>> {
        let company = company(scope)?;
        let account = |id| self.reconciliation.get_bank_account(scope, company, id);
        Ok(match operation {
            "account" => format::told(account(id).await)?.ok().map(|a| a.name),
            "matching" => format::told(account(id).await)?
                .ok()
                .map(|a| format!("{}: matching", a.name)),
            "reconciliation" => {
                match format::told(
                    self.reconciliation
                        .get_reconciliation(scope, company, id)
                        .await,
                )? {
                    Ok(r) => format::told(account(r.bank_account_id).await)?
                        .ok()
                        .map(|a| format!("{}: reconciliation {}", a.name, r.reconciliation_number)),
                    Err(_) => None,
                }
            }
            _ => None,
        })
    }
}
fn path_id(request: &PageRequest<'_, Scope>) -> Result<i64> {
    request
        .path
        .get("id")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or(Error::NotFound)
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "index" => Some("Bank accounts"),
            "account" => Some("Bank statements"),
            "matching" => Some("Matching"),
            "reconciliation" => Some("Reconciliation"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(&request)?;
            let body = state
                .update(LiveRequest {
                    scope,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?;
            let title = if request.operation == "index" {
                None
            } else {
                self.record_title(scope, request.operation, path_id(&request)?)
                    .await?
            };
            Ok(PageBody {
                html: body.html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                // Bilimbi subscribes to the ledger's postings once the page
                // connects, so these two follow the books.
                "matching" => Box::new(
                    matching::Matching::new(self, path_id(&request)?)?
                        .follow(Follow::subscribe(self, scope).await),
                ),
                "reconciliation" => Box::new(
                    reconciliation::Detail::new(self, path_id(&request)?)?
                        .follow(Follow::subscribe(self, scope).await),
                ),
                _ => self.state(&request)?,
            };
            Ok(Some(state))
        })
    }
}

/// The ledger's posting notices for a connected page (Bilimbi
/// `Ledger.subscribe_postings` in `mount`, and `handle_info` draining the
/// mailbox): every notice since the last read wakes the page once, and the
/// page reads again through the facade, since a notice carries no figures. A
/// scope without the ledger's view gets no notices and reads on demand.
#[derive(Default)]
pub(super) struct Follow {
    posting: Option<PostingSubscription>,
    bridge: Option<JoinHandle<()>>,
    posted: Arc<AtomicBool>,
    tenant: i64,
}
impl Follow {
    async fn subscribe(pages: &Pages, scope: &Scope) -> Self {
        let posting = match company(scope) {
            Ok(company) => pages.ledger.subscribe_postings(scope, company).await.ok(),
            Err(_) => None,
        };
        Self {
            posting,
            bridge: None,
            posted: Arc::default(),
            tenant: scope.tenant(),
        }
    }
    /// The page's subscription; the receiver exists before forwarding
    /// begins, and the mount's first refresh reads after LISTEN, so no
    /// posting falls in between.
    fn subscriptions(&mut self) -> Vec<Subscription> {
        let Some(mut posting) = self.posting.take() else {
            return vec![];
        };
        let (sender, receiver) = broadcast::channel(1);
        let posted = self.posted.clone();
        let tenant = self.tenant;
        self.bridge = Some(tokio::spawn(async move {
            while posting.next().await.is_some() {
                if !posted.swap(true, Ordering::SeqCst) {
                    let _ = sender.send(Change { tenant, user: None });
                }
            }
        }));
        vec![Subscription { receiver }]
    }
    /// Whether a posting arrived since the last call.
    fn posted(&self) -> bool {
        self.posted.swap(false, Ordering::SeqCst)
    }
}
impl Drop for Follow {
    fn drop(&mut self) {
        if let Some(task) = self.bridge.take() {
            task.abort();
        }
    }
}
fn event_type(value: &serde_json::Value) -> Result<&str> {
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(Error::Invalid)
}
fn record_id(value: &serde_json::Value) -> Option<i64> {
    value
        .get("id")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| v.as_i64())
        })
        .filter(|id| *id > 0)
}
/// Clears the page's message when the browser dismisses it or it times out
/// (Bilimbi `lv:clear-flash`); a posting notice's refresh keeps it.
fn clear_flash(message: &mut Option<(bilibi_ui::StatusKind, String)>, v: &serde_json::Value) {
    if message
        .as_ref()
        .is_some_and(|(kind, _)| v["key"].as_str() == Some(kind.name()))
    {
        *message = None;
    }
}
fn flash_map(flash: &Option<(bilibi_ui::StatusKind, String)>) -> bilibi_ui::FlashMap<'_> {
    let mut map = bilibi_ui::NO_FLASH;
    if let Some((kind, text)) = flash {
        match kind {
            bilibi_ui::StatusKind::Success => map.success = text,
            bilibi_ui::StatusKind::Error => map.error = text,
            bilibi_ui::StatusKind::Info => map.info = text,
            bilibi_ui::StatusKind::Warning => map.warning = text,
        }
    }
    map
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, PAGES, bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: PAGES,
    contracts: &[("preview_opening", "finance.reconciliation.opening.import"), ("import_opening", "finance.reconciliation.opening.import"), ("preview_opening_match", "finance.reconciliation.opening.match"), ("commit_opening_match", "finance.reconciliation.opening.match"), ("unmatch", "finance.reconciliation.opening.unmatch")],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Bank accounts, statement imports, matching, finalising and abandoning write through the Reconciliation facade; no contract yet." },
}) }
