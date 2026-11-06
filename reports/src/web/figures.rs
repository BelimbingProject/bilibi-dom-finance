//! Presentation of Bilimbi Reports.Web.Figures. Never calculates ledger totals.
use super::{render, PATH};
use crate::{
    AccountActivity, BalanceSheet, CashPosition, Column, Group, Header, Overview, ProfitAndLoss,
    ReportsError, Section,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_finance_ledger::{AccountType, MinorUnits, Money, People};
use bilibi_ui::{
    params, Alert, BackLink, BuiltinColumn, Button, Card, EmptyState, Fact, Facts, FlexRow,
    FlexTable, Html, ListColumns, ListState, RecordLink, SectionHeading, StatItem, StatStrip,
    StatusKind, TableEmpty,
};
use std::collections::HashMap;

pub(super) enum Report {
    Overview(Overview),
    Profit(ProfitAndLoss),
    Balance(BalanceSheet),
    Cash(CashPosition),
    Activity(AccountActivity),
}
impl Report {
    pub(super) fn header(&self) -> Option<&Header> {
        match self {
            Self::Overview(r) => Some(&r.header),
            Self::Profit(r) => Some(&r.header),
            Self::Balance(r) => Some(&r.header),
            Self::Cash(r) => Some(&r.header),
            Self::Activity(_) => None,
        }
    }
    pub(super) fn render(
        &self,
        columns: &mut HashMap<&'static str, ListColumns>,
        list: &ListState,
        query: &HashMap<String, String>,
        display: &DisplayContext,
        names: &People,
        framed: bool,
    ) -> Result<Vec<Html>> {
        let mut parts = Vec::new();
        let unmatched = match self {
            Self::Profit(r) => &r.unmatched_codes[..],
            Self::Balance(r) => &r.unmatched_codes,
            Self::Cash(r) => &r.unmatched_codes,
            _ => &[],
        };
        if !unmatched.is_empty() {
            parts.push(render(Alert {
                kind: StatusKind::Warning,
                class: "mb-3",
                body: Html::text(&format!(
                    "The report sections name account codes the chart does not have: {}.",
                    unmatched.join(", ")
                )),
                ..Alert::default()
            })?);
        }
        match self {
            Self::Overview(r) => parts.extend(overview(r, columns, list, query)?),
            Self::Activity(r) => parts.extend(activity(r, columns, list, query, display, names)?),
            Self::Profit(r) => {
                let mut rows = Vec::new();
                for g in &r.groups {
                    group(&mut rows, "profit-and-loss", g, &r.header, framed)?;
                }
                rows.push(total(
                    "profit-and-loss-net-income",
                    "Net income",
                    &r.net_income,
                    r.header.minor_units,
                )?);
                let empty = r.groups.iter().all(|g| g.sections.is_empty());
                parts.push(statement(
                    "profit-and-loss",
                    &r.header,
                    rows,
                    empty.then_some((
                        "No income or expenses",
                        "No entry in these periods posts to an income or expense account.",
                    )),
                    columns,
                    list,
                )?);
            }
            Self::Balance(r) => {
                let mut rows = Vec::new();
                for g in &r.groups {
                    // Earnings are already included in the facade's equity total.
                    let mut grouped = Vec::new();
                    group(&mut grouped, "balance-sheet", g, &r.header, framed)?;
                    if g.account_type == AccountType::Equity {
                        let last = grouped.pop().expect("group total");
                        grouped.push(extra(
                            "balance-sheet-earnings",
                            "Income less expenses not yet closed",
                            &r.earnings,
                            r.header.minor_units,
                        )?);
                        grouped.push(last);
                    }
                    rows.extend(grouped);
                }
                rows.push(total(
                    "balance-sheet-total-liabilities-and-equity",
                    "Total liabilities and equity",
                    &r.total_liabilities_and_equity,
                    r.header.minor_units,
                )?);
                parts.push(statement(
                    "balance-sheet",
                    &r.header,
                    rows,
                    None,
                    columns,
                    list,
                )?);
            }
            Self::Cash(r) => {
                let mut rows = Vec::new();
                sections(
                    &mut rows,
                    "cash-position",
                    "cash",
                    &r.sections,
                    &r.header,
                    framed,
                )?;
                rows.push(total(
                    "cash-position-cash-total",
                    "Total cash",
                    &r.totals,
                    r.header.minor_units,
                )?);
                parts.push(statement("cash-position",&r.header,rows,r.sections.is_empty().then_some(("No cash accounts configured","Add a bank account in Bank reconciliation, or choose accounts in Report sections.")),columns,list)?);
            }
        }
        Ok(parts)
    }
}

pub(super) fn header(
    report: Option<&Report>,
    activity: bool,
    pinned: bool,
    _query: &HashMap<String, String>,
    auditor_pack: Option<&str>,
) -> Result<Html> {
    let mut subtitle = Html::EMPTY;
    let mut actions = Html::EMPTY;
    let mut title = "Reports".to_owned();
    let mut back = Html::EMPTY;
    let mut account_id = None;
    if activity {
        title = "Account entries".into();
        back = render(BackLink {
            href: PATH,
            title: "Reports",
            ..BackLink::default()
        })?;
        if let Some(Report::Activity(a)) = report {
            title = format!("{} · {}", a.account.name, a.account.code);
            account_id = Some(a.account.id.to_string());
            subtitle.push(&Html::text(&format!(
                "{} · {}",
                type_label(a.account.account_type),
                window(a)
            )));
            if _query.get("closing").is_some_and(|v| v == "excluded") {
                subtitle.push(&text(
                    "activity-closing",
                    " · year-end closing entries left out",
                    false,
                )?);
            }
        }
    } else if let Some(header) = report.and_then(Report::header) {
        subtitle = text("report-currency", &header.currency_code, false)?;
        if pinned {
            subtitle.push(&Html::text(" · figures frozen"));
        }
        // The shared button has no patch-link variant. A page event changes
        // only URL/read state, retaining the mounted page just as patch does.
        actions = render(Button {
            id: "export-auditor-pack",
            event: "export_auditor_pack",
            button_type: "button",
            body: Html::text("Generate auditor pack"),
            ..Button::default()
        })?;
        if let Some(artifact) = auditor_pack {
            actions.push(&render(Button {
                id: "download-auditor-pack",
                href: &format!("/finance/reports/auditor-packs/{artifact}"),
                body: Html::text("Download auditor pack"),
                ..Button::default()
            })?);
        }
        actions.push(&render(Button {
            id: if pinned { "unpin-report" } else { "pin-report" },
            event: if pinned { "unpin" } else { "pin" },
            name: "through",
            value: &header.through_entry_number.to_string(),
            button_type: "button",
            body: Html::text(if pinned {
                "Show latest figures"
            } else {
                "Freeze these figures"
            }),
            ..Button::default()
        })?);
    }
    // Bilimbi shows only the back link above a refused account's empty state.
    let missing = activity && !matches!(report, Some(Report::Activity(_)));
    let title_actions = Html::EMPTY;
    // Back is the header's last action, as on every record page; it stands
    // alone only when there is no header to hold it.
    let back = if missing {
        back
    } else {
        actions.push(&back);
        Html::EMPTY
    };
    render(HeaderView {
        title: &title,
        subtitle,
        title_actions,
        actions,
        back,
        account_id: account_id.as_deref(),
        missing,
    })
}

pub(super) fn notice() -> Result<Html> {
    render(Alert {
        id: "management-report-notice",
        class: "mb-3",
        body: Html::text("For management use, not statutory statements."),
        ..Alert::default()
    })
}
pub(super) fn live_warning() -> Result<Html> {
    render(Alert {
        id: "report-live-warning",
        kind: StatusKind::Warning,
        body: Html::text(
            "Posting notices are unavailable. Latest figures refresh periodically; reload to reconnect live updates.",
        ),
        ..Alert::default()
    })
}
pub(super) fn refusal(error: &ReportsError, activity: bool) -> Result<Html> {
    let state = match error {
        ReportsError::Unauthorized => EmptyState::forbidden("view the ledger"),
        _ if activity => EmptyState::missing(
            "Account not found",
            "This company's chart has no such account, or the link's options are not valid.",
        ),
        ReportsError::BookNotOpen => EmptyState::missing(
            "The ledger is not open",
            "Reports read the company's book, which has not been opened yet.",
        ),
        _ => EmptyState::missing(
            "These figures cannot be shown",
            "The company or the report options could not be read. Choose another period or reload the page.",
        ),
    };
    render(state.id(if activity {
        "activity-refusal"
    } else {
        "report-refusal"
    }))
}

fn overview(
    r: &Overview,
    columns: &mut HashMap<&'static str, ListColumns>,
    list: &ListState,
    query: &HashMap<String, String>,
) -> Result<Vec<Html>> {
    let mut rows = Vec::new();
    let figures = [
        ("income", "Income", &r.income),
        ("expense", "Expenses", &r.expense),
        ("net-income", "Net income", &r.net_income),
        ("assets", "Assets", &r.assets),
        ("liabilities", "Liabilities", &r.liabilities),
        ("equity", "Equity", &r.equity),
        ("cash", "Cash", &r.cash),
    ];
    for (id, label, values) in figures {
        let mut row = row(&format!("overview-figure-{id}"));
        row = row.slot("account", Html::text(label));
        for (i, value) in values.iter().enumerate() {
            row = row.slot(
                &format!("v{i}"),
                text("", &amount(*value, r.header.minor_units), false)?,
            );
        }
        row = row.slot(
            "change",
            text(
                "",
                &amount(values[0] - values[1], r.header.minor_units),
                false,
            )?,
        );
        rows.push(row);
    }
    let mut strips = Vec::new();
    for (id, title, view, range) in [
        (
            "overview-profit-and-loss",
            "Profit and loss",
            "profit-and-loss",
            0..3,
        ),
        (
            "overview-position",
            "Financial position",
            "balance-sheet",
            3..6,
        ),
        ("overview-cash", "Cash position", "cash-position", 6..7),
    ] {
        let mut q = query.clone();
        q.insert("view".into(), view.into());
        strips.push(render(StatStrip {
            id,
            title,
            href: &params::address(PATH, &q),
            items: range
                .map(|i| {
                    StatItem::number(figures[i].1, amount(figures[i].2[0], r.header.minor_units))
                })
                .collect(),
        })?);
    }
    let mut labels = vec![("account".into(), "Figure".into())];
    labels.extend(
        r.header
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| (format!("v{i}"), column_label(c))),
    );
    labels.push(("change".into(), "Change".into()));
    let table = table(
        "overview-figures",
        &labels,
        rows,
        "Headline figures against the previous period",
        None,
        columns,
        list,
    )?;
    Ok(vec![
        render(OverviewView { strips })?,
        render(Card {
            id: "overview-comparison",
            class: "mt-4",
            inner_class: "p-0",
            body: table,
            ..Card::default()
        })?,
    ])
}

