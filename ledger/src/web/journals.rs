//! Journals: postings from every source and unposted manual journals.
//! Approved manual journals appear as their posting; waiting journals post nothing.
use super::{
    dialog_actions, error_text, event_type, figure, journal_period::Window, no_company, render,
    shell, source_label, unavailable, view, Failure, Flash, Pages, Read, Refusal,
};
use crate::SUBMIT_JOURNALS;
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{filter::Outcome, Clock, Mode, PeriodFilter, Preset};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Attrs, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Cell,
    Choice, ColumnsOutcome, Combobox, CreateButton, EmptyState, Field, Filter, FilterToolbar,
    FlexRow, FlexTable, Form, Header, Html, InputKind, ListColumns, ListSpec, ListState, Modal,
    ModalWidth, PageVariant, RecordLink, StatusKind, TableEmpty, ToolbarControl,
};
use jiff::Timestamp;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

pub(super) const INDEX: &str = "/finance/ledger/journals";
const LIMIT: usize = 100;
const FILTER_ID: &str = "ledger-journals-period";
const ALL_DATES: &str = "all";
fn clear_event() -> String {
    format!("period_clear:{FILTER_ID}")
}
/// Fields a posted form keeps are bounded (64), so a journal has at most this many lines.
const MAX_LINES: usize = 12;

const BOOK_UNREADABLE: &str =
    "The company's book could not be read. Reload the page and try again.";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: vec![100],
        default_page_size: 100,
        filters: BTreeMap::from([
            (
                "status".into(),
                Filter::one_of(
                    &[
                        "all",
                        "submitted",
                        "posted",
                        "reversed",
                        "reversal",
                        "rejected",
                        "withdrawn",
                    ],
                    "all",
                ),
            ),
            (
                "recording".into(),
                Filter::one_of(
                    &["all", "on_time", "approaching", "late", "unposted"],
                    "all",
                ),
            ),
            (
                "review".into(),
                Filter::one_of(&["all", "pending", "reviewed"], "all"),
            ),
            ("after".into(), Filter::string("")),
            ("source".into(), Filter::string("")),
            ("account".into(), Filter::string("")),
            ("through".into(), Filter::string("")),
        ]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("manual journals list spec")
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Debit,
    Credit,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Self::Debit => Self::Credit,
            Self::Credit => Self::Debit,
        }
    }
}

#[derive(Clone, Default)]
struct LineDraft {
    account: String,
    debit: String,
    credit: String,
    description: String,
}

impl LineDraft {
    fn amount(&self, side: Side) -> &str {
        match side {
            Side::Debit => self.debit.trim(),
            Side::Credit => self.credit.trim(),
        }
    }
    fn set(&mut self, side: Side, text: &str) {
        match side {
            Side::Debit => self.debit = text.to_owned(),
            Side::Credit => self.credit = text.to_owned(),
        }
    }
    fn no_amount(&self) -> bool {
        self.debit.trim().is_empty() && self.credit.trim().is_empty()
    }
    /// The one positive amount on this line, when it carries exactly one and
    /// the book accepts it; a line with both sides, a negative or an amount
    /// finer than the book's minor units is nothing to balance against.
    fn single(&self, units: crate::MinorUnits) -> Option<(Side, crate::Money)> {
        let side = match (
            self.amount(Side::Debit).is_empty(),
            self.amount(Side::Credit).is_empty(),
        ) {
            (false, true) => Side::Debit,
            (true, false) => Side::Credit,
            _ => return None,
        };
        crate::Money::parse(self.amount(side), units)
            .ok()
            .filter(|money| money.is_positive())
            .map(|money| (side, money))
    }
}

/// The opposite amount the dialog filled in to balance a two-line journal.
#[derive(Clone)]
struct Fill {
    line: usize,
    side: Side,
    /// The amount written in, with the book's decimal places; while the field
    /// still holds exactly this, it is the dialog's suggestion, not the person's.
    amount: String,
}

/// The journal a correction dialog withdraws when it is submitted.
struct Replaces {
    id: i64,
    number: i64,
}

/// The new-journal dialog's values and what the ledger said about them.
struct Draft {
    date: String,
    description: String,
    reference: String,
    /// Why the journal is being replaced (optional), when the dialog corrects one.
    reason: String,
    replaces: Option<Replaces>,
    /// What the dialog filled in from an earlier journal that the person should check.
    note: String,
    units: crate::MinorUnits,
    lines: Vec<LineDraft>,
    refusal: Refusal,
    /// For each line sent, which dialog line it was (blank lines are not sent).
    sent: Vec<usize>,
    notice: String,
    /// The balancing amount the dialog filled in, if it is still showing.
    fill: Option<Fill>,
    /// The amount whose suggestion the person cleared: it is not offered
    /// again until the amount it balanced changes.
    declined: Option<String>,
    /// The line whose typed amount "Fill difference" would replace, waiting
    /// for the person to ask a second time.
    replacing: Option<usize>,
}

/// What "Fill difference" would put on a line: the side and amount that
/// balance the other lines.
struct Balancing {
    side: Side,
    money: crate::Money,
}

impl Draft {
    fn new(today: String, units: crate::MinorUnits) -> Self {
        Self {
            date: today,
            description: String::new(),
            reference: String::new(),
            reason: String::new(),
            replaces: None,
            note: String::new(),
            units,
            lines: vec![LineDraft::default(), LineDraft::default()],
            refusal: Refusal::default(),
            sent: Vec::new(),
            notice: String::new(),
            fill: None,
            declined: None,
            replacing: None,
        }
    }

