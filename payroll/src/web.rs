//! Bilimbi Payroll.Web.{PostingsLive,SetupLive} at 4abfaf829321f8540581d833752a086b21b45ba9.
//! Runs and claims come from Finance-owned copies of People events.
use crate::{
    MappingChange, Payroll, PayrollError, Reversal, SettingsChange, MANAGE_SETTINGS, POST_POSTINGS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_finance_ledger::{Account, AccountType, FieldError, LedgerError, MinorUnits, Money};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    forms::{Choice, Field, InputKind},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, ConfirmDialog,
    EmptyState, Fact, Facts, FlashMap, FlexRow, FlexTable, Header, Html, ListColumns, ListSpec,
    ListState, SectionHeading, StatusKind, TableEmpty, NO_FLASH,
};
use jiff::Timestamp;
use rust_decimal::Decimal;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
pub(crate) struct Pages {
    pub payroll: Arc<Payroll>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

struct State {
    pages: Pages,
    setup: bool,
    list: ListState,
    columns: Vec<ListColumns>,
    settings: Option<HashMap<String, String>>,
    mapping: HashMap<String, String>,
    errors: Vec<FieldError>,
    error_form: &'static str,
    reversing: Option<i64>,
    reversal: HashMap<String, String>,
    pending: Option<(i64, Reversal)>,
    flash: Option<(StatusKind, String)>,
}

impl State {
    fn new(pages: Pages, operation: &str, query: &HashMap<String, String>) -> Result<Self> {
        let setup = match operation {
            "setup" => true,
            "postings" => false,
            _ => return Err(Error::NotFound),
        };
        let tables: &[(&str, &[(&str, &str)])] = if setup {
            &[
                (
                    "payroll-mappings-table",
                    &[
                        ("kind", "Kind"),
                        ("code", "Code"),
                        ("direction", "Direction"),
                        ("expense", "Expense account"),
                        ("liability", "Liability account"),
                    ],
                ),
                (
                    "payroll-settings-changes-table",
                    &[
                        ("when", "When"),
                        ("by", "By"),
                        ("net", "Net pay"),
                        ("claims", "Claims payable"),
                        ("reason", "Reason"),
                    ],
                ),
                (
                    "payroll-mapping-changes-table",
                    &[
                        ("when", "When"),
                        ("by", "By"),
                        ("mapping", "Mapping"),
                        ("expense", "Expense account"),
                        ("liability", "Liability account"),
                        ("reason", "Reason"),
                    ],
                ),
            ]
        } else {
            &[
                (
                    "payroll-runs-table",
                    &[
                        ("reference", "Run"),
                        ("date", "Pay date"),
                        ("status", "Run status"),
                        ("standing", "In the books"),
                        ("failure", "Last failure"),
                    ],
                ),
                (
                    "payroll-claims-table",
                    &[
                        ("reference", "Claim"),
                        ("category", "Category"),
                        ("date", "Approved on"),
                        ("amount", "Amount"),
                        ("standing", "In the books"),
                        ("failure", "Last failure"),
                    ],
                ),
                (
                    "payroll-postings-table",
                    &[
                        ("source", "Source"),
                        ("entry", "Entry"),
                        ("date", "Entry date"),
                        ("total", "Total"),
                        ("standing", "Status"),
                        ("reason", "Reversal reason"),
                    ],
                ),
            ]
        };
        let columns = tables
            .iter()
            .map(|(id, fields)| {
                ListColumns::mount(
                    id,
                    fields
                        .iter()
                        .map(|(key, label)| {
                            let col = BuiltinColumn::new(key, label);
                            if matches!(*key, "total" | "amount") {
                                col.right()
                            } else {
                                col
                            }
                        })
                        .collect(),
                )
                .map_err(|_| Error::Internal)
            })
            .collect::<Result<Vec<_>>>()?;
        // No sort, filter or pager in Bilimbi Payroll. ListState validates the
        // address without allowing a forged query to hide operational rows.
        let spec = ListSpec {
            omit_defaults: true,
            ..ListSpec::default()
        }
        .shared()
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            setup,
            columns,
            list: ListState::parse(query, spec).map_err(|_| Error::Internal)?,
            settings: None,
            mapping: default_mapping(),
            errors: Vec::new(),
            error_form: "",
            reversing: None,
            reversal: HashMap::new(),
            pending: None,
            flash: None,
        })
    }

    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.pages.payroll.db.get().await?;
        match crate::authorize(&**client, scope, company(scope)?, capability).await {
            Ok(()) => Ok(true),
            Err(PayrollError::Unauthorized) => Ok(false),
            Err(_) => Ok(false),
        }
    }

    fn refusal(&mut self, form: &'static str, error: PayrollError) {
        let message = match &error {
            PayrollError::Invalid(fields) | PayrollError::Ledger(LedgerError::Invalid(fields)) => {
                self.errors = fields.clone();
                self.error_form = form;
                format!(
                    "Refused: {}.",
                    fields
                        .iter()
                        .map(|e| format!("{} {}", e.field.replace('_', " "), e.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            }
            PayrollError::Failed(_) => "It could not be done. No change was confirmed.".into(),
            _ => error.to_string(),
        };
        self.flash = Some((StatusKind::Error, message));
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let event = value["type"].as_str().unwrap_or_default();
        if let Some(index) = event
            .strip_prefix("grid_")
            .and_then(|i| i.parse::<usize>().ok())
        {
            if let Some(columns) = self.columns.get_mut(index) {
                columns.handle(&params::string_values(value));
            }
            return Ok(());
        }
        if event == "clear-flash" {
            if self
                .flash
                .as_ref()
                .is_some_and(|(kind, _)| value["key"].as_str() == Some(kind.name()))
            {
                self.flash = None;
            }
            return Ok(());
        }
        let settings_event = matches!(event, "save_settings" | "save_mapping" | "edit_mapping");
        let posting_event = matches!(
            event,
            "post_run"
                | "post_claim"
                | "post_all_claims"
                | "reverse"
                | "confirm_reversal"
                | "commit_reversal"
        );
        if (settings_event && !self.setup) || (posting_event && self.setup) {
            return Ok(());
        }
        if (settings_event && !self.allowed(scope, MANAGE_SETTINGS).await?)
            || (posting_event && !self.allowed(scope, POST_POSTINGS).await?)
        {
            self.pending = None;
            self.refusal("", PayrollError::Unauthorized);
            return Ok(());
        }
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        match event {
            "change_settings" | "save_settings" => {
                let form = match read_form(
                    value,
                    "settings",
                    &["net_pay_account_id", "claims_payable_account_id", "reason"],
                ) {
                    Ok(form) => form,
                    Err(error) => {
                        self.refusal("settings", error);
                        return Ok(());
                    }
                };
                self.settings = Some(form.clone());
                if event == "save_settings" {
                    self.errors.clear();
                    let result = match (
                        account_id(&form, "net_pay_account_id", true),
                        account_id(&form, "claims_payable_account_id", true),
                    ) {
                        (Ok(net), Ok(claims)) => {
                            payroll
                                .put_settings(
                                    scope,
                                    company,
                                    &SettingsChange {
                                        net_pay_account_id: net,
                                        claims_payable_account_id: claims,
                                        reason: Some(get(&form, "reason").into()),
                                    },
                                )
                                .await
                        }
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    };
                    match result {
                        Ok(_) => {
                            self.settings = None;
                            self.flash =
                                Some((StatusKind::Success, "Posting accounts saved.".into()));
                        }
                        Err(e) => self.refusal("settings", e),
                    }
                }
            }
            "change_mapping" | "save_mapping" => {
                self.mapping = match read_form(
                    value,
                    "mapping",
                    &[
                        "kind",
                        "code",
                        "direction",
                        "expense_account_id",
                        "liability_account_id",
                        "reason",
                    ],
                ) {
                    Ok(form) => form,
                    Err(error) => {
                        self.refusal("mapping", error);
                        return Ok(());
                    }
                };
                if event == "save_mapping" {
                    self.errors.clear();
                    let direction = if get(&self.mapping, "kind") == "claim_category" {
                        "claim"
                    } else {
                        get(&self.mapping, "direction")
                    };
                    let result = match (
                        account_id(&self.mapping, "expense_account_id", false),
                        account_id(&self.mapping, "liability_account_id", false),
                    ) {
                        (Ok(expense), Ok(liability)) => {
                            payroll
                                .put_mapping(
                                    scope,
                                    company,
                                    &MappingChange {
                                        kind: get(&self.mapping, "kind").into(),
                                        code: get(&self.mapping, "code").into(),
                                        direction: direction.into(),
                                        expense_account_id: expense,
                                        liability_account_id: liability,
                                        reason: Some(get(&self.mapping, "reason").into()),
                                    },
                                )
                                .await
                        }
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    };
                    match result {
                        Ok(m) => {
                            self.flash = Some((
                                StatusKind::Success,
                                format!("Mapping {} ({}) saved.", m.code, m.direction),
                            ));
                            self.mapping = default_mapping();
                        }
                        Err(e) => self.refusal("mapping", e),
                    }
                }
            }
            "edit_mapping" => {
                let id = params::positive_id(value["id"].as_str());
                match payroll.list_mappings(scope, company).await {
                    Ok(mappings) => {
                        if let Some(m) = mappings.iter().find(|m| Some(m.id) == id) {
                            self.mapping = HashMap::from([
                                ("kind".into(), m.kind.clone()),
                                ("code".into(), m.code.clone()),
                                ("direction".into(), m.direction.clone()),
                                (
                                    "expense_account_id".into(),
                                    m.expense_account_id
                                        .map(|id| id.to_string())
                                        .unwrap_or_default(),
                                ),
                                (
                                    "liability_account_id".into(),
                                    m.liability_account_id
                                        .map(|id| id.to_string())
                                        .unwrap_or_default(),
                                ),
                                ("reason".into(), String::new()),
                            ]);
                            self.errors.clear();
                        }
                    }
                    Err(e) => self.refusal("mapping", e),
                }
            }
            "reverse" => {
                self.reversing = params::positive_id(value["id"].as_str());
                self.reversal.clear();
                self.errors.clear();
                self.pending = None;
            }
            "cancel_reversal" => {
                self.reversing = None;
                self.pending = None;
            }
            "cancel_confirmation" => self.pending = None,
            "change_reversal" | "confirm_reversal" => {
                self.pending = None;
                self.reversal = match read_form(value, "reversal", &["reason", "entry_date"]) {
                    Ok(form) => form,
                    Err(error) => {
                        self.refusal("reversal", error);
                        return Ok(());
                    }
                };
                if event == "confirm_reversal" {
                    self.errors.clear();
                    match reversal(&self.reversal) {
                        Ok(reversal) => {
                            if let Some(id) = self.reversing {
                                self.pending = Some((id, reversal));
                            }
                        }
                        Err(e) => self.refusal("reversal", e),
                    }
                }
            }
            "commit_reversal" => {
                if let Some((id, reversal)) = self.pending.take() {
                    match payroll.reverse_posting(scope, company, id, &reversal).await {
                        Ok(p) => {
                            self.reversing = None;
                            self.flash = Some((
                                StatusKind::Success,
                                format!(
                                    "Reversed as entry {}.",
                                    p.reversal.expect("committed reversal").entry_number
                                ),
                            ));
                        }
                        Err(e) => self.refusal("reversal", e),
                    }
                }
            }
            "post_run" | "post_claim" | "post_all_claims" => {
                // Resolve the source and standing again for every write.
                let id = params::positive_id(value["id"].as_str()).unwrap_or(0);
                if event == "post_all_claims" {
                    match payroll.list_claims(scope, company).await {
                        Ok(claims) => {
                            let ids = claims
                                .iter()
                                .filter(|c| c["posting"].is_null())
                                .filter_map(|c| c["id"].as_i64())
                                .collect::<Vec<_>>();
                            match payroll.post_claims(scope, company, &ids).await {
                                Ok(outcomes) => {
                                    let posted = outcomes.iter().filter(|(_, r)| r.is_ok()).count();
                                    let failed = outcomes.len() - posted;
                                    self.flash = Some((
                                        if failed == 0 {
                                            StatusKind::Success
                                        } else {
                                            StatusKind::Error
                                        },
                                        format!("{posted} claims posted, {failed} failed."),
                                    ));
                                }
                                Err(e) => self.refusal("", e),
                            }
                        }
                        Err(e) => self.refusal("", e),
                    }
                } else {
                    let result = if event == "post_run" {
                        payroll.post_payroll_run(scope, company, id).await
                    } else {
                        payroll.post_claim(scope, company, id).await
                    };
                    match result {
                        Ok(p) => {
                            self.flash = Some((
                                StatusKind::Success,
                                format!(
                                    "{} posted as entry {}.",
                                    if event == "post_run" {
                                        "Payroll run"
                                    } else {
                                        "Claim"
                                    },
                                    p.entry_number
                                ),
                            ))
                        }
                        Err(e) => self.refusal("", e),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let mut parts = Vec::new();
        let (title, subtitle, link_id, link, label) = if self.setup {
            (
                "Payroll posting accounts",
                "Where net pay and approved claims are owed, and which accounts each payroll component and claim category posts to.",
                "payroll-setup-postings",
                "/finance/payroll",
                "Payroll and claims",
            )
        } else {
            (
                "Payroll and claims",
                "Finalised payroll runs and approved claims, and whether each is in the books.",
                "payroll-postings-setup",
                "/finance/payroll/setup",
                "Posting accounts",
            )
        };
        let title_actions = Html::EMPTY;
        parts.push(render(Header {
            title: Html::text(title),
            title_actions,
            subtitle,
            actions: render(HeaderLink {
                id: link_id,
                href: link,
                label,
            })?,
        })?);
        if self.setup {
            parts.extend(self.setup_parts(scope).await?);
        } else {
            parts.extend(self.postings_parts(scope).await?);
        }
        let mut flash = NO_FLASH;
        if let Some((kind, text)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = text,
                StatusKind::Error => flash.error = text,
                StatusKind::Info => flash.info = text,
                StatusKind::Warning => flash.warning = text,
            }
        }
        PageView {
            id: if self.setup {
                "payroll-setup"
            } else {
                "payroll-postings"
            },
            flash,
            parts,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    async fn postings_parts(&mut self, scope: &Scope) -> Result<Vec<Html>> {
        let mut parts = Vec::new();
        if self.pages.payroll.people_available() {
            parts.extend(self.source_parts(scope).await?);
        } else {
            parts.push(render(EmptyState::missing("Payroll is not in use", "Payroll runs and claims appear here once payroll is in use. The postings already made are listed below.").id("payroll-source-unavailable"))?);
        }
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        let can_post = self.allowed(scope, POST_POSTINGS).await?;
        if self.reversing.is_some() && can_post {
            let reason = self
                .field(
                    "reversal",
                    "reason",
                    "payroll-reversal-reason",
                    "Reason",
                    &self.reversal,
                )
                .required()
                .attr("maxlength", "500");
            let date = self
                .field(
                    "reversal",
                    "entry_date",
                    "payroll-reversal-date",
                    "Entry date (company's today unless given)",
                    &self.reversal,
                )
                .kind(InputKind::Date);
            let form = render(FormView {
                id: "payroll-reversal-form",
                change: "change_reversal",
                submit: "confirm_reversal",
                fields: vec![reason, date],
                button_id: "payroll-reversal-confirm",
                label: "Reverse posting",
                variant: ButtonVariant::Danger,
                cancel: true,
            })?;
            parts.push(section("payroll-reversal", "payroll-reversal-heading", "Reverse a posting", "The reversal is a new entry with the sides swapped; the original stays in the books. Say why.", None, form)?);
        }
        let postings = payroll.list_postings(scope, company).await;
        let body = match postings {
            Err(_) => render(
                EmptyState::forbidden("view payroll postings").id("payroll-postings-unavailable"),
            )?,
            Ok(postings) => {
                let places = payroll
                    .ledger
                    .get_book(scope, company)
                    .await
                    .map(|b| b.minor_units.get())
                    .unwrap_or(2);
                let mut rows = Vec::new();
                for posting in postings {
                    let source = if posting.source_kind == "payroll_run" {
                        "Payroll run"
                    } else {
                        "Claim"
                    };
                    let standing = posting.reversal.as_ref().map_or("Posted".into(), |r| {
                        format!("Reversed by entry {}", r.entry_number)
                    });
                    let mut row = row(
                        &format!("payroll-posting-{}", posting.id),
                        &[
                            ("source", format!("{source} {}", posting.source_reference)),
                            ("entry", posting.entry_number.to_string()),
                            ("date", posting.entry_date),
                            (
                                "total",
                                format!(
                                    "{} {}",
                                    posting.total.parse::<Decimal>().map_or_else(
                                        |_| "—".into(),
                                        |amount| {
                                            Money::display_decimal(
                                                amount,
                                                MinorUnits::new(i64::from(places)),
                                            )
                                        },
                                    ),
                                    posting.currency_code
                                ),
                            ),
                            (
                                "reason",
                                posting
                                    .reversal
                                    .as_ref()
                                    .map_or("—".into(), |r| r.reason.clone()),
                            ),
                        ],
                    );
                    row = row.slot(
                        "standing",
                        render(Badge::status(
                            if posting.reversal.is_some() {
                                BadgeKind::Warning
                            } else {
                                BadgeKind::Success
                            },
                            &standing,
                        ))?,
                    );
                    if can_post && posting.reversal.is_none() {
                        row.actions = action(
                            &format!("payroll-reverse-posting-{}", posting.id),
                            "reverse",
                            posting.id,
                            "Reverse",
                        )?;
                    }
                    rows.push(row);
                }
                self.table(
                    2,
                    rows,
                    "Postings made",
                    "Nothing has been posted yet.",
                    true,
                )?
            }
        };
        parts.push(section(
            "payroll-postings-list",
            "payroll-postings-heading",
            "Postings made",
            "",
            None,
            body,
        )?);
        if self.pending.is_some() {
            parts.push(render(ConfirmDialog { id: "payroll-reversal-dialog", consequence: "Reverse a posting", detail: "The reversal is a new entry with the sides swapped; the original stays in the books.", confirm: "Reverse posting", working: "Reversing…", confirm_event: "commit_reversal", cancel_event: "cancel_confirmation" })?);
        }
        Ok(parts)
    }

    async fn source_parts(&mut self, scope: &Scope) -> Result<Vec<Html>> {
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        let can_post = self.allowed(scope, POST_POSTINGS).await?;
        let units = payroll
            .ledger
            .get_book(scope, company)
            .await
            .map(|b| Some(b.minor_units))
            .unwrap_or(MinorUnits::new(2));
        let mut parts = Vec::new();
        for (index, claims) in [false, true].into_iter().enumerate() {
            let (kind, title, empty) = if claims {
                ("claim", "Approved claims", "No approved claims.")
            } else {
                ("run", "Payroll runs", "No payroll runs yet.")
            };
            let result = if claims {
                payroll.list_claims(scope, company).await
            } else {
                payroll.list_payroll_runs(scope, company).await
            };
            let mut content = Html::EMPTY;
            let mut actions = Html::EMPTY;
            match result {
                Err(_) => content.push(&render(
                    EmptyState::forbidden(if claims {
                        "read the company's approved claims"
                    } else {
                        "read the company's payroll runs"
                    })
                    .id(format!("payroll-{kind}s-unavailable")),
                )?),
                Ok(sources) => {
                    if claims && can_post && !sources.is_empty() {
                        actions = action(
                            "payroll-post-all-claims",
                            "post_all_claims",
                            0,
                            "Post every unposted claim",
                        )?;
                    }
                    let mut rows = Vec::new();
                    for source in sources {
                        let id = source["id"].as_i64().ok_or(Error::Internal)?;
                        let posting = &source["posting"];
                        let reversed = !posting["reversal"].is_null();
                        let posted = !posting.is_null();
                        let failure = source["last_failure"]["reason"].as_str();
                        let standing = if posted {
                            format!(
                                "{}ntry {}{}",
                                if reversed { "Reversed (e" } else { "E" },
                                posting["entry_number"],
                                if reversed { ")" } else { "" }
                            )
                        } else if failure.is_some() {
                            "Failed".into()
                        } else {
                            "Not posted".into()
                        };
                        let mut cells = vec![
                            (
                                "reference",
                                source["reference"].as_str().unwrap_or("—").to_owned(),
                            ),
                            (
                                "date",
                                source[if claims { "approved_on" } else { "pay_date" }]
                                    .as_str()
                                    .unwrap_or(if claims {
                                        "unknown"
                                    } else {
                                        "missing or invalid"
                                    })
                                    .to_owned(),
                            ),
                            ("failure", failure.unwrap_or("—").to_owned()),
                        ];
                        if claims {
                            let amount = source["amount"]
                                .as_str()
                                .and_then(|v| v.parse::<Decimal>().ok())
                                .map(|v| Money::display_decimal(v, units))
                                .unwrap_or_else(|| "—".into());
                            cells.push((
                                "amount",
                                format!(
                                    "{amount} {}",
                                    source["currency_code"].as_str().unwrap_or("")
                                ),
                            ));
                            cells.push((
                                "category",
                                source["category_code"].as_str().unwrap_or("—").to_owned(),
                            ));
                        } else {
                            cells.push(("status", "approved".into()));
                        }
                        let mut row = row(&format!("payroll-{kind}-{id}"), &cells).slot(
                            "standing",
                            render(Badge::status(
                                if reversed {
                                    BadgeKind::Warning
                                } else if posted {
                                    BadgeKind::Success
                                } else if failure.is_some() {
                                    BadgeKind::Danger
                                } else {
                                    BadgeKind::Neutral
                                },
                                &standing,
                            ))?,
                        );
                        if can_post && !posted {
                            row.actions.push(&action(
                                &format!("payroll-post-{kind}-{id}"),
                                if claims { "post_claim" } else { "post_run" },
                                id,
                                if failure.is_some() {
                                    "Retry posting"
                                } else {
                                    "Post"
                                },
                            )?);
                        } else if can_post && posted && !reversed {
                            row.actions.push(&action(
                                &format!("payroll-reverse-{kind}-{id}"),
                                "reverse",
                                posting["id"].as_i64().ok_or(Error::Internal)?,
                                "Reverse",
                            )?);
                        }
                        rows.push(row);
                    }
                    content.push(&self.table(index, rows, title, empty, true)?);
                }
            }
            let section_id = if claims {
                "payroll-claims"
            } else {
                "payroll-runs"
            };
            let heading_id = if claims {
                "payroll-claims-heading"
            } else {
                "payroll-runs-heading"
            };
            let mut body = render(SectionHeading {
                id: heading_id,
                title,
                class: "p-5 pb-0",
                actions,
                ..SectionHeading::default()
            })?;
            body.push(&content);
            parts.push(render(Card {
                id: section_id,
                class: "mt-6",
                inner_class: "p-0",
                role: "region",
                labelledby: heading_id,
                body,
                ..Card::default()
            })?);
        }
        Ok(parts)
    }

    async fn setup_parts(&mut self, scope: &Scope) -> Result<Vec<Html>> {
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        let settings = match payroll.get_settings(scope, company).await {
            Ok(s) => Some(s),
            Err(PayrollError::PayrollNotConfigured) => None,
            Err(_) => {
                return Ok(vec![render(
                    EmptyState::forbidden("view payroll posting accounts")
                        .id("payroll-setup-forbidden"),
                )?]);
            }
        };
        // Payroll-only readers still see their own records if Ledger is withheld.
        let accounts = payroll
            .ledger
            .list_accounts(scope, company)
            .await
            .unwrap_or_default();
        let can_manage = self.allowed(scope, MANAGE_SETTINGS).await?;
        let liability = account_choices(&accounts, true);
        let expense = account_choices(&accounts, false);
        let mut current = Html::default();
        if let Some(s) = &settings {
            current.push(&render(Facts {
                id: "payroll-settings-current",
                items: vec![
                    Fact {
                        title: "Net pay",
                        id: "payroll-settings-net-pay",
                        value: Html::text(&account_label(&accounts, Some(s.net_pay_account_id))),
                    },
                    Fact {
                        title: "Claims payable",
                        id: "payroll-settings-claims-payable",
                        value: Html::text(&account_label(
                            &accounts,
                            Some(s.claims_payable_account_id),
                        )),
                    },
                ],
            })?);
        }
        if can_manage {
            let values = self.settings.clone().unwrap_or_else(|| {
                HashMap::from([
                    (
                        "net_pay_account_id".into(),
                        settings
                            .as_ref()
                            .map(|s| s.net_pay_account_id.to_string())
                            .unwrap_or_default(),
                    ),
                    (
                        "claims_payable_account_id".into(),
                        settings
                            .as_ref()
                            .map(|s| s.claims_payable_account_id.to_string())
                            .unwrap_or_default(),
                    ),
                ])
            });
            current.push(&render(FormView {
                id: "payroll-settings-form",
                change: "change_settings",
                submit: "save_settings",
                fields: vec![
                    self.field(
                        "settings",
                        "net_pay_account_id",
                        "payroll-settings-net-pay-account",
                        "Net pay account (liability)",
                        &values,
                    )
                    .kind(InputKind::Select)
                    .options(liability.clone())
                    .prompt("Choose an account")
                    .required(),
                    self.field(
                        "settings",
                        "claims_payable_account_id",
                        "payroll-settings-claims-payable-account",
                        "Claims payable account (liability)",
                        &values,
                    )
                    .kind(InputKind::Select)
                    .options(liability.clone())
                    .prompt("Choose an account")
                    .required(),
                    self.field(
                        "settings",
                        "reason",
                        "payroll-settings-reason",
                        "Reason for the change",
                        &values,
                    )
                    .attr("maxlength", "500"),
                ],
                button_id: "payroll-settings-save",
                label: "Save posting accounts",
                variant: ButtonVariant::Primary,
                cancel: false,
            })?);
        } else {
            current.push(&render(
                EmptyState::forbidden("change payroll posting accounts")
                    .id("payroll-settings-manage-forbidden"),
            )?);
        }
        let mut parts = vec![section(
            "payroll-settings",
            "payroll-settings-heading",
            "Owed to staff",
            "Liability accounts: a run's net pay is credited to the first, an approved claim to the second, until they are paid.",
            None,
            current,
        )?];
        let mappings = payroll
            .list_mappings(scope, company)
            .await
            .unwrap_or_default();
        let mut rows = Vec::new();
        for m in &mappings {
            let mut row = row(
                &format!("payroll-mapping-{}", m.id),
                &[
                    (
                        "kind",
                        if m.kind == "component" {
                            "Payroll component"
                        } else {
                            "Claim category"
                        }
                        .into(),
                    ),
                    ("code", m.code.clone()),
                    ("direction", m.direction.clone()),
                    ("expense", account_label(&accounts, m.expense_account_id)),
                    (
                        "liability",
                        account_label(&accounts, m.liability_account_id),
                    ),
                ],
            );
            if can_manage {
                row.actions = action(
                    &format!("payroll-mapping-edit-{}", m.id),
                    "edit_mapping",
                    m.id,
                    "Change",
                )?;
            }
            rows.push(row);
        }
        let table = self.table(
            0,
            rows,
            "Account mappings",
            "No mappings yet: a run or claim cannot post until its components are mapped.",
            true,
        )?;
        parts.push(section("payroll-mappings", "payroll-mappings-heading", "Account mappings", "A payroll component is a pay item code with the direction of its lines; a claim category is its code.", Some(mappings.len()), table)?);
        if can_manage {
            let mut fields = vec![
                self.field(
                    "mapping",
                    "kind",
                    "payroll-mapping-kind",
                    "Kind",
                    &self.mapping,
                )
                .kind(InputKind::Select)
                .options(choices(&[
                    ("Payroll component", "component"),
                    ("Claim category", "claim_category"),
                ])),
                self.field(
                    "mapping",
                    "code",
                    "payroll-mapping-code",
                    "Code (as the source names it)",
                    &self.mapping,
                )
                .required()
                .attr("maxlength", "60"),
            ];
            if get(&self.mapping, "kind") != "claim_category" {
                fields.push(
                    self.field(
                        "mapping",
                        "direction",
                        "payroll-mapping-direction",
                        "Direction",
                        &self.mapping,
                    )
                    .kind(InputKind::Select)
                    .options(choices(&[
                        ("earning", "Earning"),
                        ("deduction", "Deduction"),
                        ("employer", "Employer"),
                    ])),
                );
            }
            fields.extend([
                self.field(
                    "mapping",
                    "expense_account_id",
                    "payroll-mapping-expense-account",
                    "Expense account (asset or expense)",
                    &self.mapping,
                )
                .kind(InputKind::Select)
                .options(expense)
                .prompt("None"),
                self.field(
                    "mapping",
                    "liability_account_id",
                    "payroll-mapping-liability-account",
                    "Liability account",
                    &self.mapping,
                )
                .kind(InputKind::Select)
                .options(liability)
                .prompt("None"),
                self.field(
                    "mapping",
                    "reason",
                    "payroll-mapping-reason",
                    "Reason for the change",
                    &self.mapping,
                )
                .attr("maxlength", "500"),
            ]);
            let form = render(FormView {
                id: "payroll-mapping-form",
                change: "change_mapping",
                submit: "save_mapping",
                fields,
                button_id: "payroll-mapping-save",
                label: "Save mapping",
                variant: ButtonVariant::Primary,
                cancel: false,
            })?;
            parts.push(section("payroll-mapping-editor", "payroll-mapping-form-heading", "Add or change a mapping", "An earning or a claim category names the expense account; a deduction names the liability account; an employer contribution names both.", None, form)?);
        }
        let display = {
            let client = payroll.db.get().await?;
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
        let mut history = Html::default();
        for (index, dataset, caption, empty, prefix) in [
            (
                1,
                "settings_change",
                "Posting account changes",
                "The posting accounts have not been set.",
                "payroll-settings-change",
            ),
            (
                2,
                "mapping_change",
                "Mapping changes",
                "No mapping has been changed.",
                "payroll-mapping-change",
            ),
        ] {
            let changes = if dataset == "settings_change" {
                payroll.list_settings_changes(scope, company).await
            } else {
                payroll.list_mapping_changes(scope, company).await
            }
            .unwrap_or_default();
            let rows = changes
                .iter()
                .map(|c| history_row(c, prefix, &accounts, &display, index == 1))
                .collect::<Vec<_>>();
            history.push(&self.table(index, rows, caption, empty, false)?);
        }
        parts.push(section(
            "payroll-changes",
            "payroll-changes-heading",
            "Change history",
            "",
            None,
            history,
        )?);
        Ok(parts)
    }

    fn field(
        &self,
        form: &str,
        key: &str,
        id: &str,
        label: &str,
        values: &HashMap<String, String>,
    ) -> Field {
        let field = Field::in_form(form, key)
            .id(id)
            .label(label)
            .value(get(values, key))
            .errors(if self.error_form == form {
                self.errors
                    .iter()
                    .filter(|e| e.field == key)
                    .map(|e| e.message.clone())
                    .collect()
            } else {
                vec![]
            });
        if key == "reason" && form != "reversal" {
            field.wrapper_class("md:col-span-2 mb-4")
        } else {
            field
        }
    }
    fn table(
        &mut self,
        index: usize,
        rows: Vec<FlexRow>,
        caption: &str,
        empty: &str,
        actions: bool,
    ) -> Result<Html> {
        let columns = &self.columns[index];
        let event = format!("grid_{index}");
        let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
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
fn get<'a>(values: &'a HashMap<String, String>, key: &str) -> &'a str {
    values.get(key).map(String::as_str).unwrap_or_default()
}
fn default_mapping() -> HashMap<String, String> {
    HashMap::from([
        ("kind".into(), "component".into()),
        ("direction".into(), "earning".into()),
    ])
}

/// Generic form_values drops strings over 1 KiB. A valid 500-character
/// Unicode reason can occupy 2 KiB: preserve known fields for facade validation.
fn read_form(
    value: &Value,
    form: &str,
    fields: &[&'static str],
) -> crate::Result<HashMap<String, String>> {
    fields
        .iter()
        .map(|key| {
            let text = match value.get(format!("{form}[{key}]")) {
                None => "",
                Some(value) => value.as_str().ok_or_else(|| invalid(key, "is invalid"))?,
            };
            // Never turn malformed values or a truncated reason into a
            // different, valid input before facade validation.
            Ok(((*key).into(), text.to_owned()))
        })
        .collect()
}
fn invalid(field: &'static str, message: &str) -> PayrollError {
    PayrollError::Invalid(vec![FieldError::new(field, message)])
}
fn account_id(
    values: &HashMap<String, String>,
    key: &'static str,
    required: bool,
) -> crate::Result<Option<i64>> {
    let text = get(values, key);
    if text.is_empty() {
        return if required {
            Err(invalid(key, "can't be blank"))
        } else {
            Ok(None)
        };
    }
    params::positive_id(Some(text))
        .map(Some)
        .ok_or_else(|| invalid(key, "is invalid"))
}
fn reversal(values: &HashMap<String, String>) -> crate::Result<Reversal> {
    let reason = crate::reason(Some(get(values, "reason")), true)?.expect("required reason");
    let date = get(values, "entry_date");
    let entry_date = if date.is_empty() {
        None
    } else {
        Some(
            date.parse()
                .map_err(|_| invalid("entry_date", "is invalid"))?,
        )
    };
    Ok(Reversal { reason, entry_date })
}
fn account_label(accounts: &[Account], id: Option<i64>) -> String {
    match id {
        None => "—".into(),
        Some(id) => accounts.iter().find(|a| a.id == id).map_or_else(
            || "Account unavailable".into(),
            |a| format!("{} {}", a.code, a.name),
        ),
    }
}
fn account_choices(accounts: &[Account], liability: bool) -> Vec<Choice> {
    accounts
        .iter()
        .filter(|a| {
            a.postable()
                && if liability {
                    a.account_type == AccountType::Liability
                } else {
                    matches!(a.account_type, AccountType::Asset | AccountType::Expense)
                }
        })
        .map(|a| Choice::new(account_label(accounts, Some(a.id)), a.id.to_string()))
        .collect()
}
fn row(id: &str, cells: &[(&str, String)]) -> FlexRow {
    let mut row = FlexRow::new(id);
    row.dom_id = Some(id.into());
    for (key, text) in cells {
        row = row.slot(key, Html::text(text));
    }
    row
}
fn history_row(
    c: &Value,
    prefix: &str,
    accounts: &[Account],
    display: &DisplayContext,
    settings: bool,
) -> FlexRow {
    let pair = |old: &str, new: &str| {
        format!(
            "{} → {}",
            account_label(accounts, c[old].as_i64()),
            account_label(accounts, c[new].as_i64())
        )
    };
    let mut cells = vec![
        ("by", format!("#{}", c["changed_by_user_id"])),
        ("reason", c["reason"].as_str().unwrap_or("—").into()),
    ];
    if settings {
        cells.extend([
            (
                "net",
                pair("old_net_pay_account_id", "new_net_pay_account_id"),
            ),
            (
                "claims",
                pair(
                    "old_claims_payable_account_id",
                    "new_claims_payable_account_id",
                ),
            ),
        ]);
    } else {
        cells.extend([
            ("mapping", format!("#{}", c["mapping_id"])),
            (
                "expense",
                pair("old_expense_account_id", "new_expense_account_id"),
            ),
            (
                "liability",
                pair("old_liability_account_id", "new_liability_account_id"),
            ),
        ]);
    }
    let id = format!("{prefix}-{}", c["id"]);
    let mut row = row(&id, &cells);
    if let Some(at) = c["changed_at"]
        .as_str()
        .and_then(|s| s.parse::<Timestamp>().ok())
    {
        let r = display.render(at, Format::DateTime, Precision::Minute);
        row = row.cell(
            "when",
            bilibi_ui::flex_table::Cell::instant(bilibi_ui::flex_table::Instant::new(
                &r.iso,
                &r.text,
                &r.company,
                &r.utc,
                r.mode.as_str(),
            )),
        );
    }
    row
}
fn action(id: &str, event: &str, value: i64, label: &str) -> Result<Html> {
    render(Button {
        id,
        event,
        name: "id",
        value: &value.to_string(),
        button_type: "button",
        body: Html::text(label),
        ..Button::default()
    })
}
fn section(
    id: &str,
    heading: &str,
    title: &str,
    description: &str,
    count: Option<usize>,
    content: Html,
) -> Result<Html> {
    let flat = matches!(
        id,
        "payroll-mappings"
            | "payroll-changes"
            | "payroll-postings-list"
            | "payroll-runs"
            | "payroll-claims"
    );
    let mut body = render(SectionHeading {
        id: heading,
        title,
        description,
        count,
        class: if flat { "p-5 pb-0" } else { "" },
        ..SectionHeading::default()
    })?;
    body.push(&content);
    render(Card {
        id,
        class: "mt-6",
        inner_class: if flat { "p-0" } else { "" },
        role: "region",
        labelledby: heading,
        body,
        ..Card::default()
    })
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "page.html")]
struct PageView<'a> {
    id: &'a str,
    flash: FlashMap<'a>,
    parts: Vec<Html>,
}
#[derive(Template)]
#[template(path = "form.html")]
struct FormView<'a> {
    id: &'a str,
    change: &'a str,
    submit: &'a str,
    fields: Vec<Field>,
    button_id: &'a str,
    label: &'a str,
    variant: ButtonVariant,
    cancel: bool,
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "postings" => Some("Payroll and claims"),
            "setup" => Some("Payroll posting accounts"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let mut state = State::new(self.clone(), request.operation, request.query)?;
            Ok(PageBody {
                html: state
                    .render(request.scope.ok_or(Error::Unauthorized)?)
                    .await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(State::new(self.clone(), request.operation, request.query)?);
            Ok(Some(state))
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                self.event(request.scope, value).await?;
            }
            // Bilimbi Payroll has no URL-driven controls; no inherited page or
            // page_size parameters are published from the shared ListState.
            Ok(LiveBody {
                html: self.render(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}

/// Bilimbi's plain header link between Postings and Setup.
#[derive(Template)]
#[template(
    source = r#"<a id="{{ id }}" href="{{ href }}">{{ label }}</a>"#,
    ext = "html"
)]
struct HeaderLink<'a> {
    id: &'a str,
    href: &'a str,
    label: &'a str,
}