fn group(rows: &mut Vec<FlexRow>, id: &str, g: &Group, h: &Header, framed: bool) -> Result<()> {
    let key = g.account_type.as_str();
    rows.push(heading(
        &format!("{id}-{key}"),
        type_label(g.account_type),
        h.columns.len(),
    )?);
    sections(rows, id, key, &g.sections, h, framed)?;
    rows.push(total(
        &format!("{id}-{key}-total"),
        &format!("Total {}", type_label(g.account_type).to_lowercase()),
        &g.totals,
        h.minor_units,
    )?);
    Ok(())
}
fn sections(
    rows: &mut Vec<FlexRow>,
    id: &str,
    group: &str,
    sections: &[Section],
    h: &Header,
    framed: bool,
) -> Result<()> {
    for (index, section) in sections.iter().enumerate() {
        if let Some(name) = &section.name {
            rows.push(heading(
                &format!("{id}-{group}-section-{index}"),
                name,
                h.columns.len(),
            )?);
        }
        for a in &section.rows {
            let dom_id = format!("{id}-account-{}", a.account_id);
            let mut row = row(&dom_id).slot(
                "account",
                render(AccountName {
                    name: &a.name,
                    code: &a.code,
                })?,
            );
            for (i, value) in a.amounts.iter().enumerate() {
                let mut q = HashMap::from([("through".into(), h.through_entry_number.to_string())]);
                match h.columns[i] {
                    Column::Period { from, to } => {
                        q.insert("from".into(), from.to_string());
                        q.insert("to".into(), to.to_string());
                        q.insert("closing".into(), "excluded".into());
                    }
                    Column::AsOf { as_of } => {
                        q.insert("to".into(), as_of.to_string());
                    }
                }
                let link = render(RecordLink {
                    workspace: framed,
                    kind: "finance/ledger",
                    record_id: &a.account_id.to_string(),
                    href: &params::address(&format!("{PATH}/accounts/{}", a.account_id), &q),
                    class: "tabular-nums text-link hover:underline",
                    body: text(
                        &format!("{dom_id}-{i}"),
                        &amount(*value, h.minor_units),
                        false,
                    )?,
                })?;
                row = row.slot(&format!("v{i}"), link);
            }
            rows.push(row);
        }
        if let Some(name) = &section.name {
            rows.push(total(
                &format!("{id}-{group}-section-{index}-total"),
                &format!("Total {name}"),
                &section.totals,
                h.minor_units,
            )?);
        }
    }
    Ok(())
}
fn row(id: &str) -> FlexRow {
    let mut row = FlexRow::new(id);
    row.dom_id = Some(id.into());
    row
}
fn heading(id: &str, label: &str, n: usize) -> Result<FlexRow> {
    let mut row = row(id).slot("account", text("", label, true)?);
    for i in 0..n {
        row = row.slot(&format!("v{i}"), Html::EMPTY);
    }
    Ok(row)
}
fn total(id: &str, label: &str, values: &[Money], units: MinorUnits) -> Result<FlexRow> {
    let mut row = row(id).slot("account", text("", label, true)?);
    for (i, value) in values.iter().enumerate() {
        row = row.slot(&format!("v{i}"), text("", &amount(*value, units), true)?);
    }
    Ok(row)
}

