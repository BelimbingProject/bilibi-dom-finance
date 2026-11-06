//! Asset categories: "+ Category" opens a dialog for a new one, and each
//! row's defaults are changed in place. A change is a complete new revision
//! (assets keep the revision they were registered under), saved through the
//! same contract the agent doors call, with the change itself as its reason.
use super::{input::Draft, *};
use crate::{Category, Method, RevisionInput};
use bilibi_ui::{
    Choice, Combobox, CommitStates, CommitStatus, InlineChoice, InlineEdit, InlineField, InputKind,
};
use std::collections::HashSet;

/// (id, name, account type, postable, code), as `Pages::account_context` reads them.
pub(super) type Accounts = [(i64, String, String, bool, String)];

/// The five account roles of a category, with the account type each takes.
pub(super) const ROLES: [(&str, &str, &str); 5] = [
    ("cost_account_id", "Cost account", "asset"),
    (
        "accumulated_account_id",
        "Accumulated depreciation account",
        "asset",
    ),
    (
        "expense_account_id",
        "Depreciation expense account",
        "expense",
    ),
    ("gain_account_id", "Disposal gain account", "income"),
    ("loss_account_id", "Disposal loss account", "expense"),
];

#[derive(Default)]
pub(super) struct State {
    /// The "+ Category" dialog's draft while it is open.
    pub new: Option<Draft>,
    pub statuses: CommitStates,
    /// Categories switched to reducing balance that wait for their rate.
    pending_reducing: HashSet<i64>,
}

pub(super) fn account_choices(accounts: &Accounts, kind: &str) -> Vec<Choice> {
    accounts
        .iter()
        .filter(|a| a.3 && a.2 == kind)
        .map(|a| Choice::new(&a.1, a.0.to_string()).detail(&a.4))
        .collect()
}

/// An account as a cell shows it: the name, then the code muted.
#[derive(Template)]
#[template(
    source = r#"{{ name }}{% if !code.is_empty() %} <span class="text-ink-muted italic">{{ code }}</span>{% endif %}"#,
    ext = "html"
)]
pub(super) struct AccountName<'a> {
    pub name: &'a str,
    pub code: &'a str,
}
pub(super) fn account_name(accounts: &Accounts, id: i64) -> Result<Html> {
    match accounts.iter().find(|a| a.0 == id) {
        Some(a) => render(AccountName {
            name: &a.1,
            code: &a.4,
        }),
        None => Ok(Html::text("Account unavailable")),
    }
}

fn rate_text(rate: Option<rust_decimal::Decimal>) -> String {
    rate.map(|r| format!("{}%", r.normalize()))
        .unwrap_or_default()
}

impl State {
    pub(super) fn dialog(&self, accounts: &Accounts) -> Result<Html> {
        let Some(d) = &self.new else {
            return Ok(Html::EMPTY);
        };
        let mut fields = vec![d.field("name", "Name", InputKind::Text, true, "asset")];
        for (key, label, kind) in ROLES {
            fields.push(
                d.field(key, label, InputKind::Select, true, "asset")
                    .options(account_choices(accounts, kind))
                    .placeholder("Choose an account"),
            );
        }
        fields.push(d.method_field());
        fields.push(d.field(
            "life_months",
            "Useful life (months)",
            InputKind::Number,
            true,
            "asset",
        ));
        if d.value("method") == "reducing_balance" {
            fields.push(
                d.field(
                    "annual_rate",
                    "Annual rate (%)",
                    InputKind::Text,
                    true,
                    "asset",
                )
                .hint("Each month charges this rate of the remaining value, divided by 12."),
            );
        }
        let alert = d
            .errors
            .iter()
            .filter(|e| e.field == "base")
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join(" ");
        Dialog {
            id: "category-modal",
            title: "New asset category",
            description: "Sets the accounts and the depreciation its assets start with.",
            change: "category_change",
            submit: "create_category",
            cancel: "close_category",
            verb: "Add category",
            working: "Adding…",
            alert: &alert,
            fields,
            extra: Html::EMPTY,
            can_submit: true,
        }
        .render()
    }

    pub(super) fn change(&mut self, v: &serde_json::Value) {
        if let Some(d) = &mut self.new {
            *d = Draft::posted(v);
            if d.value("method") != "reducing_balance" {
                d.values.remove("annual_rate");
            }
        }
    }

    pub(super) async fn create(
        &mut self,
        pages: &Pages,
        s: &Scope,
        credential: &str,
    ) -> crate::Result<String> {
        let d = self.new.as_ref().ok_or(Error::Invalid)?;
        let name = d.value("name").trim().to_owned();
        let mut errors = Vec::new();
        if name.is_empty() {
            errors.push(bilibi_finance_ledger::FieldError::new(
                "name",
                "can't be blank",
            ));
        }
        let mut draft = d.clone();
        draft
            .values
            .insert("reason".into(), "Category created".into());
        let revision = match draft.revision() {
            Ok(r) => Some(r),
            Err(FixedAssetsError::Invalid(fields)) => {
                errors.extend(fields);
                None
            }
            Err(e) => return Err(e),
        };
        let Some(revision) = revision.filter(|_| errors.is_empty()) else {
            errors.sort_by_key(|e| e.field);
            return Err(FixedAssetsError::Invalid(errors));
        };
        let mut input = serde_json::to_value(&revision).map_err(|_| Error::Internal)?;
        input["name"] = serde_json::json!(name);
        pages
            .submit(
                s,
                credential,
                "create_category",
                "finance.fixed_assets.category.save",
                input,
            )
            .await?;
        Ok(name)
    }

