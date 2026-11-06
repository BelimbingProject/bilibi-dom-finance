//! Bilimbi `Web.DocumentLive`: immutable snapshots and freshly authorized actions.
use super::*;
use crate::{Attention, DocumentView, PartySnapshot, StatusEvent, CANCEL, SUBMIT, VIEW};
use bilibi_ui::{
    params, BuiltinColumn, ConfirmDialog, EmptyState, Fact, Facts, Field, FlashMap, InputKind,
    Modal, RecordLink, StatusKind, NO_FLASH,
};
use std::collections::HashMap;

pub(super) struct Detail {
    pages: Pages,
    document_id: i64,
    document: Option<DocumentView>,
    lines: ListColumns,
    sources: ListColumns,
    events: ListColumns,
    framed: bool,
    cancelling: bool,
    confirming: bool,
    reason: String,
    copied: Option<bool>,
    flash: Option<(StatusKind, String)>,
}

#[derive(Template)]
#[template(path = "document.html")]
struct View<'a> {
    document: Option<&'a DocumentView>,
    title: String,
    title_actions: Html,
    header_subtitle: Html,
    actions: Html,
    supplier: Vec<String>,
    buyer: Vec<String>,
    authority: Html,
    share: Html,
    lines: Html,
    totals: Html,
    sources: Html,
    events: Html,
    attention: Vec<(&'static str, String)>,
    dialog: Html,
    flash: FlashMap<'a>,
}

#[derive(Template)]
#[template(path = "document_actions.html")]
struct Actions {
    submit: bool,
    refresh: bool,
    cancel: bool,
    pdf_href: Option<String>,
}

#[derive(Template)]
#[template(path = "document_share.html")]
struct Share<'a> {
    url: Option<&'a str>,
    qr: Html,
    copied: Option<bool>,
}

#[derive(Template)]
#[template(path = "document_cancel.html")]
struct CancelForm {
    reason: Field,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{{ status }}<span class="ml-2 text-ink-muted">{{ environment }}</span>{% if d.self_billed %}<span id="einvoice-document-self-billed" class="ml-2">{% call ui::badge(dot=false) %}Self-billed{% endcall %}</span>{% endif %}{% if let Some(month) = d.document.details.consolidation_month %}<span id="einvoice-document-consolidated" class="ml-2">{% call ui::badge(dot=false) %}Consolidated {{ month }}{% endcall %}</span>{% endif %}"#,
    ext = "html"
)]
struct Subtitle<'a> {
    d: &'a DocumentView,
    environment: String,
    status: Html,
}