/// A group's extra line (Bilimbi `group.extras`): an italic label and plain
/// amounts, not a bold total.
fn extra(id: &str, label: &str, values: &[Money], units: MinorUnits) -> Result<FlexRow> {
    let mut row = row(id).slot("account", render(Italic { text: label })?);
    for (i, value) in values.iter().enumerate() {
        row = row.slot(&format!("v{i}"), text("", &amount(*value, units), false)?);
    }
    Ok(row)
}
#[derive(Template)]
#[template(source = r#"<em>{{ text }}</em>"#, ext = "html")]
struct Italic<'a> {
    text: &'a str,
}

fn statement(
    id: &'static str,
    h: &Header,
    rows: Vec<FlexRow>,
    empty: Option<(&str, &str)>,
    columns: &mut HashMap<&'static str, ListColumns>,
    list: &ListState,
) -> Result<Html> {
    let title = format!("Report: {}", id.replace('-', " "));
    let mut body = Html::EMPTY;
    if let Some((title, reason)) = empty {
        body.push(&render(EmptyState::missing(title, reason))?);
    } else {
        let mut labels = vec![("account".into(), "Account".into())];
        labels.extend(
            h.columns
                .iter()
                .enumerate()
                .map(|(i, c)| (format!("v{i}"), column_label(c))),
        );
        body.push(&table(id, &labels, rows, &title, None, columns, list)?);
    }
    // The unframed FlexTable notch rises above its card. Keep the label
    // outside that card so the shared component can reserve the notch's gap.
    let mut statement = render(SectionHeading {
        id: &format!("{id}-label"),
        title: &title,
        ..SectionHeading::default()
    })?;
    statement.push(&render(Card {
        id: &format!("{id}-card"),
        inner_class: "p-0",
        body,
        ..Card::default()
    })?);
    Ok(statement)
}

