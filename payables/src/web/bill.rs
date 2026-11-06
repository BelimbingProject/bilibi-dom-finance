//! Bilimbi Payables.Web.BillLive. The reviewed hold belongs to the dialog,
//! not to a subsequent refreshed document or a value supplied by the browser.
//! The page is drawn in `bill/view.rs`; an address with `?do=void` or
//! `?do=approve_exception` (the "Actions…" menu) opens that dialog.
mod view;

use super::{company, format as f, render, Pages, Written};
use crate::{
    Document, MatchApproval, PayablesError, APPROVE_DOCUMENTS, APPROVE_MATCH, POST_DOCUMENTS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MinorUnits, RecordActions};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PanelSpec},
    params, BuiltinColumn, ButtonVariant, Field, FlashMap, FlexRow, FlexTable, Html, ListColumns,
    StatusKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;

enum Dialog {
    Void,
    ApproveException { held_event_id: i64 },
}

pub(super) struct Bill {
    pages: Pages,
    id: i64,
    balance_input: Value,
    balance_pins: Value,
    document: Option<Document>,
    units: Option<MinorUnits>,
    accounts: Vec<(i64, String, String)>,
    dialog: Option<Dialog>,
    credit_actions: super::credits::Credits,
    reason: String,
    flash: Option<(StatusKind, String)>,
    /// The dialog an address asked for (`?do=`), opened once the bill loads.
    requested: Option<String>,
    lines: ListColumns,
    matches: ListColumns,
    events: ListColumns,
}

impl Bill {
    pub(super) fn new(
        pages: Pages,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            pages,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            balance_input: super::credits::balance_query(query)?,
            balance_pins: Value::Null,
            document: None,
            units: None,
            accounts: vec![],
            dialog: None,
            credit_actions: super::credits::Credits::new()?,
            reason: String::new(),
            flash: None,
            requested: RecordActions::requested(query),
            lines: columns(
                "payables-bill-lines-table",
                &[
                    ("number", "#"),
                    ("description", "Description"),
                    ("account", "Account"),
                    ("order_line", "Order line"),
                    ("quantity", "Quantity"),
                    ("amount", "Amount"),
                ],
                &["quantity", "amount"],
            )?,
            matches: columns(
                "payables-bill-match-table",
                &[
                    ("number", "#"),
                    ("order", "Order"),
                    ("billed", "Billed"),
                    ("ordered", "Ordered"),
                    ("received", "Received"),
                    ("billed_before", "Billed before"),
                    ("tolerances", "Tolerances"),
                    ("result", "Result"),
                ],
                &["billed", "ordered", "received", "billed_before"],
            )?,
            events: columns(
                "payables-bill-match-events-table",
                &[
                    ("when", "When"),
                    ("event", "Event"),
                    ("exceptions", "Exceptions"),
                    ("reason", "Reason"),
                    ("by", "By"),
                ],
                &[],
            )?,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        let historical = !self.balance_input["as_of"].is_null();
        self.document = None;
        (self.units, self.accounts) = self.pages.account_context(scope).await?;
        if historical {
            let mut input = self.balance_input.clone();
            input["company_id"] = json!(company(scope).ok_or(Error::Unauthorized)?);
            input["document_id"] = json!(self.id);
            let answer = self
                .pages
                .contracts
                .read(scope, "finance.payables.document.get", &input)
                .await
                .map_err(|_| Error::Invalid)?;
            let mut result = answer["result"].clone();
            self.balance_pins = result["balance_pins"].clone();
            self.balance_input = self.balance_pins.clone();
            result["match_events"] = result["match_context"].take();
            self.document = Some(serde_json::from_value(result).map_err(|_| Error::Internal)?);
            self.credit_actions = super::credits::Credits::new()?;
            self.dialog = None;
        } else {
            self.balance_pins = Value::Null;
            self.document = match company(scope) {
                Some(c) => self
                    .pages
                    .payables
                    .get_document(scope, c, self.id)
                    .await
                    .ok(),
                None => None,
            };
        }

        if let Some(d) = &self.document {
            if !historical {
                self.credit_actions
                    .load(&self.pages, scope, d.id, &d.document_type)
                    .await?;
            }
            self.lines.load(&d.lines, |l| l.id.to_string());
            self.events.load(&d.match_events, |e| e.id.to_string());
            if let Some(e) = f::last_match(d) {
                self.matches.load(&e.lines, |l| l.line_number.to_string());
            }
        }
        Ok(())
    }