    /// A dialog holding an earlier journal's date, description, reference and
    /// lines: its correction (`replaces` set, so submitting withdraws it) or a
    /// fresh journal prefilled from a rejected one. An account the chart no
    /// longer offers is left to choose again.
    fn from_journal(
        journal: &view::Journal,
        units: crate::MinorUnits,
        accounts: &[view::Account],
        replaces: bool,
    ) -> Self {
        let places = usize::from(units.get());
        let shown = |text: &str| {
            let amount = view::decimal(text);
            if amount.is_zero() {
                String::new()
            } else {
                format!("{amount:.places$}")
            }
        };
        let mut gone = false;
        let mut lines: Vec<LineDraft> = journal
            .lines
            .iter()
            .map(|line| {
                let active = accounts
                    .iter()
                    .any(|a| a.id == line.account_id && a.postable());
                gone |= !active;
                LineDraft {
                    account: if active {
                        line.account_id.to_string()
                    } else {
                        String::new()
                    },
                    debit: shown(&line.debit),
                    credit: shown(&line.credit),
                    description: line.description.clone().unwrap_or_default(),
                }
            })
            .collect();
        while lines.len() < 2 {
            lines.push(LineDraft::default());
        }
        let mut draft = Self::new(journal.entry_date.clone(), units);
        draft.description = journal.description.clone();
        draft.reference = journal.reference.clone().unwrap_or_default();
        draft.lines = lines;
        if replaces {
            draft.replaces = Some(Replaces {
                id: journal.id,
                number: journal.journal_number,
            });
        }
        if !replaces {
            draft.note = format!(
                "Started from journal {}, which is {}. It needs its own approval.",
                journal.journal_number, journal.status
            );
        }
        if gone {
            if !draft.note.is_empty() {
                draft.note.push(' ');
            }
            draft.note.push_str(
                "A line uses an account that is no longer active. Choose another account for it.",
            );
        }
        draft
    }

    /// Fills the empty opposite amount of a two-line journal with the amount
    /// that balances it, once the other line has its one amount and this
    /// line has its account. `before` holds the lines as the person last saw
    /// them. The value stays visible and editable: an amount the person typed
    /// is never touched, an edited suggestion becomes theirs, an amount the
    /// person clears, suggested or their own, stays clear until the amount it
    /// balanced changes, and a suggestion whose basis is gone (the amount
    /// changed, cleared, made invalid, both sides filled, the account unset, a
    /// third line added) is withdrawn or refilled from the new amount.
    fn suggest(&mut self, before: &[LineDraft]) {
        if self.lines.len() != 2 {
            self.fill = None;
            return;
        }
        let mut withdrawn = None;
        if let Some(fill) = self.fill.take() {
            let shown = self.lines[fill.line].amount(fill.side).to_owned();
            if shown == fill.amount {
                // Still the dialog's own figure: withdrawn, and offered again
                // below while it still balances.
                self.lines[fill.line].set(fill.side, "");
                withdrawn = Some(fill.line);
            }
            // Otherwise the person made the amount their own.
        }
        for source in 0..2 {
            let target = 1 - source;
            let Some((side, money)) = self.lines[source].single(self.units) else {
                continue;
            };
            let amount = money.shown(self.units);
            let was_empty = withdrawn == Some(target)
                || match before.get(target) {
                    Some(line) => line.no_amount(),
                    None => true,
                };
            if !was_empty && self.lines[target].no_amount() {
                self.declined = Some(amount);
                continue;
            }
            if !was_empty
                || !self.lines[target].no_amount()
                || self.lines[target].account.is_empty()
                || self.declined.as_deref() == Some(amount.as_str())
            {
                continue;
            }
            self.lines[target].set(side.other(), &amount);
            self.fill = Some(Fill {
                line: target,
                side: side.other(),
                amount,
            });
            self.declined = None;
            return;
        }
    }

    /// The amount that balances the journal when it goes on `line`, from every
    /// other line. The error is the sentence to show when there is none.
    fn balancing(&self, line: usize) -> std::result::Result<Balancing, String> {
        let mut difference = Decimal::ZERO;
        for (i, other) in self.lines.iter().enumerate().filter(|(i, _)| *i != line) {
            for (side, sign) in [(Side::Debit, Decimal::ONE), (Side::Credit, -Decimal::ONE)] {
                let text = other.amount(side);
                if text.is_empty() {
                    continue;
                }
                match crate::Money::parse(text, self.units) {
                    Ok(money) => difference += sign * money.amount(),
                    Err(_) => {
                        return Err(format!(
                            "Line {} has an amount the book cannot hold. Correct it first.",
                            i + 1
                        ));
                    }
                }
            }
        }
        let side = if difference.is_sign_negative() {
            Side::Debit
        } else {
            Side::Credit
        };
        match crate::Money::new(difference.abs(), self.units) {
            Ok(money) if money.is_positive() => Ok(Balancing { side, money }),
            Ok(_) => Err("The other lines already balance. There is no difference to fill.".into()),
            Err(_) => Err("The difference cannot be held by the book.".into()),
        }
    }

    /// Puts the balancing amount on `line`. A typed amount there that differs
    /// is replaced only when asked for a second time in a row.
    fn fill_difference(&mut self, line: usize) {
        self.refusal = Refusal::default();
        self.notice.clear();
        let asked = self.replacing.take() == Some(line);
        if self.lines.len() < 3 || line >= self.lines.len() {
            return;
        }
        let balancing = match self.balancing(line) {
            Ok(balancing) => balancing,
            Err(text) => {
                self.notice = text;
                return;
            }
        };
        let amount = balancing.money.shown(self.units);
        let current = &self.lines[line];
        if current.amount(balancing.side) == amount
            && current.amount(balancing.side.other()).is_empty()
        {
            return;
        }
        if !current.no_amount() && !asked {
            self.replacing = Some(line);
            return;
        }
        self.lines[line].set(balancing.side, &amount);
        self.lines[line].set(balancing.side.other(), "");
    }

    /// Whether the amount at `line` and `side` is the dialog's own suggestion.
    fn suggested(&self, line: usize, side: Side) -> bool {
        self.fill
            .as_ref()
            .is_some_and(|fill| fill.line == line && fill.side == side)
    }

    fn read(&mut self, value: &Value) {
        let fields = params::form_values(value, "journal");
        let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
        self.date = get("entry_date");
        self.description = get("description");
        self.reference = get("reference");
        self.reason = get("reason");
        for (i, line) in self.lines.iter_mut().enumerate() {
            line.account = get(&format!("line{i}_account"));
            line.debit = get(&format!("line{i}_debit"));
            line.credit = get(&format!("line{i}_credit"));
            line.description = get(&format!("line{i}_description"));
        }
        self.refusal = Refusal::default();
        self.notice.clear();
        self.replacing = None;
    }

