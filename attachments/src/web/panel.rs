//! The source documents of one record, for the record's page (Bilimbi
//! `Finance.Attachments.Web.Panel`): the standing attachments with a download
//! link each, an upload form for the next and, while the record is not final,
//! a replace and a remove action per attachment. Every action is a facade call
//! that re-asks its capability; the `can_*` flags only decide which controls
//! show.
//!
//! A record's page mounts it as a panel of the embed
//! [`EMBED`](super::EMBED):
//!
//! ```text
//! PanelSpec { id: "payables-bill-attachments", embed: "finance.attachments-panel",
//!   path: { "panel": "payables-bill-attachments",
//!           "source": "finance/payables/bill", "document": "<record id>" },
//!   query: {} }
//! ```
//!
//! `panel` repeats the spec's element id (a panel is not told otherwise).
//! Whether the record is final is the source's own answer on every render
//! (Bilimbi passes `final?` from the page), so a page cannot show replace or
//! remove for a record that has just become final.
use super::{download::company_of, format, time_html, DENIED};
use crate::{Attachment, Attachments, NewFile, SourceClient, CREATE, DELETE, MAX_FILE_BYTES};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::DisplayPolicy;
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageFuture,
        PageRequest,
    },
    params, EmptyState, Field, Html, UploadSpec, Uploads,
};
use std::sync::Arc;

const UPLOAD: &str = "file";

/// The services of every mounted panel.
#[derive(Clone)]
pub(crate) struct Panels {
    pub db: Db,
    pub attachments: Arc<Attachments>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

enum Action {
    None,
    Replace(i64),
    Remove(i64),
}

struct Notice {
    error: bool,
    message: String,
}

struct Item {
    id: i64,
    id_text: String,
    file_name: String,
    size: String,
    sha256: String,
    short_hash: String,
    retain_until: String,
    on_hold: bool,
    captured: Html,
    show_actions: bool,
    can_replace: bool,
    can_remove: bool,
    replace_id: String,
    remove_id: String,
    confirm_id: String,
    removal: Option<Field>,
}

#[derive(Template)]
#[template(path = "panel.html")]
struct View {
    /// Not `id`: inside a macro call body Askama resolves `id` to the macro's.
    panel: String,
    card_id: String,
    heading_id: String,
    settings_id: String,
    count: Option<usize>,
    notice: Option<Notice>,
    items: Vec<Item>,
    empty: Option<Html>,
    file_input: Option<Html>,
    replacing: bool,
}

struct Panel {
    pages: Panels,
    id: String,
    source: String,
    document: i64,
    action: Action,
    reason: String,
    notice: Option<Notice>,
    uploads: Uploads,
}

impl Page<Scope, Error> for Panels {
    fn render<'a>(&'a self, _request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async { Err(Error::NotFound) })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "panel" {
                return Err(Error::NotFound);
            }
            let text = |key: &str| {
                request
                    .path
                    .get(key)
                    .filter(|v| !v.is_empty() && v.len() <= 128)
                    .cloned()
                    .ok_or(Error::Invalid)
            };
            let id = text("panel")?;
            let source = text("source")?;
            let document: i64 = text("document")?
                .parse()
                .ok()
                .filter(|d| *d > 0)
                .ok_or(Error::Invalid)?;
            if request.path.len() != 3
                || !request.query.is_empty()
                || !self.attachments.source_mounted(&source)
            {
                return Err(Error::Invalid);
            }
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Panel {
                pages: self.clone(),
                id,
                source,
                document,
                action: Action::None,
                reason: String::new(),
                notice: None,
                uploads: Uploads::new([UploadSpec::new(UPLOAD, MAX_FILE_BYTES as u64)]),
            });
            Ok(Some(state))
        })
    }
}

