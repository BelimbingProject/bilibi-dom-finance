//! Mirrors Bilimbi `Finance.Reconciliation.Web.MatchingLive`: the statement
//! lines with no standing match beside the ledger entries on the account with
//! none, each line's suggested entry, and a form that matches whatever is
//! ticked on both sides, one or many each. Standing matches are listed and
//! unmatched with a reason. Every write is a facade call that re-asks its
//! capability, and the dialog holds the match it unmatches, never an id the
//! browser sends with the reason.
use super::{
    clear_flash, company, event_type, flash_map, format::told, record_id, render, Follow, Pages,
    DENIED,
};
use crate::{
    BankAccount, LedgerItem, Match, MatchStatus, NewMatch, OpeningItem, PageOptions,
    StatementLineItem, IMPORT_STATEMENTS, MANAGE_MATCHES,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{Format, Precision};
use bilibi_finance_ledger::{EntryLink, LedgerError, MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, params, BackLink, BuiltinColumn, Button, ButtonVariant, Cell, EmptyState, Field,
    FlashMap, FlexRow, FlexTable, Header, Html, IconButton, IconKind, Instant, ListColumns,
    RowMode, StatusKind, TableEmpty,
};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};

const INDEX: &str = "/finance/reconciliation";

/// What the page last read through the facade.
struct Loaded {
    account: BankAccount,
    lines: Vec<StatementLineItem>,
    entries: Vec<LedgerItem>,
    opening_items: Vec<OpeningItem>,
    suggestions: BTreeMap<i64, LedgerItem>,
    /// Standing matches, newest first.
    matches: Vec<Match>,
    units: u8,
    can_match: bool,
    /// Whether any statement was ever imported: with none, an empty lines
    /// list is not "every line matched" but "nothing to match yet".
    has_statements: bool,
    can_import: bool,
}

pub(super) struct Matching {
    pages: Pages,
    id: i64,
    loaded: Option<Loaded>,
    lines_columns: ListColumns,
    entries_columns: ListColumns,
    matches_columns: ListColumns,
    selected_lines: BTreeSet<i64>,
    selected_entries: BTreeSet<i64>,
    note: String,
    /// The match the unmatch dialog is open for: its id and number.
    unmatching: Option<(i64, i64)>,
    reason: String,
    flash: Option<(StatusKind, String)>,
    redirect: Option<String>,
    follow: Follow,
    opening_match: super::opening_match::OpeningMatch,
}

fn columns(id: &str, values: &[(&str, &str)]) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        values
            .iter()
            .map(|(key, label)| match *key {
                // Bilimbi's selection column has no heading.
                "select" => BuiltinColumn {
                    short_label: String::new(),
                    ..BuiltinColumn::new(key, label)
                },
                "amount" => BuiltinColumn::new(key, label).right(),
                _ => BuiltinColumn::new(key, label),
            })
            .collect(),
    )
    .map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "matching.html")]
struct View<'a> {
    header: Html,
    flash: FlashMap<'a>,
    can_match: bool,
    forbidden: Html,
    line_count: Option<usize>,
    entry_count: Option<usize>,
    match_count: Option<usize>,
    lines: Html,
    entries: Html,
    opening_match: Html,
    matches: Html,
    selected_lines: usize,
    selected_entries: usize,
    lines_sum: String,
    entries_sum: String,
    note: Field,
    unmatching: Option<UnmatchView>,
}
struct UnmatchView {
    title: String,
    reason: Field,
}

/// Bilimbi's bare selection checkbox, which the match form reads through
/// `form=`: FlexTable carries forms of its own, which cannot nest in one.
#[derive(Template)]
#[template(
    source = r#"<label class="-mx-1 -my-1 inline-flex size-10 cursor-pointer items-center justify-center pointer-coarse:-my-2"><input type="checkbox" form="matching-form" name="{{ name }}" value="{{ value }}" id="{{ dom_id }}"{% if checked %} checked{% endif %} aria-label="{{ label }}" class="size-4" /></label>"#,
    ext = "html"
)]
struct Select<'a> {
    name: &'a str,
    value: i64,
    dom_id: String,
    checked: bool,
    label: String,
}