    fn input(&mut self) -> Value {
        self.sent.clear();
        let mut lines = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            if line.account.is_empty()
                && line.debit.trim().is_empty()
                && line.credit.trim().is_empty()
                && line.description.trim().is_empty()
            {
                continue;
            }
            let mut out = json!({});
            if let Ok(account) = line.account.parse::<i64>() {
                out["account_id"] = json!(account);
            } else {
                // The contract needs an account id; zero is refused with its own words.
                out["account_id"] = json!(0);
            }
            if !line.debit.trim().is_empty() {
                out["debit"] = json!(line.debit.trim());
            }
            if !line.credit.trim().is_empty() {
                out["credit"] = json!(line.credit.trim());
            }
            if !line.description.trim().is_empty() {
                out["description"] = json!(line.description.trim());
            }
            lines.push(out);
            self.sent.push(i);
        }
        let mut input = json!({
            "entry_date": self.date,
            "description": self.description.trim(),
            "lines": lines,
        });
        if !self.reference.trim().is_empty() {
            input["reference"] = json!(self.reference.trim());
        }
        if let Some(replaces) = &self.replaces {
            input["journal_id"] = json!(replaces.id);
            if !self.reason.trim().is_empty() {
                input["reason"] = json!(self.reason.trim());
            }
        }
        input
    }

    /// The messages that belong to one dialog line and one of its fields.
    fn line_errors(&self, line: usize, field: &str) -> Vec<String> {
        let Some(sent) = self.sent.iter().position(|&i| i == line) else {
            return Vec::new();
        };
        self.refusal
            .fields
            .get(&format!("lines.{sent}.{field}"))
            .into_iter()
            .flatten()
            .map(|m| error_text(field, m))
            .collect()
    }

    fn totals(&self) -> (Decimal, Decimal) {
        let sum = |pick: fn(&LineDraft) -> &str| {
            self.lines
                .iter()
                .map(|l| view::decimal(pick(l).trim()))
                .sum::<Decimal>()
        };
        (sum(|l| &l.debit), sum(|l| &l.credit))
    }
}

pub(super) struct Journals {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    loaded_keys: Option<Vec<String>>,
    flash: Flash,
    draft: Option<Draft>,
    opening_dialog: Option<super::opening_dialog::OpeningDialog>,
    destination: Option<String>,
    /// A journal named in the address (`correct` or `copy`) whose dialog is
    /// still to open: it is read through the contract on the first update.
    opening: Option<Opening>,
    framed: bool,
    window: Window,
    period: PeriodFilter,
    browser_zone: Option<String>,
    timezone_request: String,
}

/// How an address names an earlier journal to prefill the dialog from.
#[derive(Clone, Copy)]
struct Opening {
    id: i64,
    /// Whether submitting withdraws it (a correction) or it only prefills.
    replaces: bool,
}

impl Opening {
    fn parse(query: &HashMap<String, String>) -> Option<Self> {
        let find = |key: &str| query.get(key).and_then(|v| v.parse::<i64>().ok());
        find("correct")
            .map(|id| Self { id, replaces: true })
            .or_else(|| {
                find("copy").map(|id| Self {
                    id,
                    replaces: false,
                })
            })
    }
}