fn table(
    id: &'static str,
    labels: &[(String, String)],
    rows: Vec<FlexRow>,
    caption: &str,
    empty: Option<EmptyState>,
    columns: &mut HashMap<&'static str, ListColumns>,
    list: &ListState,
) -> Result<Html> {
    // Different comparison widths have different columns; each keeps its own arrangement.
    let key = match (id, labels.len()) {
        ("profit-and-loss", 2) => "profit-and-loss-single",
        ("balance-sheet", 2) => "balance-sheet-single",
        ("cash-position", 2) => "cash-position-single",
        _ => id,
    };
    if !columns.contains_key(key) {
        let builtins = labels
            .iter()
            .map(|(key, label)| {
                let c = BuiltinColumn::new(key, label);
                if key == "account"
                    || ["date", "description", "reference", "posted"].contains(&key.as_str())
                {
                    c
                } else {
                    c.right()
                }
            })
            .collect();
        columns.insert(
            key,
            ListColumns::mount(id, builtins).map_err(|_| Error::Internal)?,
        );
    }
    let host = &columns[key];
    let mut flex = host.flex_columns();
    for c in &mut flex {
        if let Some((_, label)) = labels.iter().find(|(key, _)| *key == c.id) {
            c.label = label.clone();
            c.short_label = label.clone();
        }
    }
    let mut t = FlexTable::new(id, flex, rows).sorted(list.sort_by.as_deref(), list.sort_dir);
    let event = format!("grid:{key}");
    t.event = &event;
    t.caption = caption;
    t.framed = false;
    t.zoom = host.zoom;
    t.add_query = &host.add_query;
    t.suggestions = host.flex_suggestions();
    t.empty = empty.map(TableEmpty::State);
    render(t)
}