#[derive(Template)]
#[template(
    source = r#"{{ text }}{% if let Some(reference) = reference %}<span class="block text-xs text-ink-muted">{{ reference }}</span>{% endif %}"#,
    ext = "html"
)]
struct Described<'a> {
    text: &'a str,
    reference: Option<&'a str>,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% if let Some(entry) = entry %}<span id="line-{{ line }}-suggestion" class="mr-1.5">{{ journal|safe }} · <span class="whitespace-nowrap">{{ date }}</span></span>{% if accept %}{% call ui::button(id=accept_id, button_type="button", event="accept", name="line", value=line.to_string()) %}Accept{% endcall %}{% endif %}{% endif %}"#,
    ext = "html"
)]
struct Suggested<'a> {
    line: i64,
    entry: Option<&'a LedgerItem>,
    /// The entry in its own words, linked to its journal.
    journal: Html,
    date: String,
    accept: bool,
    accept_id: String,
}

/// A ledger entry as its journal's link ("Journal 12"), so a person can open
/// what was posted.
fn journal(entry: Option<&LedgerItem>) -> Result<Html> {
    EntryLink::html(entry.map(|e| e.entry_id), entry.map(|e| e.entry_number))
}

fn described(text: &str, reference: Option<&str>, mode: RowMode) -> Result<Html> {
    render(Described {
        text,
        reference: reference.filter(|_| mode != RowMode::Compact),
    })
}

fn plural(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

/// A posted value in full (a 500-character reason or note can take 2 KiB,
/// past `form_values`' limit); the facade bounds it.
fn text(v: &serde_json::Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_owned()
}

/// The positive ids of a `name[]` list the form posted.
fn ids(v: &serde_json::Value, key: &str) -> BTreeSet<i64> {
    v[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().and_then(|s| s.parse().ok()))
                .filter(|id| *id > 0)
                .collect()
        })
        .unwrap_or_default()
}

