//! Bilimbi `Web.DocumentLive`: immutable snapshots and freshly authorized actions.
use super::*;
use crate::{Attention, DocumentView, PartySnapshot, StatusEvent, CANCEL, SUBMIT, VIEW};
use bilibi_finance_ledger::Money;
use bilibi_ui::{
    params, BackLink, BuiltinColumn, Button, ButtonVariant, ConfirmDialog, EmptyState, Fact, Facts,
    Field, FlashMap, Fold, IconButton, InputKind, Modal, RecordLink, StatusKind, NO_FLASH,
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
    attention: Vec<(&'static str, Html)>,
    dialog: Html,
    flash: FlashMap<'a>,
}

/// What the document's header offers.
struct Actions {
    submit: bool,
    refresh: bool,
    cancel: bool,
    pdf_href: Option<String>,
}

impl Actions {
    /// Back, the document's own actions as icon buttons with their names as
    /// tooltips, then the one primary verb (Submit to authority) at the end.
    fn render(&self) -> Result<Html> {
        let mut html = render(BackLink {
            href: ADDRESS,
            id: "einvoice-document-back",
            title: "Back to e-invoices",
            ..BackLink::default()
        })?;
        let mut icon = |id: &str, icon: &str, label: &str, event: &str, href: &str| {
            render(IconButton {
                id,
                event,
                href,
                button_type: if href.is_empty() { "button" } else { "" },
                title: label,
                ..IconButton::new(icon, label)
            })
            .map(|button| html.push(&button))
        };
        if self.refresh {
            icon(
                "einvoice-document-refresh",
                "arrow-path",
                "Refresh status",
                "refresh",
                "",
            )?;
        }
        if self.cancel {
            icon(
                "einvoice-document-open-cancel",
                "x-mark",
                "Cancel document",
                "open_cancel",
                "",
            )?;
        }
        icon(
            "einvoice-document-generate-pdf",
            "document-plus",
            "Generate PDF",
            "generate_pdf",
            "",
        )?;
        if let Some(link) = &self.pdf_href {
            icon(
                "einvoice-document-download-pdf",
                "arrow-down-tray",
                "Download PDF",
                "",
                link,
            )?;
        }
        if self.submit {
            html.push(&render(Button {
                id: "einvoice-document-submit",
                event: "submit",
                button_type: "button",
                variant: ButtonVariant::Primary,
                body: Html::text("Submit to authority"),
                ..Button::default()
            })?);
        }
        Ok(html)
    }
}

/// A ledger entry this document posted or reversed, linked to its journal.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="text-link hover:underline">{{ text }}</a>"#,
    ext = "html"
)]
struct EntryById<'a> {
    id: i64,
    text: &'a str,
}

