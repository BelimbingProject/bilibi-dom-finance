//! Bilimbi Einvoice.Web.{ConsolidationLive,ReconciliationLive,SetupLive}
//! at 4abfaf829321f8540581d833752a086b21b45ba9. The facade owns every read
//! and mutation; page state retains exactly the candidates shown at load.
use crate::{
    Authority, Candidates, CertificateSetup, ConsolidationRequest, DocumentSummary, Einvoice,
    EinvoiceError, LoginOutcome, Reconciliation, Setup, ISSUE, SETTINGS, SUBMIT,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_finance_ledger::POST_ENTRIES;
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    forms::{Field, InputKind},
    lists::RecordLink,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Button, Card, EmptyState, Fact, Facts, FlashMap,
    FlexRow, FlexTable, Header, Html, IconButton, ListColumns, ListSpec, ListState, PageVariant,
    SectionHeading, StatusKind, TableEmpty, NO_FLASH,
};
use jiff::{civil::Date, Timestamp};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

pub(crate) const SERVICE: &str = "finance/einvoice.control_pages";

#[derive(Clone)]
pub(crate) struct Pages {
    pub einvoice: Arc<Einvoice>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

struct State {
    pages: Pages,
    operation: &'static str,
    framed: bool,
    month: Date,
    month_control_revision: u64,
    code_number: String,
    candidates: Option<Candidates>,
    reconciliation: Option<Reconciliation>,
    setup: Option<Setup>,
    columns: Vec<ListColumns>,
    list: ListState,
    flash: Option<(StatusKind, String)>,
    redirect: Option<String>,
}

impl State {
    fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let tables: &[(&str, &[(&str, &str)])] = match request.operation {
            "consolidation" => &[
                (
                    "einvoice-consolidation-candidates-table",
                    &[
                        ("number", "Number"),
                        ("issued", "Issued"),
                        ("buyer", "Buyer"),
                        ("status", "Status"),
                        ("payable", "Payable"),
                    ],
                ),
                (
                    "einvoice-consolidation-refused-table",
                    &[("number", "Number"), ("payable", "Payable"), ("why", "Why")],
                ),
            ],
            "reconciliation" => &[
                (
                    "einvoice-unposted-table",
                    &[
                        ("number", "Number"),
                        ("type", "Type"),
                        ("payable", "Payable"),
                        ("reason", "Last failure"),
                    ],
                ),
                (
                    "einvoice-unreversed-table",
                    &[
                        ("number", "Number"),
                        ("status", "Status"),
                        ("entry", "Entry"),
                        ("reason", "Last failure"),
                    ],
                ),
            ],
            "setup" => &[],
            _ => return Err(Error::NotFound),
        };
        let columns = tables
            .iter()
            .map(|(id, fields)| {
                ListColumns::mount(
                    id,
                    fields
                        .iter()
                        .map(|(key, label)| {
                            let column = BuiltinColumn::new(key, label);
                            if request.operation == "consolidation" && *key == "payable" {
                                column.right()
                            } else {
                                column
                            }
                        })
                        .collect(),
                )
                .map_err(|_| Error::Internal)
            })
            .collect::<Result<Vec<_>>>()?;
        // Bilimbi has no sort, filters or paging on these bounded facade reads.
        let spec = ListSpec {
            omit_defaults: true,
            ..ListSpec::default()
        }
        .shared()
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            operation: request.operation,
            framed: request.framed,
            month: month(request.query.get("month").map(String::as_str)),
            month_control_revision: 0,
            code_number: String::new(),
            candidates: None,
            reconciliation: None,
            setup: None,
            columns,
            list: ListState::parse(request.query, spec).map_err(|_| Error::Internal)?,
            flash: None,
            redirect: None,
        })
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let company = company(scope)?;
        match self.operation {
            "consolidation" => {
                self.candidates = self
                    .pages
                    .einvoice
                    .consolidation_candidates(scope, company, self.month, Timestamp::now())
                    .await
                    .ok()
            }
            "reconciliation" => {
                self.reconciliation = self
                    .pages
                    .einvoice
                    .reconciliation(scope, company)
                    .await
                    .ok()
            }
            "setup" => self.setup = self.pages.einvoice.setup(scope, company).await.ok(),
            _ => unreachable!(),
        }
        Ok(())
    }

    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.pages.einvoice.db.get().await?;
        Ok(
            crate::authorize(&**client, scope, company(scope)?, capability)
                .await
                .is_ok(),
        )
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let event = value["type"].as_str().unwrap_or_default();
        if let Some(index) = event
            .strip_prefix("grid_")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if let Some(columns) = self.columns.get_mut(index) {
                columns.handle(&params::string_values(value));
            }
            return Ok(());
        }
        if event == "clear-flash" {
            super::messages::clear_flash(&mut self.flash, value);
            return Ok(());
        }
        let company = company(scope)?;
        match (self.operation, event) {
            ("consolidation", "month") => {
                self.month = month(value["month"].as_str());
                self.load(scope).await?;
            }
            ("consolidation", "issue_change") => {
                self.code_number = issue_number(value);
            }
            ("consolidation", "issue") => {
                self.code_number = issue_number(value);
                let result = if !self.allowed(scope, ISSUE).await? {
                    Err(EinvoiceError::Unauthorized)
                } else if let Some(candidates) =
                    self.candidates.as_ref().filter(|c| !c.documents.is_empty())
                {
                    // Never use None (which would include documents arriving after load).
                    self.pages
                        .einvoice
                        .issue_consolidated_document(
                            scope,
                            company,
                            &ConsolidationRequest {
                                month: self.month,
                                code_number: Some(self.code_number.clone()),
                                document_ids: Some(
                                    candidates.documents.iter().map(|d| d.id).collect(),
                                ),
                            },
                            Timestamp::now(),
                        )
                        .await
                } else {
                    Err(EinvoiceError::NothingToConsolidate)
                };
                match result {
                    Ok(view) => {
                        self.flash = Some((
                            StatusKind::Success,
                            format!(
                                "Consolidated document {} issued.",
                                view.document.code_number
                            ),
                        ));
                        self.redirect =
                            Some(format!("/finance/einvoice/documents/{}", view.document.id));
                    }
                    Err(error) => {
                        self.flash = Some((StatusKind::Error, issue_error(&error)));
                        self.load(scope).await?;
                    }
                }
            }
            ("reconciliation", "retry_posting" | "retry_reversal") => {
                let id = params::positive_id(value["id"].as_str()).unwrap_or(0);
                let posting = event == "retry_posting";
                let result = if !self.allowed(scope, SUBMIT).await?
                    || !self.allowed(scope, POST_ENTRIES).await?
                {
                    Err(EinvoiceError::Unauthorized)
                } else if posting {
                    self.pages.einvoice.retry_posting(scope, company, id).await
                } else {
                    self.pages.einvoice.retry_reversal(scope, company, id).await
                };
                self.flash = Some(match result {
                    Ok(_) => (
                        StatusKind::Success,
                        if posting {
                            "The entry was posted."
                        } else {
                            "The entry was reversed."
                        }
                        .into(),
                    ),
                    Err(error) => (StatusKind::Error, retry_error(&error)),
                });
                self.load(scope).await?;
            }
            ("setup", "test_connection") => {
                let environment = self.setup.as_ref().map_or("", |s| s.environment.as_str());
                self.flash = Some(if !self.allowed(scope, SETTINGS).await? {
                    (
                        StatusKind::Error,
                        connection_error(&EinvoiceError::Unauthorized).into(),
                    )
                } else {
                    match self.pages.einvoice.test_connection(scope, company).await {
                        Ok(LoginOutcome::Succeeded) => (
                            StatusKind::Info,
                            format!(
                                "Connected: the authority accepted the {environment} credentials."
                            ),
                        ),
                        Ok(outcome) => (StatusKind::Error, connection_outcome(outcome)),
                        Err(error) => (StatusKind::Error, connection_error(&error).into()),
                    }
                });
                self.load(scope).await?;
            }
            _ => (),
        }
        Ok(())
    }

    async fn render(&self, scope: &Scope) -> Result<String> {
        let display = {
            let client = self.pages.einvoice.db.get().await?;
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
        let (title, subtitle) = match self.operation {
            "consolidation" => (
                "E-invoice consolidation",
                "One consolidated document reports a month's sales that no buyer asked an individual e-invoice for.",
            ),
            "reconciliation" => (
                "E-invoice reconciliation",
                "Validated documents without a ledger entry, and entries whose document is cancelled.",
            ),
            _ => (
                "E-invoice setup",
                "Credentials, certificate, and supplier profile for this company.",
            ),
        };
        let page_id = format!("einvoice-{}", self.operation);
        let setup = self.operation == "setup";
        // Setup's values are edited in System settings: the link opens this
        // company's e-invoice settings there, not the whole list.
        let settings = settings_href(scope);
        let mut parts = vec![render(Header {
            title: Html::text(title),
            title_actions: bilibi_ui::Html::EMPTY,
            subtitle,
            actions: render(Button {
                id: if setup {
                    "einvoice-edit-settings"
                } else if self.operation == "consolidation" {
                    "einvoice-consolidation-documents"
                } else {
                    "einvoice-reconciliation-documents"
                },
                href: if setup {
                    &settings
                } else {
                    "/finance/einvoice/documents"
                },
                body: Html::text(if setup {
                    "Edit e-invoice settings"
                } else {
                    "E-invoices"
                }),
                ..Button::default()
            })?,
        })?];
        match self.operation {
            "consolidation" => parts.extend(self.consolidation(scope, &display).await?),
            "reconciliation" => parts.extend(self.reconciliation(scope).await?),
            _ => parts.extend(self.setup(&display, &settings)?),
        }
        let mut flash = NO_FLASH;
        if let Some((kind, text)) = &self.flash {
            match kind {
                StatusKind::Info => flash.info = text,
                StatusKind::Success => flash.success = text,
                StatusKind::Warning => flash.warning = text,
                StatusKind::Error => flash.error = text,
            }
        }
        PageView {
            page_id: &page_id,
            variant: if setup {
                PageVariant::Detail
            } else {
                PageVariant::List
            },
            flash,
            parts,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    async fn consolidation(&self, scope: &Scope, display: &DisplayContext) -> Result<Vec<Html>> {
        let mut parts = vec![render(ConsolidationForm {
            issue: false,
            month_control_revision: self.month_control_revision,
            field: Field::named("month")
                .id("einvoice-consolidation-month-input")
                .label("Month")
                .kind(InputKind::Month)
                .value(self.month.strftime("%Y-%m").to_string()),
        })?];
        let Some(c) = &self.candidates else {
            parts.push(unavailable(
                "consolidation",
                "Consolidation unavailable",
                "This company's documents cannot be read with your current access.",
            )?);
            return Ok(parts);
        };
        let mut covered = instant("einvoice-consolidation-from", c.from, display)?;
        covered.push(&Html::text(" to "));
        covered.push(&instant("einvoice-consolidation-until", c.until, display)?);
        let mut closes = instant("einvoice-consolidation-closes", c.closes_at, display)?;
        closes.push(&Html::text(" "));
        let window = if c.open {
            (
                "einvoice-consolidation-open",
                "Open now",
                BadgeKind::Success,
            )
        } else {
            (
                "einvoice-consolidation-closed",
                "Not open now",
                BadgeKind::Warning,
            )
        };
        closes.push(&render(WindowBadge {
            window_id: window.0,
            label: window.1,
            kind: window.2,
        })?);
        let mut body = facts(
            "einvoice-consolidation-facts",
            vec![
                ("Covers documents issued", covered),
                ("May be issued until", closes),
                ("Environment", Html::text(&super::sentence(&c.environment))),
                ("Documents", Html::text(&c.documents.len().to_string())),
                (
                    "Tax exclusive",
                    Html::text(&c.line_extension_amount.to_string()),
                ),
                ("Tax", Html::text(&c.tax_amount.to_string())),
                ("Payable", Html::text(&c.payable_amount.to_string())),
            ],
        )?;
        if c.open && !c.documents.is_empty() && self.allowed(scope, ISSUE).await? {
            body.push(&render(ConsolidationForm {
                issue: true,
                month_control_revision: self.month_control_revision,
                field: Field::in_form("issue", "code_number")
                    .id("einvoice-consolidation-code-number")
                    .label("Number of the consolidated document")
                    .value(&self.code_number)
                    .required()
                    .attr("maxlength", "50"),
            })?);
        }
        parts.push(section(
            "einvoice-consolidation-summary",
            "Window and totals",
            "",
            false,
            body,
        )?);
        let rows = c
            .documents
            .iter()
            .map(|d| {
                let id = format!("einvoice-candidate-{}", d.id);
                let mut row = document_row(d, &id, self.framed)?;
                row = row
                    .slot(
                        "issued",
                        instant(
                            &format!("einvoice-candidate-issued-{}", d.id),
                            d.issued_at,
                            display,
                        )?,
                    )
                    .slot(
                        "buyer",
                        Html::text(d.counterparty_name.as_deref().unwrap_or_default()),
                    );
                Ok(row)
            })
            .collect::<Result<Vec<_>>>()?;
        parts.push(section(
            "einvoice-consolidation-candidates",
            "Documents it would cover",
            "",
            true,
            self.table(
                0,
                rows,
                "Documents it would cover",
                "No document of this month is waiting to be consolidated.",
                false,
            )?,
        )?);
        if !c.refused.is_empty() {
            let rows = c
                .refused
                .iter()
                .map(|r| {
                    Ok(document_row(
                        &r.document,
                        &format!("einvoice-refused-{}", r.document.id),
                        self.framed,
                    )?
                    .slot("why", Html::text(&format!("It {}.", r.reason))))
                })
                .collect::<Result<Vec<_>>>()?;
            parts.push(section(
                "einvoice-consolidation-refused",
                "Documents that need an individual e-invoice",
                "",
                true,
                self.table(
                    1,
                    rows,
                    "Documents that need an individual e-invoice",
                    "",
                    false,
                )?,
            )?);
        }
        Ok(parts)
    }

    async fn reconciliation(&self, scope: &Scope) -> Result<Vec<Html>> {
        let Some(reconciliation) = &self.reconciliation else {
            return Ok(vec![unavailable(
                "reconciliation",
                "Reconciliation unavailable",
                "This company's documents cannot be read with your current access.",
            )?]);
        };
        let can_retry =
            self.allowed(scope, SUBMIT).await? && self.allowed(scope, POST_ENTRIES).await?;
        let can_settle =
            !reconciliation.unposted.is_empty() && self.allowed(scope, crate::SETTINGS).await?;
        let mut parts = Vec::new();
        for (index, rows, kind, title, empty, label, event) in [
            (
                0,
                &reconciliation.unposted,
                "unposted",
                "Valid documents without a ledger entry",
                "Every validated document has its ledger entry.",
                "Retry posting",
                "retry_posting",
            ),
            (
                1,
                &reconciliation.unreversed,
                "unreversed",
                "Entries whose document is cancelled",
                "Every entry of a cancelled document has been reversed.",
                "Retry reversal",
                "retry_reversal",
            ),
        ] {
            let rows = rows
                .iter()
                .map(|r| {
                    let id = r.document.id;
                    let mut row =
                        document_row(&r.document, &format!("einvoice-{kind}-{id}"), self.framed)?
                            .slot(
                                "type",
                                Html::text(
                                    &Einvoice::authority()
                                        .document_type_name(&r.document.document_type),
                                ),
                            )
                            .slot(
                                "entry",
                                match r.ledger_entry_id {
                                    Some(id) => render(EntryById { id })?,
                                    None => Html::text("—"),
                                },
                            )
                            .slot(
                                "reason",
                                Html::text(r.reason.as_deref().unwrap_or("not attempted")),
                            );
                    // Retrying acts on this row: an icon button with its name.
                    if can_retry {
                        let tooltip = format!("{label} {}", r.document.code_number);
                        row.actions = render(IconButton {
                            id: &format!("einvoice-{}-{id}", event.replace('_', "-")),
                            event,
                            name: "id",
                            value: &id.to_string(),
                            button_type: "button",
                            title: &tooltip,
                            ..IconButton::new("arrow-path", &tooltip)
                        })?;
                    }
                    Ok(row)
                })
                .collect::<Result<Vec<_>>>()?;
            // A posting refused for its accounts is fixed in the e-invoice
            // settings, which this section links to.
            let mut content = Html::EMPTY;
            if index == 0 && can_settle {
                content.push(&render(SettingsNote {
                    href: &settings_href(scope),
                })?);
            }
            content.push(&self.table(index, rows, title, empty, can_retry)?);
            parts.push(section(
                &format!("einvoice-{kind}"),
                title,
                "",
                true,
                content,
            )?);
        }
        Ok(parts)
    }

    fn setup(&self, display: &DisplayContext, settings: &str) -> Result<Vec<Html>> {
        let edit = |id: &str| {
            render(Button {
                id,
                href: settings,
                body: Html::text("Edit e-invoice settings"),
                ..Button::default()
            })
        };
        let Some(s) = &self.setup else {
            return Ok(vec![unavailable(
                "setup",
                "Setup unavailable",
                "This company's e-invoice setup cannot be read with your current access.",
            )?]);
        };
        let mut connection = facts(
            "einvoice-connection-facts",
            vec![
                ("Environment", Html::text(&super::sentence(&s.environment))),
                (
                    "Client ID",
                    Html::text(if s.client_id_set { "Set" } else { "Not set" }),
                ),
                (
                    "Client secret",
                    Html::text(if s.client_secret_set {
                        "Set"
                    } else {
                        "Not set"
                    }),
                ),
                (
                    "Client secret expires",
                    Html::text(&bilibi_finance_ledger::dates::display_text(
                        s.client_secret_expires_on
                            .as_deref()
                            .unwrap_or("Not recorded"),
                    )),
                ),
            ],
        )?;
        connection.push(&render(Button {
            id: "einvoice-test-connection",
            event: "test_connection",
            button_type: "button",
            body: Html::text("Test connection"),
            ..Button::default()
        })?);
        let mut parts = vec![section(
            "einvoice-connection",
            "Connection",
            "Changed in this company's e-invoice settings.",
            false,
            connection,
        )?];
        let certificate = match &s.certificate {
            CertificateSetup::Missing => render(
                EmptyState::missing(
                    "No certificate",
                    "Add the company's certificate and private key in the e-invoice settings.",
                )
                .id("einvoice-certificate-missing")
                .action(edit("einvoice-certificate-add")?),
            )?,
            CertificateSetup::Unreadable => render(
                EmptyState::missing(
                    "Certificate unreadable",
                    "The stored certificate cannot be read as an X.509 certificate. Add it again.",
                )
                .id("einvoice-certificate-invalid")
                .action(edit("einvoice-certificate-replace")?),
            )?,
            CertificateSetup::Facts(c) => facts(
                "einvoice-certificate-facts",
                vec![
                    ("Issuer", Html::text(&c.issuer_name)),
                    ("Serial number", Html::text(&c.serial_number)),
                    (
                        "TIN on certificate",
                        Html::text(c.tax_id.as_deref().unwrap_or("—")),
                    ),
                    (
                        "Registration number on certificate",
                        Html::text(c.registration_number.as_deref().unwrap_or("—")),
                    ),
                    (
                        "Valid until",
                        instant("einvoice-certificate-expiry", c.not_after, display)?,
                    ),
                    ("Days left", Html::text(&c.days_until_expiry.to_string())),
                    (
                        "Private key",
                        Html::text(if c.private_key_matches {
                            "Matches the certificate"
                        } else {
                            "Missing or does not match the certificate"
                        }),
                    ),
                    (
                        "Supplier TIN",
                        Html::text(if s.tax_id_matches_certificate {
                            "Matches the certificate"
                        } else {
                            "Differs from the certificate"
                        }),
                    ),
                ],
            )?,
        };
        parts.push(section(
            "einvoice-certificate",
            "Signing certificate",
            "",
            false,
            certificate,
        )?);
        if !s.expiring.is_empty() {
            let what = s
                .expiring
                .iter()
                .map(|key| {
                    if *key == "certificate" {
                        "The certificate"
                    } else {
                        "The client secret"
                    }
                })
                .collect::<Vec<_>>()
                .join(" and ");
            let description = format!(
                "{what} expires within {} days or has expired. Submissions fail once it does.",
                s.expiry_alert_days
            );
            parts.push(render(Card {
                id: "einvoice-expiry-alert",
                role: "alert",
                inner_class: "p-5 sm:p-6",
                body: render(SectionHeading {
                    id: "einvoice-expiry-heading",
                    title: "Renew soon",
                    description: &description,
                    ..SectionHeading::default()
                })?,
                ..Card::default()
            })?);
        }
        let supplier = if s.supplier_missing.is_empty() {
            render(SupplierComplete {})?
        } else {
            let missing = s
                .supplier_missing
                .iter()
                .map(|field| supplier_label(field))
                .collect::<Vec<_>>()
                .join(", ");
            render(
                EmptyState::missing(
                    "Supplier profile incomplete",
                    format!("Set {missing} in the e-invoice settings before issuing documents."),
                )
                .id("einvoice-supplier-incomplete")
                .action(edit("einvoice-supplier-complete")?),
            )?
        };
        parts.push(section(
            "einvoice-supplier",
            "Supplier profile",
            "",
            false,
            supplier,
        )?);
        Ok(parts)
    }

    fn table(
        &self,
        index: usize,
        rows: Vec<FlexRow>,
        caption: &str,
        empty: &str,
        actions: bool,
    ) -> Result<Html> {
        let c = &self.columns[index];
        let event = format!("grid_{index}");
        let mut table = FlexTable::new(&c.id, c.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = c.zoom;
        table.suggestions = c.flex_suggestions();
        table.add_query = &c.add_query;
        table.event = &event;
        table.caption = caption;
        table.framed = false;
        table.actions = actions;
        table.empty = Some(TableEmpty::Content(Html::text(empty)));
        render(table)
    }
}

fn company(scope: &Scope) -> Result<i64> {
    scope
        .current_company()
        .map(|id| id.get())
        .ok_or(Error::Forbidden)
}

fn month(value: Option<&str>) -> Date {
    if let Some(date) = value
        .filter(|v| v.len() == 7)
        .and_then(|v| format!("{v}-01").parse::<Date>().ok())
    {
        return date;
    }
    let now = Timestamp::now();
    let this = now
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .first_of_month();
    if Einvoice::authority().consolidation_window(this).until <= now {
        this
    } else {
        this.yesterday()
            .expect("current month has predecessor")
            .first_of_month()
    }
}

fn issue_number(value: &Value) -> String {
    value["issue[code_number]"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn issue_error(error: &EinvoiceError) -> String {
    match error {
        EinvoiceError::PeriodNotEnded => {
            "The month has not ended yet; its consolidated document is issued after it ends.".into()
        }
        EinvoiceError::ConsolidationWindowClosed => {
            "The time the authority allows for this month's consolidated document has passed."
                .into()
        }
        EinvoiceError::NothingToConsolidate => {
            "No document of this month can be consolidated.".into()
        }
        EinvoiceError::CodeNumberTaken => "That number is already used in this environment.".into(),
        EinvoiceError::Unauthorized => "You do not have permission to issue documents.".into(),
        EinvoiceError::CompanyArchived => "The company is archived.".into(),
        EinvoiceError::NotConsolidatable(id) => {
            format!("Document #{id} can no longer be consolidated; reload the page.")
        }
        EinvoiceError::Invalid(fields) => {
            let mut grouped: Vec<(String, Vec<&str>)> = Vec::new();
            for field in fields {
                if let Some((_, messages)) = grouped.iter_mut().find(|(key, _)| key == &field.field)
                {
                    messages.push(&field.message);
                } else {
                    grouped.push((field.field.clone(), vec![&field.message]));
                }
            }
            let errors = grouped
                .iter()
                .map(|(key, messages)| {
                    let path = key
                        .split(['.', '[', ']'])
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            s.parse::<usize>()
                                .map_or_else(|_| s.to_owned(), |n| (n + 1).to_string())
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("{path} {}", messages.join(", "))
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!("The consolidated document was refused: {errors}.")
        }
        // Do not expose database errors or internal response bodies in a flash.
        _ => "The consolidated document could not be issued.".into(),
    }
}

fn retry_error(error: &EinvoiceError) -> String {
    match error {
        EinvoiceError::LedgerFollowUpFailed(reason) => format!("It failed again: {reason}."),
        EinvoiceError::Unauthorized => "You do not have permission to post to the ledger.".into(),
        EinvoiceError::NotPostable => "Only a valid document can be posted.".into(),
        EinvoiceError::NotReversible => "Only a cancelled document can be reversed.".into(),
        EinvoiceError::CompanyArchived => "The company is archived.".into(),
        EinvoiceError::DocumentNotFound => "The document no longer exists.".into(),
        _ => "Nothing was changed. Reload the page and try again.".into(),
    }
}

fn connection_error(error: &EinvoiceError) -> &'static str {
    match error {
        EinvoiceError::MissingCredentials => {
            "Connection not tested: set the client ID and client secret for this environment first."
        }
        EinvoiceError::Unauthorized => {
            "Connection not tested: you do not have permission to manage e-invoice settings."
        }
        _ => "The connection test failed.",
    }
}

fn connection_outcome(outcome: LoginOutcome) -> String {
    match outcome {
        LoginOutcome::MissingCredentials => {
            connection_error(&EinvoiceError::MissingCredentials).into()
        }
        LoginOutcome::Refused(status) => {
            // The HTTP status is for the log, never for a person.
            let _ = status;
            "MyInvois did not accept the client ID and secret. Check both in e-invoice setup."
                .into()
        }
        LoginOutcome::RateLimited { seconds, .. } => {
            format!("The authority is limiting logins. Try again in {seconds} seconds.")
        }
        LoginOutcome::Unreachable => "The authority could not be reached. Try again later.".into(),
        _ => "The connection test failed.".into(),
    }
}

fn supplier_label(field: &str) -> &str {
    match field {
        "tax_id" => "supplier TIN",
        "registration_number" => "registration number",
        "industry_code" => "industry classification code",
        "business_activity" => "business activity",
        "address_line1" => "address line 1",
        "state_code" => "state code",
        "country_code" => "country code",
        other => other,
    }
}

fn document_row(d: &DocumentSummary, row_id: &str, framed: bool) -> Result<FlexRow> {
    let mut row = FlexRow::new(d.id.to_string());
    row.dom_id = Some(row_id.into());
    let kind = match d.status.as_str() {
        "valid" => BadgeKind::Success,
        "submitted" | "consolidated" | "issued" | "released" => BadgeKind::Plain,
        "cancelled" | "rejected" => BadgeKind::Warning,
        _ => BadgeKind::Danger,
    };
    Ok(row
        .slot(
            "number",
            render(RecordLink {
                workspace: framed,
                kind: "finance/einvoice",
                record_id: &d.id.to_string(),
                href: &format!("/finance/einvoice/documents/{}", d.id),
                body: Html::text(&d.code_number),
                class: "font-medium text-ink-strong hover:underline",
            })?,
        )
        .slot("status", render(Badge::status(kind, &d.status))?)
        .slot(
            "payable",
            Html::text(&format!("{} {}", d.payable_amount, d.currency_code)),
        ))
}

fn facts(id: &str, items: Vec<(&str, Html)>) -> Result<Html> {
    render(Facts {
        id,
        items: items
            .into_iter()
            .map(|(title, value)| Fact {
                title,
                value,
                ..Fact::default()
            })
            .collect(),
    })
}

fn section(id: &str, title: &str, description: &str, flat: bool, content: Html) -> Result<Html> {
    let heading = format!("{id}-heading");
    let mut body = render(SectionHeading {
        id: &heading,
        title,
        description,
        class: if flat { "p-5 pb-0" } else { "" },
        ..SectionHeading::default()
    })?;
    body.push(&content);
    render(Card {
        id,
        role: "region",
        labelledby: &heading,
        inner_class: if flat { "p-0" } else { "p-5 sm:p-6" },
        body,
        ..Card::default()
    })
}

fn unavailable(operation: &str, title: &str, reason: &str) -> Result<Html> {
    render(EmptyState::missing(title, reason).id(format!("einvoice-{operation}-unavailable")))
}

fn instant(id: &str, at: Timestamp, display: &DisplayContext) -> Result<Html> {
    let value = display.render(at, Format::DateTime, Precision::Minute);
    render(Instant {
        instant_id: id,
        value: bilibi_ui::flex_table::Instant::new(
            &value.iso,
            &value.text,
            &value.company,
            &value.utc,
            value.mode.as_str(),
        ),
    })
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// This company's e-invoice settings in System settings.
fn settings_href(scope: &Scope) -> String {
    match scope.current_company() {
        Some(id) => format!(
            "/system/settings?company={}&module=finance/einvoice",
            id.get()
        ),
        None => "/system/settings?module=finance/einvoice".into(),
    }
}

/// A ledger entry, linked to its journal.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="text-link hover:underline">Open journal</a>"#,
    ext = "html"
)]
struct EntryById {
    id: i64,
}

/// Where the posting accounts are chosen, above the documents they hold back.
#[derive(Template)]
#[template(
    source = r#"<p id="einvoice-unposted-settings" class="px-5 pb-3 text-sm text-ink-muted">A posting refused for its accounts posts once they are chosen in the <a href="{{ href }}" class="text-link hover:underline">e-invoice settings</a>; then retry it here.</p>"#,
    ext = "html"
)]
struct SettingsNote<'a> {
    href: &'a str,
}

#[derive(Template)]
#[template(path = "page.html")]
struct PageView<'a> {
    page_id: &'a str,
    variant: PageVariant,
    flash: FlashMap<'a>,
    parts: Vec<Html>,
}
#[derive(Template)]
#[template(path = "consolidation_forms.html")]
struct ConsolidationForm {
    issue: bool,
    month_control_revision: u64,
    field: Field,
}
#[derive(Template)]
#[template(path = "instant.html")]
struct Instant<'a> {
    instant_id: &'a str,
    value: bilibi_ui::flex_table::Instant,
}
#[derive(Template)]
#[template(path = "supplier_complete.html")]
struct SupplierComplete {}
#[derive(Template)]
#[template(
    source = "{% import \"components.html\" as ui %}<span id=\"{{ window_id }}\" class=\"ml-2\">{% call ui::badge(kind=kind) %}{{ label }}{% endcall %}</span>",
    ext = "html"
)]
struct WindowBadge<'a> {
    window_id: &'a str,
    label: &'a str,
    kind: BadgeKind,
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "consolidation" => Some("E-invoice consolidation"),
            "reconciliation" => Some("E-invoice reconciliation"),
            "setup" => Some("E-invoice setup"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = State::new(self.clone(), &request)?;
            state.load(scope).await?;
            Ok(PageBody {
                html: state.render(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let mut state = State::new(self.clone(), &request)?;
            state
                .load(request.scope.ok_or(Error::Unauthorized)?)
                .await?;
            Ok(Some(Box::new(state) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}

impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Event(value) => self.event(request.scope, value).await?,
                LiveInput::Params(query) => {
                    let next = month(query.get("month").map(String::as_str));
                    if next != self.month {
                        // The pinned patcher preserves a focused input's dirty value.
                        // Replace only this control on Back/Forward, not while editing.
                        self.month_control_revision += 1;
                    }
                    self.month = next;
                    self.load(request.scope).await?;
                }
                LiveInput::Refresh => self.load(request.scope).await?,
            }
            let mut query = HashMap::new();
            if self.operation == "consolidation" {
                query.insert("month".into(), self.month.strftime("%Y-%m").to_string());
            }
            let redirect = self.redirect.take();
            let flash = if redirect.is_some() {
                self.flash.take()
            } else {
                None
            };
            Ok(LiveBody {
                html: self.render(request.scope).await?,
                query,
                redirect,
                flash,
                ..LiveBody::default()
            })
        })
    }
}