    /// Opens the dialog an address asked for, as its menu item's link does.
    pub(super) async fn open_requested(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        if let Some(action) = self.requested.take() {
            if matches!(action.as_str(), "void" | "approve_exception") && self.document.is_some() {
                self.event(
                    scope,
                    credential,
                    &json!({"type": "open_dialog", "dialog": action}),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// `Bill #3`, or `Bill from Acme` while it has no number to name it by.
    fn heading(&self, d: &Document) -> String {
        match d.document_number {
            Some(n) => format!("{} #{n}", f::kind(&d.document_type)),
            None => format!("{} from {}", f::kind(&d.document_type), d.party.name),
        }
    }

    pub(super) fn title(&self) -> Option<String> {
        self.document.as_ref().map(|d| match d.document_number {
            Some(_) => format!("{} from {}", self.heading(d), d.party.name),
            None => self.heading(d),
        })
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<bool> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        if self.balance_pins.is_object()
            && !matches!(
                event,
                "clear-flash" | "lines_grid" | "matches_grid" | "events_grid" | "credit_grid"
            )
        {
            return Err(Error::Invalid);
        }
        match event {
            "ask_credit_apply" | "ask_credit_refund" | "ask_credit_reverse" | "cancel_credit"
            | "change_credit" | "preview_credit" | "commit_credit" | "credit_grid" => {
                self.credit_actions
                    .event(&self.pages, scope, credential, v, self.id)
                    .await?;
                return Ok(true);
            }

            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| v["key"].as_str() == Some(kind.name()))
                {
                    self.flash = None;
                }
                return Ok(false);
            }
            "lines_grid" | "matches_grid" | "events_grid" => {
                let columns = match event {
                    "lines_grid" => &mut self.lines,
                    "matches_grid" => &mut self.matches,
                    _ => &mut self.events,
                };
                columns.handle(&params::string_values(v));
                return Ok(false);
            }
            "close_dialog" => {
                self.dialog = None;
                self.reason.clear();
                return Ok(false);
            }
            "reason" => {
                self.reason = reason(v).unwrap_or_default().to_owned();
                return Ok(false);
            }
            _ => (),
        }
        let action = if event == "open_dialog" {
            v["dialog"].as_str().unwrap_or("")
        } else {
            event
        };
        let capability = match action {
            "approve" | "void" => APPROVE_DOCUMENTS,
            "post" => POST_DOCUMENTS,
            "approve_exception" => APPROVE_MATCH,
            _ => return Err(Error::Invalid),
        };
        if !self.pages.allowed(scope, capability).await? {
            self.flash = Some((
                StatusKind::Error,
                f::refusal(action, PayablesError::Unauthorized),
            ));
            self.dialog = None;
            self.reason.clear();
            return Ok(true);
        }
        if event == "open_dialog" {
            self.flash = None;
            self.dialog = match action {
                "void" => Some(Dialog::Void),
                "approve_exception" => self
                    .document
                    .as_ref()
                    .filter(|d| d.match_status.as_deref() == Some("held"))
                    .and_then(f::last_match)
                    .filter(|e| e.event == "held")
                    .map(|e| Dialog::ApproveException {
                        held_event_id: e.id,
                    }),
                _ => None,
            };
            self.reason.clear();
            return Ok(false);
        }
        let Some(company) = company(scope) else {
            self.flash = Some((
                StatusKind::Error,
                f::refusal(action, PayablesError::Unauthorized),
            ));
            self.dialog = None;
            return Ok(true);
        };
        if action == "approve_exception" {
            let result = match self.dialog {
                Some(Dialog::ApproveException { held_event_id }) => {
                    self.pages
                        .payables
                        .approve_match_exception(
                            scope,
                            company,
                            self.id,
                            &MatchApproval {
                                held_event_id,
                                reason: reason(v).unwrap_or("").into(),
                            },
                        )
                        .await
                }
                _ => Err(PayablesError::NotHeld),
            };
            self.dialog = None;
            self.reason.clear();
            self.flash = Some(match result {
                Err(e) => (StatusKind::Error, f::refusal(action, e)),
                Ok(_) => (
                    StatusKind::Success,
                    "Match exceptions approved; the bill may now be posted.".into(),
                ),
            });
            return Ok(true);
        }
        // The rest are the contracts the agent doors call.
        let (contract, input, verb) = match action {
            "approve" => (
                "finance.payables.document.approve",
                json!({"company_id": company, "document_id": self.id}),
                "The approval",
            ),
            "post" => (
                "finance.payables.document.post",
                json!({"company_id": company, "document_id": self.id}),
                "The posting",
            ),
            "void" if matches!(self.dialog, Some(Dialog::Void)) => (
                "finance.payables.document.void",
                json!({"company_id": company, "document_id": self.id, "void_reason": reason(v).unwrap_or("")}),
                "The void",
            ),
            _ => return Err(Error::Invalid),
        };
        let written = self
            .pages
            .write(scope, credential, action, contract, input)
            .await?;
        self.dialog = None;
        self.reason.clear();
        let mut reload = true;
        self.flash = Some(match written {
            Written::Done(_) => {
                // The page shows what the contract did, so read it back
                // before wording the outcome (a post may be a hold).
                self.reload(scope).await?;
                reload = false;
                let d = self.document.as_ref();
                match action {
                    "approve" => (StatusKind::Success, "Document approved.".into()),
                    "post" if d.is_some_and(|d| d.status == "held") => (
                        StatusKind::Warning,
                        format!(
                            "Not posted: the three-way match found {} exception(s), so the bill is held.",
                            d.and_then(f::last_match).map_or(0, f::exceptions)
                        ),
                    ),
                    "post" => (
                        StatusKind::Success,
                        format!(
                            "Posted as number {}.",
                            d.and_then(|d| d.document_number).unwrap_or_default()
                        ),
                    ),
                    _ => (StatusKind::Success, "Document voided.".into()),
                }
            }
            Written::Forbidden => (
                StatusKind::Error,
                f::refusal(action, PayablesError::Unauthorized),
            ),
            Written::Changed => (
                StatusKind::Error,
                format!(
                    "{verb} did not go through because the document changed. Review it and try again."
                ),
            ),
            Written::Fields(fields) if action == "void" && fields.contains_key("void_reason") => (
                StatusKind::Error,
                "Give a reason of 1 to 500 characters.".into(),
            ),
            Written::Fields(fields) => (StatusKind::Error, f::refused(verb, &fields)),
        });
        Ok(reload)
    }
}

impl LiveState<Scope, Error> for Bill {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Refresh => true,
                LiveInput::Params(query) => {
                    self.balance_input = super::credits::balance_query(query)?;
                    self.balance_pins = Value::Null;
                    true
                }
                LiveInput::Event(v) => self.event(r.scope, r.credential, v).await?,
            };
            if reload {
                self.reload(r.scope).await?;
            }
            self.open_requested(r.scope, r.credential).await?;
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query: super::credits::balance_params(&self.balance_pins),
                ..LiveBody::default()
            })
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        if self.document.is_none() || self.balance_pins.is_object() {
            return vec![];
        }
        vec![PanelSpec {
            id: "payables-bill-attachments".into(),
            embed: bilibi_finance_attachments::PANEL_EMBED.into(),
            path: [
                ("panel".into(), "payables-bill-attachments".into()),
                ("source".into(), crate::DOCUMENT_SOURCE.into()),
                ("document".into(), self.id.to_string()),
            ]
            .into(),
            query: HashMap::new(),
        }]
    }
}