impl Panel {
    fn notice(&mut self, error: bool, message: impl Into<String>) {
        self.notice = Some(Notice {
            error,
            message: message.into(),
        });
    }

    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.pages.db.get().await?;
        Ok(bilibi_authz::decide(&**client, scope, capability)
            .await?
            .allowed)
    }

    /// Bilimbi `Attachments.list_attachments`: an unreadable record is an
    /// empty list; only a failure is raised.
    async fn list(&self, scope: &Scope) -> Result<Vec<Attachment>> {
        let Some(company) = company_of(scope) else {
            return Ok(Vec::new());
        };
        match self
            .pages
            .attachments
            .list_attachments(scope, company, &self.source, self.document)
            .await
        {
            Ok(list) => Ok(list),
            Err(crate::AttachmentError::Failed(error)) => Err(error),
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Whether the source calls the record final now; one it cannot read
    /// offers no replace or remove.
    async fn is_final(&self, scope: &Scope) -> Result<bool> {
        let Some(company) = company_of(scope) else {
            return Ok(true);
        };
        let client = self.pages.db.get().await?;
        let registered = match self.pages.attachments.registered(&self.source) {
            Ok(registered) => registered,
            Err(_) => return Ok(true),
        };
        match self
            .pages
            .attachments
            .source_document(
                SourceClient::Client(&client),
                scope,
                company,
                registered,
                self.document,
            )
            .await
        {
            Ok(document) => Ok(document.is_final),
            Err(crate::AttachmentError::Failed(error)) => Err(error),
            Err(_) => Ok(true),
        }
    }

    async fn body(&self, scope: &Scope) -> Result<String> {
        let attachments = self.list(scope).await?;
        let can_create = self.allowed(scope, CREATE).await?;
        let can_delete = self.allowed(scope, DELETE).await?;
        let is_final = self.is_final(scope).await?;
        let display = {
            let client = self.pages.db.get().await?;
            let locale = self
                .pages
                .locales
                .resolve(&**client, scope, None)
                .await?
                .locale;
            self.pages
                .display
                .display(&**client, scope, None, locale)
                .await?
        };
        let id = &self.id;
        let mut items = Vec::with_capacity(attachments.len());
        for a in &attachments {
            let replacing = matches!(self.action, Action::Replace(r) if r == a.id);
            let removing = matches!(self.action, Action::Remove(r) if r == a.id);
            items.push(Item {
                id: a.id,
                id_text: a.id.to_string(),
                file_name: a.file_name.clone(),
                size: format::bytes(a.byte_size),
                sha256: a.sha256.clone(),
                short_hash: format::short_hash(&a.sha256),
                retain_until: format::retain_until(a.retain_until),
                on_hold: a.on_hold,
                captured: time_html(&display, &format!("{id}-{}-captured", a.id), a.captured_at)?,
                show_actions: !is_final && (can_create || can_delete),
                can_replace: can_create && !replacing,
                can_remove: can_delete && !removing,
                replace_id: format!("{id}-{}-replace", a.id),
                remove_id: format!("{id}-{}-remove", a.id),
                confirm_id: format!("{id}-{}-remove-confirm", a.id),
                removal: removing.then(|| {
                    Field::in_form("removal", "reason")
                        .id(format!("{id}-{}-remove-reason", a.id))
                        .label("Reason for removing (1 to 500 characters)")
                        .attr("maxlength", "500")
                        .required()
                        .value(self.reason.clone())
                }),
            });
        }
        let empty = if attachments.is_empty() && !can_create {
            Some(render_html(
                EmptyState::missing(
                    "No source documents",
                    "Nothing has been attached to this record.",
                )
                .id(format!("{id}-empty")),
            )?)
        } else {
            None
        };
        let file_input = if can_create {
            Some(
                // One button: choosing the file attaches it, with no second
                // press on a separate submit.
                Html::render(
                    &self
                        .uploads
                        .input(UPLOAD, format!("{id}-file"))
                        .immediate()
                        .label(if matches!(self.action, Action::Replace(_)) {
                            "Choose replacement"
                        } else {
                            "Attach file"
                        }),
                )
                .map_err(|_| Error::Internal)?,
            )
        } else {
            None
        };
        View {
            panel: id.clone(),
            card_id: format!("{id}-card"),
            heading_id: format!("{id}-heading"),
            settings_id: format!("{id}-storage-settings"),
            count: Some(attachments.len()),
            notice: self.notice.as_ref().map(|n| Notice {
                error: n.error,
                message: n.message.clone(),
            }),
            items,
            empty,
            file_input,
            replacing: matches!(self.action, Action::Replace(_)),
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// Bilimbi `attach/1`: the uploaded bytes are taken once, here, and
    /// handed to the facade, which keeps them as a records-grade document.
    async fn attach(&mut self, scope: &Scope) -> Result<()> {
        if !self.allowed(scope, CREATE).await? {
            self.notice(true, DENIED);
            return Ok(());
        }
        let mut files = self.uploads.consume(UPLOAD);
        let Some(file) = files.pop() else {
            self.notice(true, "Choose a file first.");
            return Ok(());
        };
        let Some(company) = company_of(scope) else {
            self.notice(
                true,
                "This company cannot be read with your current access.",
            );
            return Ok(());
        };
        let file = NewFile {
            file_name: file.name,
            content: file.bytes,
            content_type: Some(file.content_type),
        };
        let result = match self.action {
            Action::Replace(id) => {
                self.pages
                    .attachments
                    .replace(scope, company, id, &file)
                    .await
            }
            _ => {
                self.pages
                    .attachments
                    .attach(scope, company, &self.source, self.document, &file)
                    .await
            }
        };
        match result {
            Ok(attachment) => {
                self.action = Action::None;
                self.notice(
                    false,
                    format!(
                        "{} is kept at least until {}.",
                        attachment.file_name,
                        format::retain_until(attachment.retain_until)
                    ),
                );
            }
            Err(error) => self.notice(true, format::refusal(error)?),
        }
        Ok(())
    }

    async fn remove(&mut self, scope: &Scope) -> Result<()> {
        if !self.allowed(scope, DELETE).await? {
            self.notice(true, DENIED);
            return Ok(());
        }
        let Action::Remove(id) = self.action else {
            return Err(Error::Invalid);
        };
        let Some(company) = company_of(scope) else {
            self.notice(
                true,
                "This company cannot be read with your current access.",
            );
            return Ok(());
        };
        match self
            .pages
            .attachments
            .remove(scope, company, id, &self.reason)
            .await
        {
            Ok(attachment) => {
                self.action = Action::None;
                self.reason.clear();
                self.notice(
                    false,
                    format!(
                        "{} was removed; its bytes stay retained.",
                        attachment.file_name
                    ),
                );
            }
            Err(error) => self.notice(true, format::refusal(error)?),
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
        let id = || {
            get("id")
                .and_then(|id| id.parse::<i64>().ok())
                .filter(|id| *id > 0)
        };
        match get("type").ok_or(Error::Invalid)? {
            "attach" => self.attach(scope).await,
            "open_replace" => {
                self.action = Action::Replace(id().ok_or(Error::Invalid)?);
                self.notice = None;
                Ok(())
            }
            "open_remove" => {
                self.action = Action::Remove(id().ok_or(Error::Invalid)?);
                self.reason.clear();
                self.notice = None;
                Ok(())
            }
            "cancel_action" => {
                self.action = Action::None;
                self.reason.clear();
                Ok(())
            }
            "reason" => {
                self.reason = params::form_values(value, "removal")
                    .remove("reason")
                    .unwrap_or_default();
                Ok(())
            }
            "remove" => {
                self.reason = params::form_values(value, "removal")
                    .remove("reason")
                    .unwrap_or_default();
                self.remove(scope).await
            }
            _ => Err(Error::Invalid),
        }
    }
}

fn render_html(state: EmptyState) -> Result<Html> {
    Html::render(&state).map_err(|_| Error::Internal)
}

impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Event(value) => self.event(request.scope, value).await?,
                LiveInput::Params(_) => return Err(Error::Invalid),
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
    fn uploads(&mut self) -> Option<&mut Uploads> {
        Some(&mut self.uploads)
    }
}