impl Matching {
    pub(super) fn new(pages: &Pages, id: i64) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            id,
            loaded: None,
            lines_columns: columns(
                "unmatched-lines-table",
                &[
                    ("select", "Select"),
                    ("date", "Date"),
                    ("description", "Description"),
                    ("amount", "Amount"),
                    ("suggested", "Suggested"),
                ],
            )?,
            entries_columns: columns(
                "unmatched-entries-table",
                &[
                    ("select", "Select"),
                    ("entry", "Entry"),
                    ("date", "Date"),
                    ("description", "Description"),
                    ("amount", "Amount"),
                ],
            )?,
            matches_columns: columns(
                "standing-matches-table",
                &[
                    ("match", "Match"),
                    ("lines", "Statement lines"),
                    ("entries", "Entries"),
                    ("amount", "Amount"),
                    ("note", "Note"),
                    ("matched", "Matched"),
                ],
            )?,
            selected_lines: BTreeSet::new(),
            selected_entries: BTreeSet::new(),
            note: String::new(),
            unmatching: None,
            reason: String::new(),
            flash: None,
            redirect: None,
            follow: Follow::default(),
            opening_match: super::opening_match::OpeningMatch::new(id),
        })
    }
    pub(super) fn follow(mut self, follow: Follow) -> Self {
        self.follow = follow;
        self
    }

    /// Bilimbi `load`: the account and what stands unmatched on it, or a
    /// redirect to the bank accounts with the refusal.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let company = company(scope)?;
        let r = &self.pages.reconciliation;
        let read = match told(r.get_bank_account(scope, company, self.id).await)? {
            Ok(account) => match told(r.unmatched_items(scope, company, self.id, None).await)? {
                Ok(unmatched) => Ok((account, unmatched)),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        let (account, unmatched) = match read {
            Ok(read) => read,
            Err(e) => {
                self.flash = Some((StatusKind::Error, e));
                self.redirect = Some(INDEX.into());
                return Ok(());
            }
        };
        let mut matches = vec![];
        loop {
            let after = matches.last().map(|m: &Match| m.id);
            let page = r
                .list_matches(
                    scope,
                    company,
                    self.id,
                    &PageOptions {
                        limit: Some(500),
                        after,
                    },
                )
                .await;
            match told(page)? {
                Ok(page) => {
                    let done = page.len() < 500;
                    matches.extend(page);
                    if done {
                        break;
                    }
                }
                Err(_) => {
                    matches.clear();
                    break;
                }
            }
        }
        matches.retain(|m| m.status == MatchStatus::Matched);
        matches.reverse();
        let units = match self.pages.ledger.get_book(scope, company).await {
            Ok(book) => book.minor_units.get(),
            Err(LedgerError::Failed(e)) => return Err(e),
            Err(_) => 2,
        };
        let has_statements = !unmatched.lines.is_empty()
            || match told(
                r.list_statements(
                    scope,
                    company,
                    self.id,
                    &PageOptions {
                        limit: Some(1),
                        after: None,
                    },
                )
                .await,
            )? {
                Ok(statements) => !statements.is_empty(),
                // Unreadable here means unknown: keep the matched wording.
                Err(_) => true,
            };
        let lines: BTreeSet<i64> = unmatched.lines.iter().map(|l| l.line.id).collect();
        let entries: BTreeSet<i64> = unmatched.entries.iter().map(|e| e.entry_id).collect();
        self.selected_lines.retain(|id| lines.contains(id));
        self.selected_entries.retain(|id| entries.contains(id));
        self.loaded = Some(Loaded {
            account,
            lines: unmatched.lines,
            entries: unmatched.entries,
            opening_items: unmatched.opening_items,
            suggestions: unmatched.suggestions,
            matches,
            units,
            can_match: self.pages.allowed(scope, MANAGE_MATCHES).await?,
            has_statements,
            can_import: self.pages.allowed(scope, IMPORT_STATEMENTS).await?,
        });
        Ok(())
    }

    /// Bilimbi `match`: the facade, a flash either way, then a fresh read.
    async fn record(&mut self, scope: &Scope, new: NewMatch) -> Result<()> {
        let result = self
            .pages
            .reconciliation
            .record_match(scope, company(scope)?, self.id, &new)
            .await;
        match told(result)? {
            Ok(m) => {
                let lines = m.statement_line_ids.len();
                let entries = m.entries.len();
                self.flash = Some((
                    StatusKind::Success,
                    format!(
                        "Match {} made: {lines} statement {} with {entries} {}.",
                        m.match_number,
                        plural(lines, "line", "lines"),
                        plural(entries, "entry", "entries")
                    ),
                ));
                self.selected_lines.clear();
                self.selected_entries.clear();
                self.note.clear();
            }
            Err(e) => self.flash = Some((StatusKind::Error, e)),
        }
        self.load(scope).await
    }

    /// Bilimbi `authorize_event` for the match capability: refused, the page
    /// says so and nothing else happens.
    async fn may_match(&mut self, scope: &Scope) -> Result<bool> {
        let allowed = self.pages.allowed(scope, MANAGE_MATCHES).await?;
        if !allowed {
            self.flash = Some((StatusKind::Error, DENIED.into()));
        }
        Ok(allowed)
    }

    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        v: &serde_json::Value,
    ) -> Result<()> {
        match event_type(v)? {
            event @ ("opening_match_change"
            | "preview_opening_match"
            | "commit_opening_match"
            | "cancel_opening_match") => {
                let Some(loaded) = &self.loaded else {
                    return Ok(());
                };
                let items = loaded.opening_items.clone();
                let lines = loaded.lines.clone();
                if self
                    .opening_match
                    .event(&self.pages, scope, credential, event, v, (&items, &lines))
                    .await?
                {
                    self.flash = Some((
                        StatusKind::Success,
                        "Opening item matched without another ledger entry.".into(),
                    ));
                    self.load(scope).await?;
                }
            }
            "clear-flash" => clear_flash(&mut self.flash, v),
            grid @ ("lines_grid" | "entries_grid" | "matches_grid") => {
                let columns = match grid {
                    "lines_grid" => &mut self.lines_columns,
                    "entries_grid" => &mut self.entries_columns,
                    _ => &mut self.matches_columns,
                };
                columns.handle(&params::string_values(v));
            }
            "select" => {
                self.selected_lines = ids(v, "lines[]");
                self.selected_entries = ids(v, "entries[]");
                self.note = text(v, "note");
            }
            "match" => {
                if self.may_match(scope).await? {
                    let new = NewMatch {
                        statement_line_ids: ids(v, "lines[]").into_iter().collect(),
                        entry_ids: ids(v, "entries[]").into_iter().collect(),
                        note: Some(text(v, "note")),
                    };
                    self.record(scope, new).await?;
                }
            }
            "accept" => {
                if self.may_match(scope).await? {
                    let line = params::positive_id(v["line"].as_str()).unwrap_or(0);
                    let entry = self
                        .loaded
                        .as_ref()
                        .and_then(|l| l.suggestions.get(&line))
                        .map(|e| e.entry_id);
                    match entry {
                        None => {
                            self.flash = Some((
                                StatusKind::Error,
                                "That suggestion is no longer available.".into(),
                            ))
                        }
                        Some(entry) => {
                            let new = NewMatch {
                                statement_line_ids: vec![line],
                                entry_ids: vec![entry],
                                note: Some("Suggested match".into()),
                            };
                            self.record(scope, new).await?;
                        }
                    }
                }
            }
            "unmatching" => {
                // Only a match this page shows; never an id of the browser's choosing.
                let id = record_id(v);
                self.unmatching = self
                    .loaded
                    .as_ref()
                    .and_then(|l| l.matches.iter().find(|m| Some(m.id) == id))
                    .map(|m| (m.id, m.match_number));
                self.reason.clear();
            }
            "cancel_unmatch" => self.unmatching = None,
            "unmatch_change" => self.reason = text(v, "unmatch[reason]"),
            "unmatch" => {
                self.reason = text(v, "unmatch[reason]");
                let Some((match_id, _)) = self.unmatching else {
                    return Ok(());
                };
                if !self.may_match(scope).await? {
                    return Ok(());
                }
                let is_opening = self
                    .loaded
                    .as_ref()
                    .and_then(|l| l.matches.iter().find(|m| m.id == match_id))
                    .is_some_and(|m| !m.opening_items.is_empty());
                if is_opening {
                    let call = bilibi_agent_api::page::Call {
                        scope,
                        credential,
                        service: super::PAGES,
                        event: "unmatch",
                        contract: "finance.reconciliation.opening.unmatch",
                    };
                    let result=self.pages.contracts.submit(&call,serde_json::json!({"company_id":company(scope)?,"match_id":match_id,"reason":self.reason})).await;
                    match result {
                        Ok(_) => {
                            self.flash =
                                Some((StatusKind::Success, "Opening item unmatched.".into()));
                            self.unmatching = None;
                            self.load(scope).await?;
                        }
                        Err(e) => {
                            self.flash = Some((
                                StatusKind::Error,
                                match e {
                                    bilibi_agent_api::Error::InvalidInput(fields) => fields
                                        .values()
                                        .flatten()
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(" "),
                                    _ => "The opening item could not be unmatched.".into(),
                                },
                            ));
                        }
                    }
                    return Ok(());
                }
                let result = self
                    .pages
                    .reconciliation
                    .unmatch(scope, company(scope)?, match_id, Some(&self.reason))
                    .await;
                match told(result)? {
                    Ok(m) => {
                        self.flash = Some((
                            StatusKind::Success,
                            format!(
                                "Match {} unmatched; its lines and entries are free again.",
                                m.match_number
                            ),
                        ));
                        self.unmatching = None;
                        self.load(scope).await?;
                    }
                    Err(e) => self.flash = Some((StatusKind::Error, e)),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    fn table(
        columns: &ListColumns,
        event: &'static str,
        rows: Vec<FlexRow>,
        actions: bool,
        empty: EmptyState,
    ) -> Result<Html> {
        let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
        table.event = event;
        table.framed = false;
        table.actions = actions;
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.empty = Some(TableEmpty::State(empty));
        render(table)
    }

    async fn body(&self, scope: &Scope) -> Result<String> {
        let Some(l) = &self.loaded else {
            // Out of reach: the live page redirects with the refusal.
            return render(EmptyState::missing(
                "Bank account unavailable",
                self.flash.as_ref().map_or("", |(_, e)| e.as_str()),
            ))
            .map(|html| html.as_str().to_owned());
        };
        let units = l.units;
        let display = self.pages.display(scope).await?;

        let mode = self.lines_columns.mode();
        let mut rows = vec![];
        for item in &l.lines {
            let line = &item.line;
            let date = line.line_date.to_string();
            let select = if l.can_match {
                render(Select {
                    name: "lines[]",
                    value: line.id,
                    dom_id: format!("line-{}-select", line.id),
                    checked: self.selected_lines.contains(&line.id),
                    label: format!("Select statement line {}", line.description),
                })?
            } else {
                Html::EMPTY
            };
            let mut row = FlexRow::new(line.id.to_string())
                .slot("select", select)
                .cell(
                    "date",
                    Cell::date(&date, &bilibi_finance_ledger::dates::display_text(&date)),
                )
                .slot(
                    "date",
                    super::nowrap(&bilibi_finance_ledger::dates::display_text(&date))?,
                )
                .cell("description", Cell::text(&line.description))
                .slot(
                    "description",
                    described(&line.description, line.reference.as_deref(), mode)?,
                )
                .cell(
                    "amount",
                    Cell::text(line.amount.display(MinorUnits::new(i64::from(units)))),
                )
                .slot(
                    "suggested",
                    render(Suggested {
                        line: line.id,
                        entry: l.suggestions.get(&line.id),
                        journal: match l.suggestions.get(&line.id) {
                            Some(e) => EntryLink::named(e.entry_id, &e.description)?,
                            None => Html::EMPTY,
                        },
                        date: l
                            .suggestions
                            .get(&line.id)
                            .map(|e| bilibi_finance_ledger::dates::display(e.entry_date))
                            .unwrap_or_default(),
                        accept: l.can_match,
                        accept_id: format!("line-{}-accept", line.id),
                    })?,
                );
            row.dom_id = Some(format!("line-{}", line.id));
            rows.push(row);
        }
        let lines = Self::table(
            &self.lines_columns,
            "lines_grid",
            rows,
            false,
            if l.has_statements {
                EmptyState::missing(
                    "Every statement line is matched",
                    "Import the next statement to continue.",
                )
            } else {
                let empty = EmptyState::missing(
                    "No statement lines yet",
                    "Import a statement for this bank account, then match its lines here.",
                );
                if l.can_import {
                    let href = format!(
                        "/finance/reconciliation/accounts/{}?do=import",
                        l.account.id
                    );
                    empty.action(render(Button {
                        id: "matching-import-statement",
                        href: &href,
                        variant: ButtonVariant::Primary,
                        body: Html::text("Import statement"),
                        ..Button::default()
                    })?)
                } else {
                    empty
                }
            },
        )?;

        let mode = self.entries_columns.mode();
        let mut rows = vec![];
        for entry in &l.entries {
            let date = entry.entry_date.to_string();
            let select = if l.can_match {
                render(Select {
                    name: "entries[]",
                    value: entry.entry_id,
                    dom_id: format!("entry-{}-select", entry.entry_id),
                    checked: self.selected_entries.contains(&entry.entry_id),
                    label: format!("Select entry {}", entry.entry_number),
                })?
            } else {
                Html::EMPTY
            };
            let mut row = FlexRow::new(entry.entry_id.to_string())
                .slot("select", select)
                .cell("entry", Cell::text(entry.entry_number.to_string()))
                .slot("entry", journal(Some(entry))?)
                .cell(
                    "date",
                    Cell::date(&date, &bilibi_finance_ledger::dates::display_text(&date)),
                )
                .slot(
                    "date",
                    super::nowrap(&bilibi_finance_ledger::dates::display_text(&date))?,
                )
                .cell("description", Cell::text(&entry.description))
                .slot(
                    "description",
                    described(&entry.description, entry.reference.as_deref(), mode)?,
                )
                .cell(
                    "amount",
                    Cell::text(entry.amount.display(MinorUnits::new(i64::from(units)))),
                );
            row.dom_id = Some(format!("entry-{}", entry.entry_id));
            rows.push(row);
        }
        let entries = Self::table(
            &self.entries_columns,
            "entries_grid",
            rows,
            false,
            EmptyState::missing(
                "Every entry on this account is matched",
                "Entries posted to the bank account appear here until a statement line matches them.",
            ),
        )?;

        let mut rows = vec![];
        for m in &l.matches {
            let total: Decimal = m
                .entries
                .iter()
                .map(|e| e.amount.amount())
                .chain(m.opening_items.iter().map(|i| i.amount.amount()))
                .sum();
            let time = display.render(m.matched_at, Format::DateTime, Precision::Minute);
            let mut journals = Html::EMPTY;
            for (i, e) in m.entries.iter().enumerate() {
                if i > 0 {
                    journals.push(&Html::text(", "));
                }
                journals.push(&journal(Some(e))?);
            }
            for i in &m.opening_items {
                if !journals.is_empty() {
                    journals.push(&Html::text(", "));
                }
                journals.push(&Html::text(&format!("Opening item {}", i.source_id)));
            }
            let mut row = FlexRow::new(m.id.to_string())
                .cell("match", Cell::text(m.match_number.to_string()))
                .cell("lines", Cell::text(m.statement_line_ids.len().to_string()))
                .cell(
                    "entries",
                    Cell::text(
                        m.entries
                            .iter()
                            .map(|e| e.entry_number.to_string())
                            .chain(
                                m.opening_items
                                    .iter()
                                    .map(|i| format!("Opening {}", i.source_id)),
                            )
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                )
                .slot("entries", journals)
                .cell(
                    "amount",
                    Cell::text(Money::display_decimal(
                        total,
                        MinorUnits::new(i64::from(units)),
                    )),
                )
                .cell("note", Cell::text(m.note.clone().unwrap_or_default()))
                .cell(
                    "matched",
                    Cell::instant(Instant::new(
                        &time.iso,
                        &time.text,
                        &time.company,
                        &time.utc,
                        time.mode.as_str(),
                    )),
                );
            row.dom_id = Some(format!("match-{}", m.id));
            if l.can_match {
                let label = format!("Unmatch match {}", m.match_number);
                row.actions = render(IconButton {
                    id: &format!("match-{}-unmatch", m.id),
                    kind: IconKind::Danger,
                    event: "unmatching",
                    name: "id",
                    value: &m.id.to_string(),
                    ..IconButton::new("link-slash", &label)
                })?;
            }
            rows.push(row);
        }
        let matches = Self::table(
            &self.matches_columns,
            "matches_grid",
            rows,
            l.can_match && !l.matches.is_empty(),
            EmptyState::missing(
                "No matches yet",
                "Lines matched above are listed here and can be unmatched with a reason.",
            ),
        )?;

        let sum = |items: Vec<Decimal>| {
            Money::display_decimal(items.into_iter().sum(), MinorUnits::new(i64::from(units)))
        };
        let back = format!("/finance/reconciliation/accounts/{}", l.account.id);
        let header = render(Header {
            title: Html::text(&format!("{}: matching", l.account.name)),
            subtitle: "Match each bank line to what is in the books.",
            title_actions: Html::EMPTY,
            actions: render(BackLink {
                href: &back,
                id: "matching-back",
                title: &l.account.name,
                ..BackLink::default()
            })?,
        })?;
        View {
            header,
            flash: flash_map(&self.flash),
            can_match: l.can_match,
            forbidden: if l.can_match {
                Html::EMPTY
            } else {
                render(EmptyState::forbidden("match statement lines").id("matching-forbidden"))?
            },
            line_count: Some(l.lines.len()),
            entry_count: Some(l.entries.len()),
            match_count: Some(l.matches.len()),
            lines,
            entries,
            opening_match: {
                let mut html =
                    self.opening_match
                        .body(&l.opening_items, &l.lines, l.units, l.can_match)?;
                html.push(&self.opening_match.modal(&self.pages).await?);
                html
            },
            matches,
            selected_lines: self.selected_lines.len(),
            selected_entries: self.selected_entries.len(),
            lines_sum: sum(l
                .lines
                .iter()
                .filter(|i| self.selected_lines.contains(&i.line.id))
                .map(|i| i.line.amount.amount())
                .collect()),
            entries_sum: sum(l
                .entries
                .iter()
                .filter(|e| self.selected_entries.contains(&e.entry_id))
                .map(|e| e.amount.amount())
                .collect()),
            note: Field::named("note")
                .id("match-note")
                .label("Note")
                .value(&self.note)
                .attr("maxlength", "500")
                .wrapper_class("mb-0 grow"),
            unmatching: self.unmatching.map(|(_, number)| UnmatchView {
                title: format!("Unmatch match {number}"),
                reason: Field::in_form("unmatch", "reason")
                    .id("unmatch-reason")
                    .label("Reason")
                    .value(&self.reason)
                    .attr("maxlength", "500")
                    .required(),
            }),
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for Matching {
    fn subscriptions(&mut self) -> Vec<Subscription> {
        self.follow.subscriptions()
    }
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                // The mount's first read, or a posting on the account, which
                // changes the ledger side.
                LiveInput::Refresh | LiveInput::Params(_) => {
                    self.follow.posted();
                    self.load(request.scope).await?
                }
                LiveInput::Event(v) => self.event(request.scope, request.credential, v).await?,
            }
            if let Some(redirect) = self.redirect.take() {
                return Ok(LiveBody {
                    redirect: Some(redirect),
                    flash: self.flash.take(),
                    ..Default::default()
                });
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                ..Default::default()
            })
        })
    }
}