fn columns(id: &str, cols: &[(&str, &str)], right: &[&str]) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        cols.iter()
            .map(|(id, label)| {
                let c = BuiltinColumn::new(id, label);
                if right.contains(id) {
                    c.right()
                } else {
                    c
                }
            })
            .collect(),
    )
    .map_err(|_| Error::Internal)
}
fn table(columns: &ListColumns, event: &str, caption: &str, rows: Vec<FlexRow>) -> Result<Html> {
    let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
    table.event = event;
    table.caption = caption;
    table.framed = false;
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    render(table)
}
// Unlike form_values' general 1 KiB limit, 500 Unicode characters can use
// 2 KiB. Pass the whole known field for facade validation, never truncate it.
fn reason(v: &serde_json::Value) -> Option<&str> {
    v["dialog[reason]"].as_str()
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% import "feedback.html" as feedback %}{% call feedback::flash_group(messages=flash) %}{% endcall %}{% call ui::page(id="payables-bill", variant=bilibi_ui::PageVariant::Detail) %}{{ missing|safe }}{% endcall %}"#,
    ext = "html"
)]
struct Missing<'a> {
    flash: FlashMap<'a>,
    missing: Html,
}
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/payables/bills/{{ record }}" class="text-action hover:underline">Bill #{{ record }}</a>"#,
    ext = "html"
)]
struct Corrects {
    record: i64,
}
#[derive(Template)]
#[template(path = "bill_attention.html")]
struct Attention {
    approved: bool,
    has_match: bool,
    exceptions: Vec<ExceptionView>,
}
struct ExceptionView {
    line: i32,
    code: String,
    detail: String,
}
#[derive(Template)]
#[template(
    source = r#"{% for detail in details %}{% if !compact || loop.first %}<span class="block text-xs text-ink">{{ detail }}</span>{% endif %}{% endfor %}"#,
    ext = "html"
)]
struct MatchResult<'a> {
    details: Vec<&'a str>,
    compact: bool,
}
#[derive(Template)]
#[template(path = "bill_dialog.html")]
struct DialogView {
    modal_id: String,
    form_id: String,
    close_id: String,
    confirm_id: String,
    title: &'static str,
    description: &'static str,
    event: &'static str,
    cancel: &'static str,
    confirm: &'static str,
    working: &'static str,
    variant: ButtonVariant,
    reason_field: Field,
}
impl DialogView {
    fn new(kind: &str, reason: &str) -> Self {
        let exception = kind == "approve-exception";
        let base = format!("payables-bill-{kind}");
        Self {
            modal_id: format!("{base}-modal"),
            form_id: format!("{base}-form"),
            close_id: format!("{base}-close"),
            confirm_id: format!("{base}-confirm"),
            title: if exception {
                "Approve the match exceptions"
            } else {
                "Void this document"
            },
            description: if exception {
                "The bill may then be posted over the exceptions its last hold recorded. The approval is kept with your name and the reason."
            } else {
                "The document stays on record as voided and cannot be approved or posted; it cannot be undone."
            },
            event: if exception {
                "approve_exception"
            } else {
                "void"
            },
            cancel: if exception {
                "Keep held"
            } else {
                "Keep document"
            },
            confirm: if exception {
                "Approve exceptions"
            } else {
                "Void document"
            },
            working: if exception {
                "Approving…"
            } else {
                "Voiding…"
            },
            variant: if exception {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Danger
            },
            reason_field: Field::in_form("dialog", "reason")
                .id(format!("{base}-reason"))
                .kind(bilibi_ui::InputKind::Textarea)
                .label("Reason (1 to 500 characters)")
                .value(reason)
                .attr("maxlength", "500")
                .required(),
        }
    }
}