/// A line on the page with a link to where it is acted on.
#[derive(Template)]
#[template(
    source = r#"{{ text }} <a href="{{ href }}" class="text-link hover:underline">{{ link }}</a>"#,
    ext = "html"
)]
struct WithLink<'a> {
    text: &'a str,
    href: &'a str,
    link: &'a str,
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
/// What an event means to a person; the authority's identifiers and answer
/// codes wait under a Details fold, and the ledger entry it names is a link
/// to its journal.
fn event_detail(e: &StatusEvent) -> Result<Html> {
    // A transport code is for the fold, never the sentence.
    let reason = e
        .reason
        .as_deref()
        .filter(|reason| !reason.starts_with("HTTP "));
    let text = match (e.event.as_str(), reason) {
        (_, Some(reason)) => reason.to_owned(),
        ("submitted", None) => format!("Sent to {}", Einvoice::authority().name()),
        _ => String::new(),
    };
    let identifiers = [
        e.authority_status
            .as_ref()
            .map(|v| format!("Status at the authority: {v}")),
        e.submission_uid.as_ref().map(|v| format!("Submission {v}")),
        e.document_uuid.as_ref().map(|v| format!("UUID {v}")),
        e.http_status.map(|v| format!("Answer code {v}")),
        e.reason
            .as_ref()
            .filter(|reason| reason.starts_with("HTTP "))
            .cloned(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let mut html = Html::text(&text);
    if !identifiers.is_empty() {
        html.push(&render(Fold {
            id: &format!("einvoice-event-{}-details", e.sequence),
            title: "Details",
            summary: "",
            open: false,
            body: Html::text(&identifiers),
        })?);
    }
    if let Some(id) = e.ledger_entry_id {
        if !text.is_empty() {
            html.push(&Html::text(" · "));
        }
        html.push(&render(EntryById {
            id,
            text: "Open journal",
        })?);
    }
    Ok(html)
}
fn actor(e: &StatusEvent, people: &bilibi_finance_ledger::People) -> String {
    match (e.actor_type.as_str(), e.actor_user_id) {
        ("user", Some(id)) => people.who(Some(id), e.actor_impersonator_id),
        ("system", _) => format!("System · {}", text(&e.system_principal)),
        _ => "—".into(),
    }
}
/// A posting or reversal that needs attention is retried on the
/// reconciliation page, at this document's row.
fn attention(flag: Attention, d: &DocumentView) -> Result<(&'static str, Html)> {
    let id = d.document.id;
    let (key, mut message) = attention_text(flag);
    // Why the last try failed, in the words the posting recorded.
    let why = |event: &str| {
        d.events
            .iter()
            .rev()
            .find(|e| e.event == event)
            .and_then(|e| e.reason.clone())
    };
    let mut retry = match flag {
        Attention::Unposted | Attention::PostingFailed => Some((
            format!("/finance/einvoice/reconciliation#einvoice-unposted-{id}"),
            "Post it again",
        )),
        Attention::Unreversed | Attention::ReversalFailed => Some((
            format!("/finance/einvoice/reconciliation#einvoice-unreversed-{id}"),
            "Reverse it again",
        )),
        _ => None,
    };
    match flag {
        Attention::PostingFailed => {
            if let Some(reason) = why("posting_failed") {
                // Accounts not chosen yet are chosen on the setup page.
                if reason.starts_with("Choose the ") {
                    retry = Some(("/finance/einvoice/setup".into(), "Choose accounts"));
                }
                message = format!("Valid, but not in the books yet. {reason}");
            }
        }
        Attention::ReversalFailed => {
            if let Some(reason) = why("reversal_failed") {
                message = format!("Cancelled, but its journal entry is not reversed yet. {reason}");
            }
        }
        _ => {}
    }
    let html = match retry {
        Some((href, link)) => render(WithLink {
            text: &message,
            href: &href,
            link,
        })?,
        None => Html::text(&message),
    };
    Ok((key, html))
}

fn attention_text(flag: Attention) -> (&'static str, String) {
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
        Attention::Unposted => ("unposted", "Valid, but not in the books yet.".into()),
        Attention::PostingFailed => (
            "posting_failed",
            "Valid, but not in the books yet.".into(),
        ),
        Attention::Unreversed => (
            "unreversed",
            "Cancelled, but its journal entry is not reversed yet.".into(),
        ),
        Attention::ReversalFailed => (
            "reversal_failed",
            "Cancelled, but its journal entry is not reversed yet.".into(),
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
            )?
            .with_record_fields([bilibi_ui::RecordField::new("unit", "Unit")]),
            sources: columns(
                "einvoice-sources",
                &[
                    ("number", "Number"),
                    ("issued", "Issued"),
                    ("buyer", "Buyer"),
                    ("lines", "On lines"),
                    ("payable", "Total"),
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
            view.attention = d
                .attention
                .iter()
                .map(|flag| attention(*flag, d))
                .collect::<Result<_>>()?;
            view.actions = Actions {
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
            }
            .render()?;
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
                    value: match (d.ledger_entry_id, d.reversal_entry_id) {
                        (None, _) => Html::text("—"),
                        (Some(id), reversal) => {
                            let mut html = render(EntryById {
                                id,
                                text: "Open journal",
                            })?;
                            if let Some(reversal) = reversal {
                                html.push(&Html::text(" · "));
                                html.push(&render(EntryById {
                                    id: reversal,
                                    text: "Open reversal",
                                })?);
                            }
                            html
                        }
                    },
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
                        // The stored precision and unit code are the
                        // authority's; the unit waits in the column picker.
                        .cell("quantity", Cell::text(l.quantity.normalize().to_string()))
                        .cell("unit", Cell::text(l.unit_code.as_deref().unwrap_or("—")))
                        .cell(
                            "unit_price",
                            Cell::text(Money::display_decimal(l.unit_price, None)),
                        )
                        .cell(
                            "amount",
                            Cell::text(Money::display_decimal(l.line_extension_amount, None)),
                        )
                        .cell(
                            "tax",
                            Cell::text(format!(
                                "{} · {}% · {}",
                                l.tax_type,
                                l.tax_rate.map_or("—".into(), |r| r.normalize().to_string()),
                                Money::display_decimal(l.tax_amount, None)
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
                    ("Total", d.document.payable_amount),
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
            let people = self
                .pages
                .people(
                    scope,
                    d.events
                        .iter()
                        .flat_map(|e| [e.actor_user_id, e.actor_impersonator_id]),
                )
                .await?;
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
                        .slot("detail", event_detail(e)?)
                        .cell("by", Cell::text(actor(e, &people))),
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