fn activity(
    a: &AccountActivity,
    columns: &mut HashMap<&'static str, ListColumns>,
    list: &ListState,
    query: &HashMap<String, String>,
    display: &DisplayContext,
    names: &People,
) -> Result<Vec<Html>> {
    let units = a.minor_units;
    let totals = render(Facts {
        id: "activity-totals",
        items: vec![
            Fact {
                title: "Debits",
                id: "activity-debits",
                value: Html::text(&amount(a.debits, units)),
            },
            Fact {
                title: "Credits",
                id: "activity-credits",
                value: Html::text(&amount(a.credits, units)),
            },
            Fact {
                title: "Debits less credits",
                id: "activity-net",
                value: Html::text(&format!(
                    "{} {}",
                    amount(a.debits - a.credits, units),
                    a.currency_code
                )),
            },
        ],
    })?;
    let compact = columns
        .get("activity-entries")
        .is_some_and(|c| c.mode() == bilibi_ui::RowMode::Compact);
    let mut rows = Vec::new();
    for own in &a.entries {
        let e = &own.entry;
        let mut desc = e.description.clone();
        if e.reverses_entry_id.is_some() {
            desc.push_str(" · reversal");
        }
        if e.reversed_by_entry_id.is_some() {
            desc.push_str(" · reversed");
        }
        let instant = display.render(e.posted_at, Format::DateTime, Precision::Minute);
        let posted = render(Posted {
            id: &format!("activity-entry-{}-posted", e.id),
            instant: bilibi_ui::flex_table::Instant::new(
                &instant.iso,
                &instant.text,
                &instant.company,
                &instant.utc,
                instant.mode.as_str(),
            ),
            by: &names.who(Some(e.posted_by_user_id), e.posted_by_impersonator_id),
            compact,
        })?;
        rows.push(
            row(&format!("activity-entry-{}", e.id))
                .slot(
                    "entry",
                    render(EntryLink {
                        id: e.id,
                        number: e.entry_number,
                    })?,
                )
                .slot("date", Html::text(&e.entry_date.to_string()))
                .slot("description", Html::text(&desc))
                .slot(
                    "reference",
                    Html::text(e.reference.as_deref().unwrap_or("")),
                )
                .slot("debit", text("", &amount(own.debit, units), false)?)
                .slot("credit", text("", &amount(own.credit, units), false)?)
                .slot("posted", posted),
        );
    }
    let labels = [
        ("entry", "Entry"),
        ("date", "Date"),
        ("description", "Description"),
        ("reference", "Reference"),
        ("debit", "Debit"),
        ("credit", "Credit"),
        ("posted", "Posted"),
    ]
    .map(|(k, v)| (k.into(), v.into()));
    let t = table(
        "activity-entries",
        &labels,
        rows,
        "Entries",
        Some(EmptyState::missing(
            "No entries",
            "No entry in this window posts to this account.",
        )),
        columns,
        list,
    )?;
    let mut parts = vec![
        totals,
        render(Card {
            id: "activity-entries-card",
            class: "mt-4",
            inner_class: "p-0",
            body: t,
            ..Card::default()
        })?,
    ];
    if a.more {
        if let Some(last) = a.entries.last() {
            let mut q = HashMap::from([
                ("through".into(), a.through_entry_number.to_string()),
                ("after_date".into(), last.entry.entry_date.to_string()),
                ("after_id".into(), last.entry.id.to_string()),
            ]);
            if let Some(d) = a.from {
                q.insert("from".into(), d.to_string());
            }
            if let Some(d) = a.to {
                q.insert("to".into(), d.to_string());
            }
            if query.get("closing").is_some_and(|v| v == "excluded") {
                q.insert("closing".into(), "excluded".into());
            }
            parts.push(render(Button {
                id: "activity-next",
                href: &params::address(&format!("{PATH}/accounts/{}", a.account.id), &q),
                body: Html::text("Next entries"),
                ..Button::default()
            })?);
        }
    }
    Ok(parts)
}