    /// One default of a category changed in place: a complete new revision
    /// with that one value changed. Switching to reducing balance waits on
    /// the page for its rate, so nothing half-set is saved.
    pub(super) async fn save(
        &mut self,
        pages: &Pages,
        s: &Scope,
        credential: &str,
        categories: &[Category],
        accounts: &Accounts,
        v: &serde_json::Value,
    ) -> Result<()> {
        let id = v["id"]
            .as_str()
            .and_then(|i| i.parse::<i64>().ok())
            .ok_or(Error::Invalid)?;
        let field = v["field"].as_str().ok_or(Error::Invalid)?;
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        let category = categories
            .iter()
            .find(|c| c.id == id)
            .ok_or(Error::Invalid)?;
        let key = format!("category-{id}-{field}");
        let mut defaults: RevisionInput = category.revision.defaults.clone();
        let (label, shown): (&str, String) = match field {
            "method" => {
                let label = "Method";
                match submitted.as_str() {
                    "straight_line" => {
                        defaults.assumptions.method = Method::StraightLine;
                        defaults.assumptions.annual_rate = None;
                        self.pending_reducing.remove(&id);
                    }
                    "reducing_balance" if defaults.assumptions.annual_rate.is_none() => {
                        self.pending_reducing.insert(id);
                        self.statuses.put(
                            key,
                            CommitStatus::Error(
                                "Enter the annual rate to switch to reducing balance.".into(),
                            ),
                        );
                        return Ok(());
                    }
                    "reducing_balance" => {
                        defaults.assumptions.method = Method::ReducingBalance;
                    }
                    _ => {
                        self.statuses.put(
                            key,
                            CommitStatus::refusal(label, &submitted, &["is not a method"]),
                        );
                        return Ok(());
                    }
                }
                (label, input::method(defaults.assumptions.method).to_owned())
            }
            "life_months" => {
                let label = "Useful life";
                let Ok(months) = submitted.parse::<i32>() else {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["must be a number of months"]),
                    );
                    return Ok(());
                };
                defaults.assumptions.life_months = months;
                (label, format!("{months} months"))
            }
            "annual_rate" => {
                let label = "Annual rate";
                let Ok(rate) = submitted.trim_end_matches('%').trim().parse() else {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["must be a percentage"]),
                    );
                    return Ok(());
                };
                defaults.assumptions.annual_rate = Some(rate);
                if self.pending_reducing.contains(&id) {
                    defaults.assumptions.method = Method::ReducingBalance;
                }
                (label, rate_text(Some(rate)))
            }
            role => {
                let Some((_, role_label, _)) = ROLES.iter().find(|(k, _, _)| *k == role) else {
                    return Err(Error::Invalid);
                };
                let label: &str = role_label;
                let Ok(account) = submitted.parse::<i64>() else {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["must be an account"]),
                    );
                    return Ok(());
                };
                match role {
                    "cost_account_id" => defaults.cost_account_id = account,
                    "accumulated_account_id" => defaults.accumulated_account_id = account,
                    "expense_account_id" => defaults.expense_account_id = account,
                    "gain_account_id" => defaults.gain_account_id = account,
                    _ => defaults.loss_account_id = account,
                }
                (
                    label,
                    accounts
                        .iter()
                        .find(|a| a.0 == account)
                        .map(|a| format!("{} · {}", a.1, a.4))
                        .unwrap_or_else(|| submitted.clone()),
                )
            }
        };
        defaults.reason = format!("{label} changed to {shown}");
        let mut input = serde_json::to_value(&defaults).map_err(|_| Error::Internal)?;
        input["category_id"] = id.into();
        let status = match pages
            .submit(
                s,
                credential,
                "save_category",
                "finance.fixed_assets.category.save",
                input,
            )
            .await
        {
            Ok(_) => {
                if field != "method" || defaults.assumptions.method == Method::StraightLine {
                    self.pending_reducing.remove(&id);
                }
                if field == "annual_rate" {
                    self.statuses
                        .put(format!("category-{id}-method"), CommitStatus::None);
                }
                CommitStatus::Saved
            }
            Err(FixedAssetsError::Invalid(fields)) => {
                let reasons: Vec<&str> = fields.iter().map(|f| f.message.as_str()).collect();
                CommitStatus::refusal(label, &submitted, &reasons)
            }
            Err(e) => CommitStatus::Error(refusal(e)),
        };
        self.statuses.put(key, status);
        Ok(())
    }

    fn inline(
        &self,
        category: &Category,
        field: &str,
        label: &str,
        value: String,
        can: bool,
    ) -> InlineField {
        let key = format!("category-{}-{field}", category.id);
        let mut inline = InlineField::new(key.clone(), field, label, value)
            .version(category.revision.id)
            .status(self.statuses.get(&key));
        inline.id_value = category.id.to_string();
        inline.save_event = "save_category".into();
        if !can {
            inline = inline.readonly();
        }
        inline
    }

    /// One category's row: each default edited in place by someone who may
    /// configure categories, read alone by anyone else.
    pub(super) fn row(&self, c: &Category, accounts: &Accounts, can: bool) -> Result<FlexRow> {
        let d = &c.revision.defaults;
        let a = &d.assumptions;
        let reducing = a.method == Method::ReducingBalance || self.pending_reducing.contains(&c.id);
        let method = {
            let editor = Field::named("method")
                .id(format!("category-{}-method-editor", c.id))
                .label_class("sr-only")
                .label("Method")
                .kind(InputKind::Select)
                .value(match a.method {
                    Method::StraightLine => "straight_line",
                    Method::ReducingBalance => "reducing_balance",
                })
                .options(vec![
                    Choice::new("Straight line", "straight_line"),
                    Choice::new("Reducing balance", "reducing_balance"),
                ]);
            let value = match a.method {
                Method::StraightLine => "straight_line",
                Method::ReducingBalance => "reducing_balance",
            };
            render(InlineChoice {
                field: self.inline(c, "method", "Method", value.into(), can),
                display: Html::text(input::method(a.method)),
                editor: render(editor)?,
            })?
        };
        let life = render(InlineEdit {
            field: self
                .inline(
                    c,
                    "life_months",
                    "Useful life (months)",
                    a.life_months.to_string(),
                    can,
                )
                .input_type("number")
                .display_value(format!("{} months", a.life_months)),
        })?;
        let rate = if reducing {
            render(InlineEdit {
                field: self
                    .inline(
                        c,
                        "annual_rate",
                        "Annual rate (%)",
                        a.annual_rate
                            .map(|r| r.normalize().to_string())
                            .unwrap_or_default(),
                        can,
                    )
                    .display_value(rate_text(a.annual_rate))
                    .placeholder("Rate, such as 20"),
            })?
        } else {
            Html::text("—")
        };
        let mut row = FlexRow::new(c.id.to_string())
            .cell("name", Cell::text(c.name.clone()))
            .slot("method", method)
            .slot("life", life)
            .slot("rate", rate);
        for (key, label, kind) in ROLES {
            let id = match key {
                "cost_account_id" => d.cost_account_id,
                "accumulated_account_id" => d.accumulated_account_id,
                "expense_account_id" => d.expense_account_id,
                "gain_account_id" => d.gain_account_id,
                _ => d.loss_account_id,
            };
            let editor = Combobox::new(format!("category-{}-{key}-input", c.id), key)
                .aria_label(label)
                .placeholder("Choose an account")
                .wrapper_class("mb-0")
                .value(id.to_string())
                .options(account_choices(accounts, kind));
            row = row.slot(
                key,
                render(InlineChoice {
                    field: self.inline(c, key, label, id.to_string(), can),
                    display: account_name(accounts, id)?,
                    editor: render(editor)?,
                })?,
            );
        }
        Ok(row)
    }
}

