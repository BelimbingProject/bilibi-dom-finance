//! The "+ Schedule" dialog and its review, both through the import contract.
use super::*;

impl State {
    /// The review of a previewed schedule, confirmed by "Retain schedule".
    pub(super) fn review_modal(&self, review: &Review) -> Result<Html> {
        let mut content=render(Alert {body:Html::text(&format!("Review source {} for {} to {}. This retains evidence only and creates no postings or submissions.",review.facts["source_reference"].as_str().unwrap_or(""),review.facts["period_start"].as_str().unwrap_or(""),review.facts["period_end"].as_str().unwrap_or(""))),..Alert::default()})?;
        let fact = |title: &'static str, value: String| Fact {
            title,
            value: Html::text(&value),
            ..Fact::default()
        };
        let row = &review.facts["rows"][0];
        let units = review.facts["minor_units"]
            .as_i64()
            .and_then(bilibi_finance_ledger::MinorUnits::new)
            .ok_or(Error::Internal)?;
        let amount = row["amount"]
            .as_str()
            .and_then(|v| bilibi_finance_ledger::Money::parse(v, units).ok())
            .map(|v| v.display(units))
            .unwrap_or_default();
        let item = row["account_name"]
            .as_str()
            .map(|name| format!("{} · {}", name, row["account_code"].as_str().unwrap_or("")))
            .unwrap_or_else(|| row["reference"].as_str().unwrap_or("").to_owned());
        content.push(&render(Facts {
            id: "source-period-review-facts",
            items: vec![
                fact(
                    "Source system",
                    review.facts["source_system"].as_str().unwrap_or("").into(),
                ),
                fact(
                    "Accounting time zone",
                    review.facts["accounting_timezone"]
                        .as_str()
                        .unwrap_or("")
                        .into(),
                ),
                fact(
                    "Conversion date",
                    review.facts["conversion_date"]
                        .as_str()
                        .unwrap_or("")
                        .into(),
                ),
                fact("Source row", row["source_id"].as_str().unwrap_or("").into()),
                fact("Account or reference", item),
                fact(
                    "Side or authority",
                    row["side"]
                        .as_str()
                        .or_else(|| row["authority"].as_str())
                        .unwrap_or("")
                        .into(),
                ),
                fact(
                    "Filing or remittance date",
                    row["event_date"].as_str().unwrap_or("").into(),
                ),
                fact(
                    "Amount",
                    format!(
                        "{} {}",
                        review.facts["currency_code"].as_str().unwrap_or(""),
                        amount
                    ),
                ),
            ],
        })?);
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
        content.push(&render(Actions { buttons })?);
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
    pub(super) async fn form(&self, scope: &Scope, company: i64) -> Result<Html> {
        let book = self
            .read(
                scope,
                "finance.ledger.book.get",
                json!({"company_id":company}),
            )
            .await?;
        let currency = book["book"]["currency_code"].as_str().unwrap_or("");
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
            (
                "source_id",
                "Row in the source report",
                InputKind::Text,
                "The row number or reference used in the source report.",
            ),
            (
                "amount",
                "Amount",
                InputKind::Text,
                "Use the book currency and decimal places.",
            ),
        ] {
            let field_path = if ["source_id", "amount"].contains(&key) {
                format!("rows.0.{key}")
            } else {
                key.into()
            };
            fields.push(&render(
                Field::in_form("schedule", key)
                    .label(label)
                    .kind(kind)
                    .value(self.values.get(key).cloned().unwrap_or_default())
                    .hint(hint)
                    .errors(self.errors.get(&field_path).cloned().unwrap_or_default())
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
        if self.kind == "comparative" || self.kind == "movement" {
            let mut options = Vec::new();
            let mut input = json!({"company_id":company,"page_size":100});
            loop {
                let page = self
                    .read(scope, "finance.ledger.account.list", input.clone())
                    .await?;
                for account in page["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|a| a["is_group"] == false)
                {
                    options.push(
                        Choice::new(
                            account["name"].as_str().unwrap_or(""),
                            account["id"].to_string(),
                        )
                        .detail(account["code"].as_str().unwrap_or("")),
                    );
                }
                if page["next_after_code"].is_null() {
                    break;
                }
                input["after_code"] = page["next_after_code"].clone();
            }
            fields.push(&render(
                Combobox::new("schedule-account_id", "schedule[account_id]")
                    .label("Account *")
                    .aria_label("Account")
                    .options(options)
                    .value(self.values.get("account_id").cloned().unwrap_or_default())
                    .errors(
                        self.errors
                            .get("rows.0.account_id")
                            .cloned()
                            .unwrap_or_default(),
                    ),
            )?);
            fields.push(&render(
                Field::in_form("schedule", "side")
                    .label("Side")
                    .kind(InputKind::Select)
                    .options(vec![
                        Choice::new("Debit", "debit"),
                        Choice::new("Credit", "credit"),
                    ])
                    .value(self.values.get("side").cloned().unwrap_or_default())
                    .errors(self.errors.get("rows.0.side").cloned().unwrap_or_default())
                    .required(),
            )?);
        } else {
            for (key, label, kind) in [
                ("authority", "Authority", InputKind::Text),
                (
                    "reference",
                    "Filing or remittance reference",
                    InputKind::Text,
                ),
                ("event_date", "Filing or remittance date", InputKind::Date),
            ] {
                fields.push(&render(
                    Field::in_form("schedule", key)
                        .label(label)
                        .kind(kind)
                        .value(self.values.get(key).cloned().unwrap_or_default())
                        .errors(
                            self.errors
                                .get(&format!("rows.0.{key}"))
                                .cloned()
                                .unwrap_or_default(),
                        )
                        .required(),
                )?);
            }
        }
        fields.push(&render(Alert {
            id: "source-period-currency",
            body: Html::text(&format!(
                "Amounts in {currency}. Add one row for each schedule."
            )),
            ..Alert::default()
        })?);
        let mut buttons = render(Button {
            button_type: "button",
            event: "close_schedule",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            button_type: "submit",
            event: "preview",
            variant: ButtonVariant::Primary,
            body: Html::text("Preview schedule"),
            ..Button::default()
        })?);
        render(Form {
            id: "source-period-form",
            event: "preview",
            class: "mt-4",
            body: fields,
            actions: render(Actions { buttons })?,
            ..Form::default()
        })
    }
}
