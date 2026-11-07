//! The "+ Schedule" dialog and its review, both through the import contract.
//! The schedule's details are typed; its rows are pasted (`paste.rs`), so a
//! whole source report is one schedule rather than one row each.
use super::paste::{self, Account, Paste, Row};
use super::*;
use bilibi_finance_ledger::{MinorUnits, Money};

/// How many rows the review lists; it counts and totals all of them.
const REVIEWED: usize = 10;

impl State {
    /// The review of a previewed schedule, confirmed by "Retain schedule".
    pub(super) fn review_modal(&self, review: &Review) -> Result<Html> {
        let facts = &review.facts;
        let text = |key: &str| facts[key].as_str().unwrap_or("").to_owned();
        let mut content = render(Alert {
            body: Html::text(&format!(
                "Review source {} for {} to {}. This retains evidence only and creates no postings or submissions.",
                text("source_reference"),
                text("period_start"),
                text("period_end")
            )),
            ..Alert::default()
        })?;
        let units = facts["minor_units"]
            .as_i64()
            .and_then(MinorUnits::new)
            .ok_or(Error::Internal)?;
        let rows = facts["rows"].as_array().cloned().unwrap_or_default();
        let money = |row: &Value| {
            row["amount"]
                .as_str()
                .and_then(|v| Money::parse(v, units).ok())
                .unwrap_or(Money::ZERO)
        };
        let accounts = self.kind == "comparative" || self.kind == "movement";
        let currency = text("currency_code");
        let total = |side: Option<&str>| {
            let sum = rows
                .iter()
                .filter(|r| side.is_none_or(|s| r["side"] == s))
                .fold(Money::ZERO, |sum, r| sum + money(r));
            format!("{currency} {}", sum.display(units))
        };
        let fact = |title: &'static str, value: String| Fact {
            title,
            value: Html::text(&value),
            ..Fact::default()
        };
        let mut items = vec![
            fact("Source system", text("source_system")),
            fact(
                "Accounting time zone",
                text("accounting_timezone").replace('_', " "),
            ),
            fact("Conversion date", text("conversion_date")),
            fact("Rows", rows.len().to_string()),
        ];
        if accounts {
            items.push(fact("Debits", total(Some("debit"))));
            items.push(fact("Credits", total(Some("credit"))));
        } else {
            items.push(fact("Total", total(None)));
        }
        content.push(&render(Facts {
            id: "source-period-review-facts",
            items,
        })?);
        let mut columns = vec![
            BuiltinColumn::new("source", "Row reference"),
            BuiltinColumn::new("item", if accounts { "Account" } else { "Reference" }),
            BuiltinColumn::new("side", if accounts { "Side" } else { "Authority" }),
        ];
        if !accounts {
            columns.push(BuiltinColumn::new("date", "Date"));
        }
        columns.push(BuiltinColumn::new("amount", "Amount").right());
        let columns = ListColumns::mount("source-period-review-rows", columns)
            .map_err(|_| Error::Internal)?;
        let lines = rows
            .iter()
            .take(REVIEWED)
            .enumerate()
            .map(|(n, r)| {
                let item = r["account_name"]
                    .as_str()
                    .map(|name| format!("{name} · {}", r["account_code"].as_str().unwrap_or("")))
                    .unwrap_or_else(|| r["reference"].as_str().unwrap_or("").to_owned());
                let side = match r["side"].as_str() {
                    Some("debit") => "Debit",
                    Some("credit") => "Credit",
                    _ => r["authority"].as_str().unwrap_or(""),
                };
                FlexRow::new(n.to_string())
                    .cell("source", Cell::text(r["source_id"].as_str().unwrap_or("")))
                    .cell("item", Cell::text(item))
                    .cell("side", Cell::text(side))
                    .cell("date", Cell::text(r["event_date"].as_str().unwrap_or("")))
                    .cell("amount", Cell::text(money(r).display(units)))
            })
            .collect();
        let mut table = FlexTable::new("source-period-review-rows", columns.flex_columns(), lines);
        table.caption = if rows.len() > REVIEWED {
            "The first rows"
        } else {
            "Rows"
        };
        content.push(&render(table)?);
        if rows.len() > REVIEWED {
            content.push(&render(Note {
                text: &format!("And {} more rows.", rows.len() - REVIEWED),
            })?);
        }
        let mut buttons = render(Button {
            button_type: "button",
            event: "cancel_review",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            button_type: "button",
            event: "do_confirm",
            variant: ButtonVariant::Primary,
            body: Html::text("Retain schedule"),
            ..Button::default()
        })?);
        content.push(&render(Footer {
            actions: render(Actions { buttons })?,
        })?);
        render(Modal {
            id: "source-period-review-modal",
            title: "Review source schedule",
            description: "Retain this source schedule for the current company.",
            cancel_event: "cancel_review",
            width: ModalWidth::Wide,
            body: content,
            ..Modal::default()
        })
    }

    /// The book's decimal places and currency, and the chart, read once per dialog.
    pub(super) async fn chart(
        &mut self,
        scope: &Scope,
        company: i64,
    ) -> Result<(MinorUnits, String, Vec<Account>)> {
        if let Some(chart) = &self.chart {
            return Ok(chart.clone());
        }
        let book = self
            .read(
                scope,
                "finance.ledger.book.get",
                json!({"company_id":company}),
            )
            .await?;
        let units = book["book"]["minor_units"]
            .as_i64()
            .and_then(MinorUnits::new)
            .ok_or(Error::Internal)?;
        let currency = book["book"]["currency_code"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let mut accounts = Vec::new();
        if self.kind == "comparative" || self.kind == "movement" {
            let mut input = json!({"company_id":company,"page_size":100});
            loop {
                let page = self
                    .read(scope, "finance.ledger.account.list", input.clone())
                    .await?;
                for account in page["records"].as_array().into_iter().flatten() {
                    accounts.push(Account {
                        id: account["id"].as_i64().unwrap_or_default(),
                        code: account["code"].as_str().unwrap_or("").to_owned(),
                        name: account["name"].as_str().unwrap_or("").to_owned(),
                        is_group: account["is_group"] == true,
                    });
                }
                if page["next_after_code"].is_null() {
                    break;
                }
                input["after_code"] = page["next_after_code"].clone();
            }
        }
        self.chart = Some((units, currency, accounts));
        self.chart.clone().ok_or(Error::Internal)
    }

    /// Takes the dialog's typed details and, when the pasted text changed,
    /// reads it again (the person's column choices then start over).
    pub(super) fn take_details(&mut self, value: &Value) -> Result<()> {
        let mut values = params::form_values_checked(
            value,
            "schedule",
            bilibi_finance_ledger::columns::MAX_BYTES,
        )
        .map_err(Error::refused)?;
        let text = values.remove("rows").unwrap_or_default();
        self.values = values;
        self.dialog_message = None;
        if text.len() > bilibi_finance_ledger::columns::MAX_BYTES {
            self.paste_text.clear();
            self.paste = None;
            self.dialog_message = Some(
                "The pasted text is larger than 128 KiB. Split the source report into smaller schedules."
                    .into(),
            );
        } else if text.trim().is_empty() {
            self.paste_text.clear();
            self.paste = None;
        } else if text != self.paste_text {
            self.paste = Some(Paste::new(
                &text,
                self.kind == "comparative" || self.kind == "movement",
            ));
            self.paste_text = text;
        }
        Ok(())
    }

    /// The import contract's input for the whole pasted schedule, or why it
    /// cannot be previewed yet. Each kept row remembers its row in the paste,
    /// so a refusal of `rows.3` reads as the pasted row it came from.
    pub(super) async fn schedule(
        &mut self,
        scope: &Scope,
        company: i64,
    ) -> Result<std::result::Result<(Value, Vec<usize>), String>> {
        let (units, currency, accounts) = self.chart(scope, company).await?;
        let Some(paste) = &self.paste else {
            return Ok(Err(
                "Paste the schedule's rows from the source report first.".into(),
            ));
        };
        if let Some(refusal) = &paste.refusal {
            return Ok(Err(refusal.clone()));
        }
        let mut rows = Vec::new();
        let mut numbers = Vec::new();
        let mut left = Vec::new();
        for (number, row) in paste.rows(&accounts, units) {
            match row {
                Row::Keep { input, .. } => {
                    rows.push(input);
                    numbers.push(number);
                }
                Row::Left(reason) => left.push(format!("row {number}: {reason}")),
                Row::Skipped => {}
            }
        }
        if !left.is_empty() {
            let shown: Vec<String> = left.iter().take(3).cloned().collect();
            return Ok(Err(format!(
                "{} left out ({}{}). Correct the columns or the pasted rows, then preview again, so the schedule keeps every row of the source report.",
                if left.len() == 1 {
                    "1 row is".to_owned()
                } else {
                    format!("{} rows are", left.len())
                },
                shown.join("; "),
                if left.len() > 3 { "; …" } else { "" }
            )));
        }
        if rows.is_empty() {
            return Ok(Err(
                "No pasted row has an amount in the chosen columns.".into()
            ));
        }
        if rows.len() > paste::MOST {
            return Ok(Err(format!(
                "A schedule takes at most {} rows. Split the source report into schedules of {} rows or fewer.",
                paste::MOST,
                paste::MOST
            )));
        }
        let get = |key: &str| self.values.get(key).cloned().unwrap_or_default();
        Ok(Ok((
            json!({"company_id":company,"kind":self.kind,"source_reference":get("source_reference"),"source_system":get("source_system"),"currency_code":currency,"accounting_timezone":get("accounting_timezone"),"period_start":get("period_start"),"period_end":get("period_end"),"rows":rows}),
            numbers,
        )))
    }

    pub(super) async fn form(&mut self, scope: &Scope, company: i64) -> Result<Html> {
        let (units, currency, accounts) = self.chart(scope, company).await?;
        let mut fields = Html::EMPTY;
        for (key, label, kind, hint) in [
            (
                "source_reference",
                "Source reference",
                InputKind::Text,
                "A permanent reference for this schedule.",
            ),
            (
                "source_system",
                "Source system",
                InputKind::Text,
                "Name the system or archive.",
            ),
            ("period_start", "Period starts", InputKind::Date, ""),
            ("period_end", "Period ends", InputKind::Date, ""),
        ] {
            fields.push(&render(
                Field::in_form("schedule", key)
                    .label(label)
                    .kind(kind)
                    .value(self.values.get(key).cloned().unwrap_or_default())
                    .hint(hint)
                    .errors(self.errors.get(key).cloned().unwrap_or_default())
                    .attr("data-debounce", "400")
                    .required(),
            )?);
        }
        // A coded choice: search the IANA zones by city or region.
        fields.push(&render(
            Combobox::new(
                "schedule-accounting_timezone",
                "schedule[accounting_timezone]",
            )
            .label("Accounting time zone *")
            .aria_label("Accounting time zone")
            .placeholder("Search time zones")
            .options(
                bilibi_datetime::timezones()
                    .into_iter()
                    .map(|zone| Choice::new(zone.replace('_', " "), zone))
                    .collect(),
            )
            .value(
                self.values
                    .get("accounting_timezone")
                    .cloned()
                    .unwrap_or_default(),
            )
            .errors(
                self.errors
                    .get("accounting_timezone")
                    .cloned()
                    .unwrap_or_default(),
            ),
        )?);
        let hint = if self.kind == "comparative" || self.kind == "movement" {
            format!("Copy the source report's rows, with or without its heading row: each account with its debit and credit, or one amount, in {currency}. You then check which column is which.")
        } else {
            format!("Copy the source report's rows, with or without its heading row: each filing or payment's reference, authority, date and amount in {currency}. You then check which column is which.")
        };
        fields.push(&render(
            Field::in_form("schedule", "rows")
                .id("schedule-rows")
                .label("Rows, pasted from a spreadsheet")
                .kind(InputKind::Textarea)
                .value(&self.paste_text)
                .hint(&hint)
                .attr("rows", "4")
                .attr("data-debounce", "500")
                .required(),
        )?);
        let mut body = render(Form {
            id: "source-period-form",
            event: "schedule_change",
            change: true,
            class: "mt-4",
            body: fields,
            actions: Html::EMPTY,
        })?;
        if let Some(paste) = &self.paste {
            body.push(&paste.view(&accounts, units)?);
        }
        if let Some(message) = &self.dialog_message {
            body.push(&render(Inset {
                alert: render(Alert {
                    id: "source-period-dialog-alert",
                    kind: StatusKind::Error,
                    body: Html::text(message),
                    ..Alert::default()
                })?,
            })?);
        }
        let mut buttons = render(Button {
            button_type: "button",
            event: "close_schedule",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "source-period-preview",
            button_type: "button",
            event: "preview",
            variant: ButtonVariant::Primary,
            body: Html::text("Preview schedule"),
            ..Button::default()
        })?);
        body.push(&render(Footer {
            actions: render(Actions { buttons })?,
        })?);
        Ok(body)
    }
}

/// A line under a table.
#[derive(Template)]
#[template(
    source = r#"<p class="mt-2 text-sm text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
struct Note<'a> {
    text: &'a str,
}

/// An alert with the dialog's own spacing above it.
#[derive(Template)]
#[template(source = r#"<div class="mt-4">{{ alert|safe }}</div>"#, ext = "html")]
struct Inset {
    alert: Html,
}

/// The dialog's actions, below the column step.
#[derive(Template)]
#[template(source = r#"<div class="mt-6">{{ actions|safe }}</div>"#, ext = "html")]
struct Footer {
    actions: Html,
}