/// Every change to the categories, newest first, folded away: who changed
/// what, and when.
pub(super) fn history(
    categories: &[Category],
    people: &People,
    display: &DisplayContext,
    columns: &ListColumns,
) -> Result<Html> {
    let mut revisions: Vec<&crate::CategoryRevision> =
        categories.iter().flat_map(|c| &c.revisions).collect();
    revisions.sort_by_key(|r| std::cmp::Reverse(r.id));
    let count = revisions.len();
    let mut rows = Vec::new();
    for r in revisions {
        rows.push(
            FlexRow::new(r.id.to_string())
                .cell("category", Cell::text(r.name.clone()))
                .cell("change", Cell::text(r.defaults.reason.clone()))
                .cell(
                    "by",
                    Cell::text(people.name(r.recorded_by_user_id, r.recorded_by_impersonator_id)),
                )
                .cell("at", time_cell(display, &r.recorded_at)?),
        );
    }
    let table = table(
        "assets-category-history",
        columns,
        rows,
        "Category changes",
        "history_grid",
        "No changes yet.",
    )?;
    render(bilibi_ui::Fold {
        id: "assets-category-history-fold",
        title: "History",
        summary: &match count {
            1 => "1 change".to_owned(),
            n => format!("{n} changes"),
        },
        open: false,
        body: table,
    })
}

pub(super) fn history_columns() -> Result<ListColumns> {
    cols(
        "assets-category-history",
        &[
            ("category", "Category"),
            ("change", "Change"),
            ("by", "By"),
            ("at", "When"),
        ],
    )
}