fn columns(name: &str, fields: &[(&str, &str)]) -> Result<ListColumns> {
    ListColumns::mount(
        name,
        fields
            .iter()
            .map(|(key, label)| {
                let column = BuiltinColumn::new(key, label);
                if ["quantity", "unit_price", "amount", "tax", "payable"].contains(key) {
                    column.right()
                } else {
                    column
                }
            })
            .collect(),
    )
    .map_err(|_| Error::Internal)
}
fn text(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("—")
}
fn party_lines(p: &PartySnapshot) -> Vec<String> {
    [
        p.name.clone(),
        Some(format!("TIN {}", p.tax_id.as_deref().unwrap_or(""))),
        p.registration_number
            .as_ref()
            .map(|n| format!("{} {n}", p.registration_type.as_deref().unwrap_or("Reg."))),
        Some(p.address.lines.join(", ")),
        Some(
            [
                &p.address.postcode,
                &p.address.city,
                &p.address.state_code,
                &p.address.country_code,
            ]
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
        ),
        p.phone.clone(),
        p.email.clone(),
    ]
    .into_iter()
    .flatten()
    .filter(|v| !v.is_empty())
    .collect()
}
fn event_detail(e: &StatusEvent) -> String {
    [
        e.authority_status.as_ref().map(|v| format!("status {v}")),
        e.submission_uid.as_ref().map(|v| format!("submission {v}")),
        e.document_uuid.as_ref().map(|v| format!("UUID {v}")),
        e.ledger_entry_id.map(|v| format!("entry #{v}")),
        e.http_status.map(|v| format!("HTTP {v}")),
        e.reason.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}
fn actor(e: &StatusEvent) -> String {
    match (
        e.actor_type.as_str(),
        e.actor_user_id,
        e.actor_impersonator_id,
    ) {
        ("user", Some(id), None) => format!("User #{id}"),
        ("user", Some(id), Some(by)) => format!("User #{id} (impersonated by #{by})"),
        ("system", _, _) => format!("System · {}", text(&e.system_principal)),
        _ => "—".into(),
    }
}
fn attention(flag: Attention) -> (&'static str, String) {
    let authority = Einvoice::authority();
    match flag {
        Attention::Overdue => (
            "overdue",
            format!(
                "Not submitted within {} hours of the authority becoming unavailable; the submission keeps retrying.",
                authority.outage_notice_hours()
            ),
        ),
        Attention::RejectionRequested => (
            "rejection_requested",
            format!(
                "The buyer requested rejection. You may cancel within the {}-hour window or issue the appropriate credit, debit or refund note e-Invoice linked to the original.",
                authority.cancellation_window_hours()
            ),
        ),
        Attention::Unposted => (
            "unposted",
            "Validated but not yet posted to the ledger.".into(),
        ),
        Attention::PostingFailed => (
            "posting_failed",
            "The ledger posting failed; see the last posting event.".into(),
        ),
        Attention::Unreversed => (
            "unreversed",
            "Cancelled but its ledger entry has not been reversed.".into(),
        ),
        Attention::ReversalFailed => (
            "reversal_failed",
            "The ledger reversal failed; see the last reversal event.".into(),
        ),
    }
}

impl Detail {
    pub fn new(pages: &Pages, path: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let document_id = path
            .get("id")
            .and_then(|id| id.parse().ok())
            .filter(|id| *id > 0)
            .unwrap_or(0);
        Ok(Self {
            pages: pages.clone(),
            document_id,
            document: None,
            framed,
            lines: columns(
                "einvoice-lines",
                &[
                    ("number", "#"),
                    ("description", "Description"),
                    ("classification", "Classification"),
                    ("quantity", "Quantity"),
                    ("unit_price", "Unit price"),
                    ("amount", "Amount"),
                    ("tax", "Tax"),
                ],
            )?,
            sources: columns(
                "einvoice-sources",
                &[
                    ("number", "Number"),
                    ("issued", "Issued"),
                    ("buyer", "Buyer"),
                    ("lines", "On lines"),
                    ("payable", "Payable"),
                ],
            )?,
            events: columns(
                "einvoice-events",
                &[
                    ("number", "#"),
                    ("when", "When"),
                    ("event", "Event"),
                    ("detail", "Detail"),
                    ("by", "By"),
                ],
            )?,
            cancelling: false,
            confirming: false,
            reason: String::new(),
            copied: None,
            flash: None,
        })
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.document = match self
            .pages
            .einvoice
            .get_document(scope, company(scope)?, self.document_id)
            .await
        {
            Ok(d) => Some(d),
            Err(EinvoiceError::Failed(error)) => return Err(error),
            Err(_) => None,
        };
        if let Some(d) = &self.document {
            self.lines
                .load(&d.document.lines, |l| l.line_number.to_string());
            self.sources.load(&d.sources, |s| s.document.id.to_string());
            self.events.load(&d.events, |e| e.sequence.to_string());
        }
        Ok(())
    }
    pub(super) fn title(&self) -> Option<String> {
        self.document.as_ref().map(|d| {
            format!(
                "{} {}",
                type_label(&d.document.document_type),
                d.document.code_number
            )
        })
    }
    async fn event(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<bool> {
        let action = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(Error::Invalid)?;
        match action {
            "clear-flash" => {
                messages::clear_flash(&mut self.flash, value);
                return Ok(false);
            }
            "lines_grid" | "sources_grid" | "events_grid" => {
                let c = match action {
                    "lines_grid" => &mut self.lines,
                    "sources_grid" => &mut self.sources,
                    _ => &mut self.events,
                };
                c.handle(&params::string_values(value));
                return Ok(false);
            }
            "link-copied" => {
                self.copied = value.get("copied").and_then(serde_json::Value::as_bool);
                return Ok(false);
            }
            "close_cancel" => {
                self.cancelling = false;
                self.confirming = false;
                self.reason.clear();
                return Ok(false);
            }
            "cancel_reason" | "review_cancel" => {
                // In full: `form_values` drops a value over 1 KiB, and a 300
                // character reason can take 1.2 KiB; the facade bounds it.
                self.reason = value["cancel[reason]"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                if action == "cancel_reason" {
                    return Ok(false);
                }
            }
            _ => {}
        }
        let capability = match action {
            "submit" | "refresh" => SUBMIT,
            "open_cancel" | "review_cancel" | "cancel" => CANCEL,
            "generate_pdf" => VIEW,
            _ => return Err(Error::Invalid),
        };
        if !self.pages.allowed(scope, capability).await? {
            self.cancelling = false;
            self.confirming = false;
            let name = if capability == CANCEL {
                "cancel"
            } else {
                action
            };
            self.flash = Some((
                StatusKind::Error,
                messages::refusal(name, EinvoiceError::Unauthorized),
            ));
            return Ok(true);
        }
        if action == "open_cancel" {
            self.cancelling = true;
            self.reason.clear();
            return Ok(false);
        }
        if action == "review_cancel" {
            if self.reason.trim().is_empty() || self.reason.chars().count() > 300 {
                self.flash = Some((
                    StatusKind::Error,
                    messages::refusal("cancel", EinvoiceError::InvalidReason),
                ));
            } else {
                self.cancelling = false;
                self.confirming = true;
            }
            return Ok(false);
        }
        // An unsolicited cancel event cannot skip the confirmation.
        if action == "cancel" && !self.confirming {
            return Ok(false);
        }
        let facade = &self.pages.einvoice;
        let company = company(scope)?;
        let outcome = match action {
            "submit" => facade
                .submit_document(scope, company, self.document_id)
                .await
                .map(|_| {
                    (
                        StatusKind::Success,
                        "Submission queued. The status updates as the authority answers.".into(),
                    )
                }),
            "refresh" => facade
                .refresh_status(scope, company, self.document_id)
                .await
                .map(|d| {
                    (
                        StatusKind::Info,
                        format!("Status read from the authority: {}.", d.status),
                    )
                }),
            "cancel" => facade
                .cancel_document(scope, company, self.document_id, &self.reason)
                .await
                .map(|d| {
                    let message = if d.already_cancelled {
                        "The tax authority had already cancelled this document. Its status is now updated."
                    } else {
                        "The authority cancelled the document."
                    };
                    (StatusKind::Success, message.into())
                }),
            _ => facade
                .generate_pdf(scope, company, self.document_id)
                .await
                .map(|_| (StatusKind::Success, "PDF generated.".into())),
        };
        self.flash =
            Some(outcome.unwrap_or_else(|e| (StatusKind::Error, messages::refusal(action, e))));
        self.cancelling = false;
        self.confirming = false;
        self.reason.clear();
        Ok(true)
    }
    async fn body(&self, scope: &Scope) -> Result<String> {
        let display = self.pages.display(scope).await?;
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Info => flash.info = message,
                _ => flash.error = message,
            }
        }
        let mut view = View {
            document: self.document.as_ref(),
            title: self.title().unwrap_or_default(),
            title_actions: Html::EMPTY,
            header_subtitle: Html::EMPTY,
            actions: Html::EMPTY,
            supplier: vec![],
            buyer: vec![],
            authority: Html::EMPTY,
            share: Html::EMPTY,
            lines: Html::EMPTY,
            totals: Html::EMPTY,
            sources: Html::EMPTY,
            events: Html::EMPTY,
            attention: vec![],
            dialog: Html::EMPTY,
            flash,
        };
        if let Some(d) = &self.document {
            view.title_actions = bilibi_ui::Html::EMPTY;
            let can_submit = self.pages.allowed(scope, SUBMIT).await?;
            let can_cancel = self.pages.allowed(scope, CANCEL).await?;
            view.header_subtitle = render(Subtitle {
                d,
                environment: sentence(&d.document.environment),
                status: status_badge(&d.status)?,
            })?;
            view.supplier = party_lines(&d.document.supplier);
            view.buyer = party_lines(&d.document.buyer);
            view.attention = d.attention.iter().copied().map(attention).collect();
            view.actions = render(Actions {
                submit: can_submit
                    && ["issued", "released", "refused", "invalid"].contains(&d.status.as_str())
                    && !d.submission_queued
                    && !(d.consolidated
                        && crate::documents::RELEASING.contains(&d.status.as_str())),
                refresh: can_submit && d.document_uuid.is_some(),
                cancel: can_cancel && ["valid", "rejected"].contains(&d.status.as_str()),
                pdf_href: d
                    .pdf_artifact_id
                    .as_ref()
                    .map(|artifact| format!("{ADDRESS}/{}/pdf/{artifact}", d.document.id)),
            })?;
            let at = |element, at: Option<jiff::Timestamp>| -> Result<Html> {
                at.map_or(Ok(Html::text("—")), |at| {
                    render(Time {
                        element,
                        r: display.render(at, Format::DateTime, Precision::Second),
                    })
                })
            };
            let mut facts = vec![
                Fact {
                    title: "Submission UID",
                    id: "",
                    value: Html::text(text(&d.submission_uid)),
                },
                Fact {
                    title: "Document UUID",
                    id: "",
                    value: Html::text(text(&d.document_uuid)),
                },
                Fact {
                    title: "Validated",
                    id: "",
                    value: at("einvoice-document-validated-at", d.validated_at)?,
                },
                Fact {
                    title: "Cancellable until",
                    id: "",
                    value: at("einvoice-document-cancellable-until", d.cancellable_until)?,
                },
                Fact {
                    title: "Ledger entry",
                    id: "",
                    value: Html::text(&format!(
                        "{}{}",
                        d.ledger_entry_id.map_or("—".into(), |id| format!("#{id}")),
                        d.reversal_entry_id
                            .map_or(String::new(), |id| format!(" (reversed by #{id})"))
                    )),
                },
            ];
            if d.recognised_by_source {
                facts.push(Fact {
                    title: "Recognised by",
                    id: "einvoice-document-source-links",
                    value: Html::text(
                        &d.source_links
                            .iter()
                            .map(|s| {
                                format!("{} {} (entry #{})", s.module, s.record, s.ledger_entry_id)
                            })
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                });
            }
            view.authority = render(Facts {
                id: "einvoice-document-authority-facts",
                items: facts,
            })?;
            // Only qrcode's geometry serializer reaches raw HTML, never user SVG.
            let qr = d
                .validation_url
                .as_deref()
                .and_then(|s| crate::qr_code_svg(s, 160))
                .map_or(Html::EMPTY, Html::raw);
            view.share = render(Share {
                url: d.validation_url.as_deref(),
                qr,
                copied: self.copied,
            })?;
            let rows = d
                .document
                .lines
                .iter()
                .map(|l| {
                    let mut row = FlexRow::new(l.line_number.to_string());
                    row.dom_id = Some(format!("einvoice-line-{}", l.line_number));
                    row.cell("number", Cell::text(l.line_number.to_string()))
                        .cell("description", Cell::text(&l.description))
                        .cell("classification", Cell::text(&l.classification_code))
                        .cell(
                            "quantity",
                            Cell::text(format!(
                                "{} {}",
                                l.quantity,
                                l.unit_code.as_deref().unwrap_or("")
                            )),
                        )
                        .cell("unit_price", Cell::text(l.unit_price.to_string()))
                        .cell("amount", Cell::text(l.line_extension_amount.to_string()))
                        .cell(
                            "tax",
                            Cell::text(format!(
                                "{} {}% = {}",
                                l.tax_type,
                                l.tax_rate.map_or("—".into(), |r| r.to_string()),
                                l.tax_amount
                            )),
                        )
                })
                .collect();
            view.lines = table(
                "einvoice-document-lines-table",
                "Lines",
                "lines_grid",
                &self.lines,
                rows,
            )?;
            view.totals = render(Facts {
                id: "einvoice-document-totals",
                items: [
                    ("Tax exclusive", d.document.tax_exclusive_amount),
                    ("Tax", d.document.tax_amount),
                    ("Tax inclusive", d.document.tax_inclusive_amount),
                    ("Payable", d.document.payable_amount),
                ]
                .iter()
                .map(|(title, amount)| Fact {
                    title,
                    id: "",
                    value: Html::text(&format!("{amount} {}", d.document.currency_code)),
                })
                .collect(),
            })?;
            let mut rows = Vec::new();
            for source in &d.sources {
                let s = &source.document;
                let key = s.id.to_string();
                let href = format!("{ADDRESS}/{}", s.id);
                let mut row = FlexRow::new(&key);
                row.dom_id = Some(format!("einvoice-source-{}", s.id));
                rows.push(
                    row.slot(
                        "number",
                        render(RecordLink {
                            workspace: self.framed,
                            kind: "finance/einvoice",
                            record_id: &key,
                            href: &href,
                            class: "",
                            body: Html::text(&s.code_number),
                        })?,
                    )
                    .cell(
                        "issued",
                        time_cell(&display, s.issued_at, Precision::Minute),
                    )
                    .cell(
                        "buyer",
                        Cell::text(s.counterparty_name.as_deref().unwrap_or("")),
                    )
                    .cell(
                        "lines",
                        Cell::text(
                            source
                                .line_numbers
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                    )
                    .cell(
                        "payable",
                        Cell::text(format!("{} {}", s.payable_amount, s.currency_code)),
                    ),
                );
            }
            view.sources = table(
                "einvoice-document-sources-table",
                "Covered documents",
                "sources_grid",
                &self.sources,
                rows,
            )?;
            let mut rows = Vec::new();
            for e in &d.events {
                let mut row = FlexRow::new(e.sequence.to_string());
                row.dom_id = Some(format!("einvoice-event-{}", e.sequence));
                rows.push(
                    row.cell("number", Cell::text(e.sequence.to_string()))
                        .cell(
                            "when",
                            time_cell(&display, e.recorded_at, Precision::Second),
                        )
                        .slot(
                            "event",
                            render(Badge {
                                kind: BadgeKind::Neutral,
                                dot: false,
                                class: "",
                                body: Html::text(&sentence(&e.event)),
                            })?,
                        )
                        .cell("detail", Cell::text(event_detail(e)))
                        .cell("by", Cell::text(actor(e))),
                );
            }
            view.events = table(
                "einvoice-document-events-table",
                "Status events",
                "events_grid",
                &self.events,
                rows,
            )?;
        }
        view.dialog = if self.confirming {
            render(ConfirmDialog {
                id: "einvoice-document-cancel-confirmation",
                consequence: "Cancel this document",
                detail: "This requests cancellation from the authority. If accepted, the ledger reversal is attempted; check its result on this document.",
                confirm: "Cancel document",
                working: "Cancelling…",
                confirm_event: "cancel",
                cancel_event: "close_cancel",
            })?
        } else if self.cancelling {
            render(Modal {
                id: "einvoice-document-cancel-modal",
                title: "Cancel this document",
                cancel_event: "close_cancel",
                description: "This requests cancellation from the authority. If accepted, the ledger reversal is attempted; check its result on this document.",
                body: render(CancelForm {
                    reason: Field::in_form("cancel", "reason")
                        .id("einvoice-document-cancel-reason")
                        .kind(InputKind::Textarea)
                        .label("Reason (1 to 300 characters)")
                        .value(&self.reason)
                        .required()
                        .attr("maxlength", "300"),
                })?,
                flash: Some(flash),
                ..Default::default()
            })?
        } else {
            Html::EMPTY
        };
        view.render().map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for Detail {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match request.input {
                LiveInput::Event(value) => {
                    // Bilimbi keeps a flash across table, copy and typing
                    // events; only an action replaces it.
                    let action = value.get("type").and_then(serde_json::Value::as_str);
                    if !matches!(
                        action,
                        Some("lines_grid" | "sources_grid" | "events_grid")
                            | Some(
                                "link-copied" | "cancel_reason" | "close_cancel" | "clear-flash"
                            )
                    ) {
                        self.flash = None;
                    }
                    self.event(request.scope, value).await?
                }
                _ => true,
            };
            if reload || self.document.is_none() {
                self.load(request.scope).await?;
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                ..Default::default()
            })
        })
    }
}