fn type_label(t: AccountType) -> &'static str {
    match t {
        AccountType::Asset => "Assets",
        AccountType::Liability => "Liabilities",
        AccountType::Equity => "Equity",
        AccountType::Income => "Income",
        AccountType::Expense => "Expenses",
    }
}
fn column_label(c: &Column) -> String {
    match c {
        Column::Period { from, to } => format!("{from} – {to}"),
        Column::AsOf { as_of } => format!("As of {as_of}"),
    }
}
fn window(a: &AccountActivity) -> String {
    match (a.from, a.to) {
        (None, None) => "All entries".into(),
        (None, Some(to)) => format!("Up to {to}"),
        (Some(from), None) => format!("From {from}"),
        (Some(from), Some(to)) => format!("{from} – {to}"),
    }
}
fn amount(m: Money, u: MinorUnits) -> String {
    m.display(u)
}

fn text(id: &str, text: &str, emphasis: bool) -> Result<Html> {
    render(Text { id, text, emphasis })
}
#[derive(Template)]
#[template(path = "text.html")]
struct Text<'a> {
    id: &'a str,
    text: &'a str,
    emphasis: bool,
}
/// An account as a person reads it: the name, then the code muted.
#[derive(Template)]
#[template(
    source = r#"{{ name }} <span class="italic text-ink-muted">{{ code }}</span>"#,
    ext = "html"
)]
struct AccountName<'a> {
    name: &'a str,
    code: &'a str,
}
/// An entry's number, opening the entry in the ledger.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="tabular-nums text-link hover:underline">{{ number }}</a>"#,
    ext = "html"
)]
struct EntryLink {
    id: i64,
    number: i64,
}
#[derive(Template)]
#[template(path = "header.html")]
struct HeaderView<'a> {
    title: &'a str,
    subtitle: Html,
    title_actions: Html,
    actions: Html,
    back: Html,
    account_id: Option<&'a str>,
    missing: bool,
}
#[derive(Template)]
#[template(path = "overview.html")]
struct OverviewView {
    strips: Vec<Html>,
}
#[derive(Template)]
#[template(path = "posted.html")]
struct Posted<'a> {
    id: &'a str,
    instant: bilibi_ui::flex_table::Instant,
    by: &'a str,
    compact: bool,
}