impl Journals {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = vec![
            BuiltinColumn::new("number", "Number"),
            BuiltinColumn::new("request", "Request"),
            BuiltinColumn::new("date", "Date"),
            BuiltinColumn::new("description", "Description"),
            BuiltinColumn::new("source", "Source"),
            BuiltinColumn::new("status", "Status"),
            BuiltinColumn::new("recording", "Recording"),
            BuiltinColumn::new("review", "Late review"),
            BuiltinColumn::new("amount", "Amount").right(),
        ];
        let window = Window::parse(query);
        let clock = Clock {
            timezone: "Etc/UTC".into(),
            today: Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date(),
        };
        Ok(Self {
            period: PeriodFilter::new(FILTER_ID, window.widget(&clock), &clock),
            window,
            browser_zone: None,
            timezone_request: Timestamp::now().to_string(),
            pages: pages.clone(),
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("ledger-journals-table", columns)
                .map_err(|_| Error::Internal)?,
            flash: Flash::default(),
            loaded_keys: None,
            draft: None,
            opening_dialog: (query.get("opening").map(String::as_str) == Some("1"))
                .then(|| super::opening_dialog::OpeningDialog::new(pages.clone(), None)),
            destination: None,
            opening: Opening::parse(query),
            framed,
        })
    }

    fn filter(&self, key: &str) -> &str {
        self.list.filters.get(key).map_or("", String::as_str)
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let can_submit = self.pages.allowed(scope, SUBMIT_JOURNALS).await?;
        let mut body = Html::EMPTY;
        let mut actions = Html::EMPTY;
        let mut shown = 0;
        if Pages::company(scope).is_none() {
            body.push(&no_company("ledger-journals-no-company")?);
        } else {
            match self.pages.book(scope).await? {
                Err(refused) => body.push(&unavailable(
                    "ledger-journals-refused",
                    "manual journals",
                    &refused,
                )?),
                Ok(state) => match state.book {
                    None => body.push(&render(
                        EmptyState::missing(
                            "The book is not open yet",
                            "Open the company's book before you submit a journal.",
                        )
                        .id("ledger-journals-no-book"),
                    )?),
                    Some(book) => {
                        if can_submit {
                            actions.push(&render(Button {
                                id: "ledger-opening-new",
                                event: "new_opening",
                                button_type: "button",
                                body: Html::text("Opening balances"),
                                ..Button::default()
                            })?);
                            actions.push(&render(CreateButton {
                                noun: "Journal",
                                id: "ledger-journal-new",
                                class: "text-xs",
                                event: "new",
                                button_type: "button",
                                ..CreateButton::default()
                            })?);
                        }
                        shown = self.table(scope, book.minor_units, &mut body).await?;
                    }
                },
            }
        }
        let mut page = render(Header {
            title: Html::text("Journals"),
            subtitle: "",
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        page.push(&body);
        if self.draft.is_some() && can_submit {
            let accounts = self.pages.chart(scope).await?.unwrap_or_default();
            let form = self.form(&accounts)?;
            // The line rule.
            let lines = "Each line takes an account and one amount, a debit or a credit. Debits and credits must balance.";
            let (title, description) = match self.draft.as_ref().and_then(|d| d.replaces.as_ref()) {
                Some(replaces) => (
                    format!("Correct journal {}", replaces.number),
                    format!(
                        "Submitting withdraws journal {} and submits this journal in its place. Nothing posts until the corrected journal is approved. {lines}",
                        replaces.number
                    ),
                ),
                None => ("New manual journal".into(), lines.to_owned()),
            };
            page.push(&render(Modal {
                id: "ledger-journal-modal",
                title: &title,
                cancel_event: "close_modal",
                width: ModalWidth::Wide,
                description: &description,
                body: form,
                ..Modal::default()
            })?);
        }
        if let Some(dialog) = &mut self.opening_dialog {
            page.push(&dialog.view(scope).await?);
        }
        let mut query = self.list.to_params();
        if let Some(dialog) = &self.opening_dialog {
            if let Some(id) = dialog.replaces() {
                query.insert("correct".into(), id.to_string());
            } else {
                query.insert("opening".into(), "1".into());
            }
        }
        query.extend(self.window.params());
        if let Some(draft) = &self.draft {
            match &draft.replaces {
                Some(replaces) => query.insert("correct".into(), replaces.id.to_string()),
                None => query.insert("new".into(), "1".into()),
            };
        }
        Ok(LiveBody {
            html: shell("ledger-journals", PageVariant::List, &self.flash, page)?,
            query,
            rows: shown,
            redirect: self.destination.take(),
            ..LiveBody::default()
        })
    }

    async fn clock(&self, scope: &Scope) -> Result<(Clock, bool)> {
        let display = self.pages.display(scope).await?;
        let settled = display.mode != Mode::Local || self.browser_zone.is_some();
        Ok((
            display.clock(self.browser_zone.as_deref(), Timestamp::now()),
            settled,
        ))
    }

    async fn table(&mut self, scope: &Scope, units: u8, body: &mut Html) -> Result<usize> {
        let (clock, settled) = self.clock(scope).await?;
        if let Some(period) = self.window.resolved(&clock) {
            if (self.window.from != Some(period.from) || self.window.to != Some(period.to))
                && (settled || self.window.from.is_none())
            {
                self.list.filters.insert("after".into(), String::new());
                self.window.apply(period);
            }
        }
        self.period.set(self.window.widget(&clock), &clock);
        let mut input = json!({"page_size": LIMIT});
        if let Some(date) = self.window.from {
            input["from"] = json!(date.to_string());
        }
        if let Some(date) = self.window.to {
            input["to"] = json!(date.to_string());
        }
        if self.filter("status") != "all" {
            input["status"] = json!(self.filter("status"));
        }
        for key in ["recording", "review"] {
            if self.filter(key) != "all" {
                input[key] = json!(self.filter(key));
            }
        }
        if !self.filter("source").is_empty() {
            input["source"] = json!(self.filter("source"));
        }
        if let Ok(id) = self.filter("account").parse::<i64>() {
            input["account_id"] = json!(id);
        }
        let search: String = self.list.search.trim().chars().take(20).collect();
        if !search.is_empty() {
            input["search"] = json!(search);
        }
        if let Ok(n) = self.filter("through").parse::<i64>() {
            input["through_entry_number"] = json!(n);
        }
        let cursor: Vec<_> = self.filter("after").split('|').collect();
        if cursor.len() == 3 {
            if let Ok(id) = cursor[1].parse::<i64>() {
                input["after_time"] = json!(cursor[0]);
                input["after_id"] = json!(id);
                input["after_kind"] = json!(cursor[2]);
            }
        }
        let value = match self
            .pages
            .read(scope, "finance.ledger.journals.list", input)
            .await?
        {
            Read::Found(value) => value,
            other => {
                body.push(&unavailable("ledger-journals-refused", "journals", &other)?);
                return Ok(0);
            }
        };
        let records: Vec<ListRow> = view::parse(value["records"].clone()).unwrap_or_default();
        let mut sources = vec![
            Choice::new("All sources", ""),
            Choice::new("Manual", "manual"),
            Choice::new("Opening", "opening"),
        ];
        if let Some(items) = value["sources"].as_array() {
            for item in items {
                if let Some(source) = item.as_str() {
                    sources.push(Choice::new(source_label(source), source));
                }
            }
        }
        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "ledger-journals-filters",
            vec![
                ToolbarControl::search(
                    "ledger-journals-search",
                    "search",
                    "Search",
                    &form["search"],
                    "Entry or request number",
                ),
                ToolbarControl::select(
                    "ledger-journals-status",
                    "status",
                    "Status",
                    &form["status"],
                    vec![
                        Choice::new("All statuses", "all"),
                        Choice::new("Waiting for approval", "submitted"),
                        Choice::new("Posted", "posted"),
                        Choice::new("Reversed", "reversed"),
                        Choice::new("Reversal", "reversal"),
                        Choice::new("Rejected", "rejected"),
                        Choice::new("Withdrawn", "withdrawn"),
                    ],
                ),
                ToolbarControl::select(
                    "ledger-journals-recording",
                    "recording",
                    "Recording",
                    &form["recording"],
                    vec![
                        Choice::new("All recordings", "all"),
                        Choice::new("On time", "on_time"),
                        Choice::new("Approaching late", "approaching"),
                        Choice::new("Late", "late"),
                        Choice::new("Unposted", "unposted"),
                    ],
                ),
                ToolbarControl::select(
                    "ledger-journals-review",
                    "review",
                    "Late review",
                    &form["review"],
                    vec![
                        Choice::new("All reviews", "all"),
                        Choice::new("Pending review", "pending"),
                        Choice::new("Reviewed", "reviewed"),
                    ],
                ),
                ToolbarControl::select(
                    "ledger-journals-source",
                    "source",
                    "Source",
                    &form["source"],
                    sources,
                ),
            ],
        ))?;
        let chart = self.pages.chart(scope).await?.unwrap_or_default();
        let mut account_options = vec![Choice::new("All accounts", "")];
        account_options.extend(
            view::by_name(chart.iter().filter(|a| !a.is_group))
                .iter()
                .map(|a| view::account_choice(a)),
        );
        let account = render(Form {
            id: "ledger-journals-account-filter",
            event: "filters",
            change: true,
            class: "mb-0 w-full sm:w-fit sm:min-w-36",
            body: render(
                Combobox::new(
                    "ledger-journals-account",
                    format!("{FILTERS_FORM}[account]"),
                )
                .label("Account")
                .placeholder("All accounts")
                .wrapper_class("mb-0")
                .value(&form["account"])
                .options(account_options),
            )?,
            actions: Html::EMPTY,
        })?;
        let mut clear = Html::EMPTY;
        let period = if self.window.is_all() && !self.period.is_open() {
            let mut options = vec![Choice::new("All dates", ALL_DATES)];
            options.extend(
                Preset::ALL
                    .iter()
                    .map(|(label, preset)| Choice::new(*label, preset.as_str())),
            );
            render(AllDates {
                id: format!("{FILTER_ID}-all"),
                event: self.period.select_event(),
                field: render(
                    Field::in_form("period_filter", "period")
                        .id(format!("{FILTER_ID}-period"))
                        .kind(InputKind::Select)
                        .label(self.period.label.clone())
                        .label_class("sr-only")
                        .wrapper_class("mb-0")
                        .value(ALL_DATES)
                        .options(options),
                )?,
            })?
        } else {
            if !self.window.is_all() {
                clear = render(Button {
                    id: "ledger-journals-period-clear",
                    button_type: "button",
                    event: &clear_event(),
                    body: Html::text("All dates"),
                    ..Button::default()
                })?;
            }
            render(self.period.clone())?
        };
        body.push(&render(FilterRegion {
            timezone_request: &self.timezone_request,
            toolbar,
            account,
            period,
            clear,
        })?);
        if !self.filter("through").is_empty() {
            body.push(&render(Alert {
                id: "ledger-journals-through",
                kind: StatusKind::Info,
                class: "mb-3",
                body: Html::text(&format!("Showing entries through number {}, as the trial balance counted them. Later entries and unposted journals are left out.", self.filter("through"))),
            })?);
        }
        let keys: Vec<_> = records
            .iter()
            .map(|row| format!("{}-{}", row.kind, row.id))
            .collect();
        if self.loaded_keys.as_ref() != Some(&keys) {
            self.columns
                .load(&records, |row| format!("{}-{}", row.kind, row.id));
            self.loaded_keys = Some(keys);
        }
        let mut rows = Vec::new();
        for record in &records {
            let href = if record.kind == "entry" {
                format!("/finance/ledger/journals/entry-{}", record.id)
            } else {
                format!("{INDEX}/{}", record.id)
            };
            let key = format!("{}-{}", record.kind, record.id);
            let record_id = if record.kind == "entry" {
                record.id.to_string()
            } else {
                key.clone()
            };
            let status = match record.status.as_str() {
                "posted" => Badge::status(BadgeKind::Success, "Posted"),
                "reversed" => Badge::status(BadgeKind::Warning, "Reversed"),
                "reversal" => Badge::status(BadgeKind::Neutral, "Reversal"),
                "rejected" => Badge::status(BadgeKind::Danger, "Rejected"),
                "withdrawn" => Badge::status(BadgeKind::Neutral, "Withdrawn"),
                _ => Badge::status(BadgeKind::Warning, "Waiting for approval"),
            };
            let link = |body: Html| {
                render(RecordLink {
                    workspace: self.framed,
                    kind: crate::MODULE,
                    record_id: &record_id,
                    href: &href,
                    class: "font-medium text-ink-strong hover:underline",
                    body,
                })
            };
            let mut row = FlexRow::new(&key)
                .cell(
                    "request",
                    Cell::text(record.request.map(|n| n.to_string()).unwrap_or_default()),
                )
                .cell("date", Cell::text(&record.date));
            row = match record.number {
                Some(number) => row
                    .slot("number", link(Html::text(&number.to_string()))?)
                    .cell("description", Cell::text(&record.description)),
                None => row
                    .cell("number", Cell::text(""))
                    .slot("description", link(Html::text(&record.description))?),
            };
            row = row
                .cell(
                    "source",
                    Cell::text(if record.source == "manual" {
                        "Manual".to_owned()
                    } else {
                        source_label(&record.source)
                    }),
                )
                .slot("status", render(status)?)
                .cell(
                    "recording",
                    Cell::text(match record.recording.as_str() {
                        "late" => {
                            format!("Late · {} days", record.recorded_days.unwrap_or_default())
                        }
                        "approaching" => format!(
                            "Approaching late · {} days",
                            record.recorded_days.unwrap_or_default()
                        ),
                        "on_time" => "On time".into(),
                        _ => "—".into(),
                    }),
                )
                .cell(
                    "review",
                    Cell::text(match record.review.as_str() {
                        "pending" => "Pending review",
                        "reviewed" => "Reviewed",
                        _ => "—",
                    }),
                )
                .slot(
                    "amount",
                    figure(&crate::Money::display_decimal(
                        view::decimal(&record.amount),
                        crate::MinorUnits::new(i64::from(units)),
                    ))?,
                );
            row.dom_id = Some(format!("ledger-{key}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("ledger-journals-table", self.columns.flex_columns(), rows);
        table.caption = "Journals";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text(
            "No journals match these filters.",
        )));
        let mut card = render(table)?;
        if value["more"].as_bool() == Some(true) {
            if let Some(last) = records.last() {
                let mut later = self.list.clone();
                later.filters.insert(
                    "after".into(),
                    format!("{}|{}|{}", last.stamp, last.id, last.kind),
                );
                let mut query = later.to_params();
                query.extend(self.window.params());
                card.push(&render(LaterLink {
                    href: params::address(INDEX, &query),
                })?);
            }
        }
        body.push(&render(Card {
            id: "ledger-journals-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        Ok(records.len())
    }

    fn form(&self, accounts: &[view::Account]) -> Result<Html> {
        let draft = self.draft.as_ref().ok_or(Error::Internal)?;
        let errors = |name: &str| -> Vec<String> {
            draft
                .refusal
                .field(name)
                .iter()
                .map(|m| error_text(name, m))
                .collect()
        };
        let choices: Vec<Choice> = view::by_name(accounts.iter().filter(|a| a.postable()))
            .iter()
            .map(|a| view::account_choice(a))
            .collect();
        let mut body = Html::EMPTY;
        let mut messages = draft.refusal.alert(&[
            "entry_date",
            "description",
            "reference",
            "account_id",
            "debit",
            "credit",
        ]);
        if !draft.notice.is_empty() {
            messages.push(draft.notice.clone());
        }
        if !messages.is_empty() {
            body.push(&render(Alert {
                id: "ledger-journal-notice",
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(&messages.join(" ")),
            })?);
        }
        if !draft.note.is_empty() {
            body.push(&render(Alert {
                id: "ledger-journal-prefill-note",
                kind: StatusKind::Info,
                class: "mb-3",
                body: Html::text(&draft.note),
            })?);
        }
        for field in [
            Field::in_form("journal", "entry_date")
                .id("ledger-journal-date")
                .label("Date")
                .kind(InputKind::Date)
                .value(&draft.date)
                .required()
                .errors(errors("entry_date")),
            Field::in_form("journal", "description")
                .id("ledger-journal-description")
                .label("Description")
                .value(&draft.description)
                .required()
                .errors(errors("description"))
                .attr("data-debounce", "300"),
            Field::in_form("journal", "reference")
                .id("ledger-journal-reference")
                .label("Reference")
                .value(&draft.reference)
                .errors(errors("reference"))
                .attr("data-debounce", "300"),
        ] {
            body.push(&render(field)?);
        }
        if draft.replaces.is_some() {
            body.push(&render(
                Field::in_form("journal", "reason")
                    .id("ledger-journal-reason")
                    .label("Reason for the correction")
                    .hint("Optional. Kept with the withdrawn journal. Up to 500 characters.")
                    .value(&draft.reason)
                    .errors(errors("reason"))
                    .attr("data-debounce", "300"),
            )?);
        }
        for (i, line) in draft.lines.iter().enumerate() {
            let mut remove = Html::EMPTY;
            if draft.lines.len() > 2 {
                remove = render(Button {
                    id: &format!("ledger-journal-remove-{i}"),
                    button_type: "button",
                    event: "remove_line",
                    name: "index",
                    value: &i.to_string(),
                    body: Html::text("Remove"),
                    ..Button::default()
                })?;
            }
            let cell = |key: &str, label: &str| {
                Field::in_form("journal", &format!("line{i}_{key}"))
                    .id(format!("ledger-journal-line{i}-{key}"))
                    .label(label)
                    .wrapper_class("mb-0")
                    .errors(draft.line_errors(i, if key == "account" { "account_id" } else { key }))
            };
            // Every line that is sent needs its account; the two amounts are
            // one choice between them, so neither is marked on its own.
            let account_field = cell("account", "Account");
            let account = render(
                Combobox::new(account_field.id.clone(), account_field.name.clone())
                    .label("Account *")
                    .aria_label("Account")
                    .placeholder("Choose an account")
                    .wrapper_class("mb-0")
                    .options(choices.clone())
                    .value(&line.account)
                    .errors(account_field.errors.clone())
                    .attrs(Attrs {
                        required: true,
                        ..Attrs::default()
                    }),
            )?;
            let replacing = draft
                .replacing
                .filter(|line| *line == i)
                .and_then(|line| draft.balancing(line).ok());
            let amount = |side: Side, key: &str, label: &str, value: &str| {
                let mut field = cell(key, label)
                    .value(value)
                    .attr("inputmode", "decimal")
                    .attr("data-debounce", "300");
                if draft.suggested(i, side) {
                    field = field.hint("Filled to balance the journal. Change it if needed.");
                }
                field
            };
            let debit = render(amount(Side::Debit, "debit", "Debit", &line.debit))?;
            let credit = render(amount(Side::Credit, "credit", "Credit", &line.credit))?;
            let note = render(
                cell("description", "Line description")
                    .value(&line.description)
                    .attr("data-debounce", "300"),
            )?;
            // With three or more lines the balancing amount is a deliberate
            // choice of line, not a suggestion.
            let mut fill = Html::EMPTY;
            if draft.lines.len() > 2 {
                fill = render(Button {
                    id: &format!("ledger-journal-fill-{i}"),
                    button_type: "button",
                    event: "fill_difference",
                    name: "index",
                    value: &i.to_string(),
                    body: render(FillLabel {
                        n: i + 1,
                        replace: replacing.as_ref().map(|b| {
                            format!(
                                "{} {}",
                                match b.side {
                                    Side::Debit => "debit",
                                    Side::Credit => "credit",
                                },
                                b.money.display(draft.units)
                            )
                        }),
                    })?,
                    ..Button::default()
                })?;
            }
            // The ledger names a line problem on the account field when the
            // account is missing, and on the amount fields otherwise.
            body.push(&render(LineRow {
                n: i,
                account,
                debit,
                credit,
                note,
                fill,
                remove,
            })?);
        }
        let (debits, credits) = draft.totals();
        let difference = debits - credits;
        let units = draft.units.get();
        body.push(&render(Totals {
            text: format!(
                "Debits {}. Credits {}. {}",
                view::amount(&debits.to_string(), units),
                view::amount(&credits.to_string(), units),
                if difference.is_zero() {
                    "They match.".to_owned()
                } else {
                    format!(
                        "The difference is {}.",
                        view::amount(&difference.abs().to_string(), units)
                    )
                }
            ),
        })?);
        let mut actions = render(Button {
            id: "ledger-journal-add-line",
            button_type: "button",
            event: "add_line",
            disabled: draft.lines.len() >= MAX_LINES,
            body: Html::text("Add line"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "ledger-journal-cancel",
            button_type: "button",
            event: "close_modal",
            class: "ml-auto",
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        actions.push(&render(Button {
            id: "ledger-journal-submit",
            button_type: "button",
            event: if draft.replaces.is_some() {
                "replace_journal"
            } else {
                "submit_journal"
            },
            variant: ButtonVariant::Primary,
            working: "Submitting…",
            body: Html::text(if draft.replaces.is_some() {
                "Submit correction"
            } else {
                "Submit for approval"
            }),
            ..Button::default()
        })?);
        render(Form {
            id: "ledger-journal-form",
            event: "validate",
            change: true,
            class: "mt-4",
            body,
            actions: dialog_actions(actions)?,
        })
    }

    async fn submit(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(draft) = &mut self.draft else {
            return Ok(());
        };
        let input = draft.input();
        let (event, contract) = if draft.replaces.is_some() {
            ("replace_journal", "finance.ledger.journal.replace")
        } else {
            ("submit_journal", "finance.ledger.journal.submit")
        };
        match self
            .pages
            .submit(r.scope, r.credential, event, contract, input)
            .await?
        {
            Ok(done) => {
                let number = done["result"]["record"]["journal_number"].as_i64();
                let replaced = draft.replaces.as_ref().map(|replaces| replaces.number);
                self.draft = None;
                self.flash.success(match (number, replaced) {
                    (Some(n), Some(old)) => format!(
                        "Journal {old} is withdrawn. Journal {n} is submitted in its place and awaiting approval."
                    ),
                    (Some(n), None) => {
                        format!("Journal {n} is submitted and awaiting approval.")
                    }
                    (None, _) => "The journal is submitted.".into(),
                });
            }
            Err(Failure::Refused(refusal)) => {
                draft.refusal = refusal;
                draft.notice.clear();
            }
            Err(failure) => draft.notice = failure.text("The journal"),
        }
        Ok(())
    }

    /// Opens the dialog for a new journal.
    async fn open_new(&mut self, scope: &Scope) -> Result<()> {
        if !self.pages.allowed(scope, SUBMIT_JOURNALS).await? {
            return Ok(());
        }
        match self.units(scope).await? {
            Ok(units) => {
                let today = self.pages.today(scope).await?.to_string();
                self.draft = Some(Draft::new(today, units));
            }
            Err(text) => self.flash.error(text),
        }
        Ok(())
    }

    /// Opens the dialog prefilled from an earlier journal the address names:
    /// a correction of the person's own waiting journal, or a new journal
    /// started from a rejected one. The journal is read through its contract,
    /// so only what the person may see is copied.
    async fn open_from(&mut self, scope: &Scope, opening: Opening) -> Result<()> {
        if !self.pages.allowed(scope, SUBMIT_JOURNALS).await? {
            return Ok(());
        }
        let units = match self.units(scope).await? {
            Ok(units) => units,
            Err(text) => {
                self.flash.error(text);
                return Ok(());
            }
        };
        let journal: view::Journal = match self
            .pages
            .read(
                scope,
                "finance.ledger.journal.get",
                json!({"journal_id": opening.id}),
            )
            .await?
        {
            Read::Found(value) => match view::parse(value) {
                Some(journal) => journal,
                None => {
                    self.flash.error("The journal could not be read.");
                    return Ok(());
                }
            },
            Read::Denied => {
                self.flash.error(super::DENIED);
                return Ok(());
            }
            _ => {
                self.flash.error("That journal does not exist.");
                return Ok(());
            }
        };
        let allowed = if opening.replaces {
            journal.status == "submitted" && journal.submitted_by_user_id == scope.user()
        } else {
            journal.status == "rejected" || journal.status == "withdrawn"
        };
        if !allowed {
            self.flash.error(if opening.replaces {
                "Only the person who submitted a journal can correct it, and only while it waits for approval."
            } else {
                "Only a rejected or withdrawn journal can be started again."
            });
            return Ok(());
        }
        if journal.opening {
            self.opening_dialog = Some(super::opening_dialog::OpeningDialog::new(
                self.pages.clone(),
                opening.replaces.then_some(journal.id),
            ));
            return Ok(());
        }
        if journal.lines.len() > MAX_LINES {
            self.flash.error(format!(
                "Journal {} has more than {MAX_LINES} lines, more than the dialog holds.",
                journal.journal_number
            ));
            return Ok(());
        }
        let accounts = self.pages.chart(scope).await?.unwrap_or_default();
        self.draft = Some(Draft::from_journal(
            &journal,
            units,
            &accounts,
            opening.replaces,
        ));
        Ok(())
    }

    /// The book's minor units, which decide the amounts the dialog can
    /// balance against; no book, no dialog.
    async fn units(&self, scope: &Scope) -> Result<std::result::Result<crate::MinorUnits, String>> {
        Ok(match self.pages.book(scope).await? {
            Ok(state) => state
                .book
                .and_then(|book| crate::MinorUnits::new(i64::from(book.minor_units)))
                .ok_or_else(|| BOOK_UNREADABLE.to_owned()),
            Err(Read::Denied) => Err(super::DENIED.to_owned()),
            Err(Read::Refused(text)) => Err(text),
            Err(_) => Err(BOOK_UNREADABLE.to_owned()),
        })
    }
}

pub(super) fn status_badge(status: &str) -> Result<Html> {
    render(match status {
        "approved" => Badge::status(BadgeKind::Success, "Posted"),
        "rejected" => Badge::status(BadgeKind::Danger, "Rejected"),
        "withdrawn" => Badge::status(BadgeKind::Neutral, "Withdrawn"),
        _ => Badge::status(BadgeKind::Warning, "Submitted"),
    })
}

#[derive(Deserialize)]
struct ListRow {
    #[serde(default)]
    recorded_days: Option<i32>,
    #[serde(default)]
    recording: String,
    #[serde(default)]
    review: String,
    id: i64,
    kind: String,
    stamp: String,
    number: Option<i64>,
    request: Option<i64>,
    date: String,
    description: String,
    source: String,
    status: String,
    amount: String,
}

#[derive(Template)]
#[template(
    source = r#"<div id="ledger-journals-period-region" data-hook="BrowserTimeZone" data-timezone-request="{{ timezone_request }}" class="flex flex-wrap items-start gap-3">{{ toolbar }}{{ account }}{{ period }}{{ clear }}</div>"#,
    ext = "html"
)]
struct FilterRegion<'a> {
    timezone_request: &'a str,
    toolbar: Html,
    account: Html,
    period: Html,
    clear: Html,
}
#[derive(Template)]
#[template(
    source = r#"<form id="{{ id }}" data-event="{{ event }}" data-change class="ui-compact-controls w-full sm:w-fit sm:min-w-36">{{ field }}</form>"#,
    ext = "html"
)]
struct AllDates {
    id: String,
    event: String,
    field: Html,
}

#[derive(Template)]
#[template(
    source = r#"<div id="ledger-journal-line-{{ n }}" class="mt-3 grid grid-cols-1 items-start gap-2 sm:grid-cols-4"><div class="sm:col-span-2">{{ account|safe }}</div>{{ debit|safe }}{{ credit|safe }}<div class="sm:col-span-2">{{ note|safe }}</div><div class="flex flex-wrap items-center gap-2">{{ fill|safe }}{{ remove|safe }}</div></div>"#,
    ext = "html"
)]
struct LineRow {
    n: usize,
    account: Html,
    debit: Html,
    credit: Html,
    note: Html,
    fill: Html,
    remove: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% match replace %}{% when Some with (amount) %}Replace with {{ amount }}<span class="sr-only"> on line {{ n }}</span>{% when None %}Fill difference<span class="sr-only"> on line {{ n }}</span>{% endmatch %}"#,
    ext = "html"
)]
struct FillLabel {
    n: usize,
    replace: Option<String>,
}

#[derive(Template)]
#[template(
    source = r#"<p id="ledger-journal-totals" class="mt-3 text-sm text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
struct Totals {
    text: String,
}

#[derive(Template)]
#[template(
    source = r#"<div class="p-3 text-sm"><a id="ledger-journals-later" href="{{ href }}" data-patch>Later journals</a></div>"#,
    ext = "html"
)]
struct LaterLink {
    href: String,
}

impl LiveState<Scope, Error> for Journals {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let Some(opening) = self.opening.take() {
                self.open_from(r.scope, opening).await?;
            }
            match r.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.window = Window::parse(values);
                }
                LiveInput::Event(value) => {
                    let kind = event_type(value)?;
                    let (clock, _) = self.clock(r.scope).await?;
                    if kind == clear_event() {
                        self.window.clear();
                        self.list.filters.insert("after".into(), String::new());
                    } else if let Some(outcome) = self.period.event(kind, value, &clock) {
                        if let Outcome::Applied(period) = outcome {
                            self.window.apply(period);
                            self.period.set(period, &clock);
                            self.list.filters.insert("after".into(), String::new());
                        }
                    } else {
                        match kind {
                            "filters" => {
                                let mut posted = params::form_values(value, FILTERS_FORM);
                                posted.insert("after".into(), String::new());
                                self.list.apply_filters(&posted);
                            }
                            "grid" => {
                                if let Some(dialog) = &mut self.opening_dialog {
                                    dialog.event(r.scope, r.credential, value).await?;
                                } else if let ColumnsOutcome::Sort(_) =
                                    self.columns.handle(&params::string_values(value))
                                {
                                }
                            }
                            "new_opening" => {
                                if self.pages.allowed(r.scope, SUBMIT_JOURNALS).await? {
                                    self.opening_dialog =
                                        Some(super::opening_dialog::OpeningDialog::new(
                                            self.pages.clone(),
                                            None,
                                        ));
                                }
                            }
                            "close_opening" => self.opening_dialog = None,
                            "preview_opening" | "submit_opening" | "cancel_opening" => {
                                if let Some(dialog) = &mut self.opening_dialog {
                                    dialog.event(r.scope, r.credential, value).await?;
                                    self.destination = dialog.take_destination();
                                    if self.destination.is_some() {
                                        self.opening_dialog = None;
                                    }
                                }
                            }
                            "new" => self.open_new(r.scope).await?,
                            "close_modal" => self.draft = None,
                            "validate" => {
                                if let Some(draft) = &mut self.draft {
                                    let before = draft.lines.clone();
                                    draft.read(value);
                                    draft.suggest(&before);
                                }
                            }
                            "fill_difference" => {
                                if let Some(draft) = &mut self.draft {
                                    let index = value["index"]
                                        .as_str()
                                        .and_then(|i| i.parse::<usize>().ok());
                                    if let Some(index) = index {
                                        draft.fill_difference(index);
                                    }
                                }
                            }
                            "add_line" => {
                                if let Some(draft) = &mut self.draft {
                                    draft.replacing = None;
                                    if draft.lines.len() < MAX_LINES {
                                        draft.lines.push(LineDraft::default());
                                        draft.suggest(&[]);
                                    }
                                }
                            }
                            "remove_line" => {
                                if let Some(draft) = &mut self.draft {
                                    let index = value["index"]
                                        .as_str()
                                        .and_then(|i| i.parse::<usize>().ok());
                                    if let Some(index) = index
                                        .filter(|i| *i < draft.lines.len() && draft.lines.len() > 2)
                                    {
                                        draft.lines.remove(index);
                                        draft.replacing = None;
                                        draft.refusal = Refusal::default();
                                        draft.suggest(&[]);
                                    }
                                }
                            }
                            "submit_journal" | "replace_journal" => self.submit(&r).await?,
                            "browser_timezone" => {
                                self.browser_zone = value["timezone"]
                                    .as_str()
                                    .filter(|zone| bilibi_datetime::valid_timezone(zone))
                                    .map(str::to_owned);
                                self.timezone_request.clear();
                            }
                            "clear-flash" => self.flash.dismiss(value),
                            _ => return Err(Error::Invalid),
                        }
                    }
                }
            }
            self.view(r.scope).await
        })
    }
}
