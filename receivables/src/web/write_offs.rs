//! The invoice's bad-debt journals use the same reviewed contracts as agents.
use super::{format as f, render, Account, Flash, Pages, Written};
use crate::{Document, APPROVE_WRITE_OFFS, SUBMIT_WRITE_OFFS};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    params, Alert, Attrs, Button, ButtonVariant, Card, Cell, Combobox, Facts, Field, FlexRow, Fold,
    Form, Html, InputKind, ListColumns, ModalWidth, SectionHeading, StatusKind,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) struct Journals {
    can_submit: bool,
    can_approve: bool,
    cols: ListColumns,
    draft: Option<Draft>,
    eligible: Vec<i64>,
}
struct Draft {
    action: String,
    id: Option<i64>,
    values: BTreeMap<String, String>,
    errors: f::Errors,
    journal: Option<crate::WriteOff>,
}
fn verb(action: &str) -> &str {
    match action {
        "submit" => "Propose write-off",
        "approve" => "Approve write-off",
        "reject" => "Reject write-off",
        _ => "Reverse write-off",
    }
}
impl Journals {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            can_submit: false,
            can_approve: false,
            draft: None,
            eligible: vec![],
            cols: f::columns(
                "receivables-write-offs-table",
                &[
                    ("date", "Journal date"),
                    ("amount", "Amount"),
                    ("account", "Allowance or expense"),
                    ("status", "Status"),
                    ("history", "History"),
                    ("actions", "Actions"),
                ],
                &["amount"],
            )?,
        })
    }
    pub(super) async fn load(&mut self, pages: &Pages, scope: &Scope) -> Result<()> {
        self.can_submit = pages.allowed(scope, SUBMIT_WRITE_OFFS).await?;
        self.can_approve = pages.allowed(scope, APPROVE_WRITE_OFFS).await?;
        self.eligible = if self.can_submit {
            pages
                .read(scope, "finance.receivables.write_off.accounts", json!({}))
                .await
                .ok()
                .and_then(|v| v["accounts"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|a| a["id"].as_i64())
                .collect()
        } else {
            vec![]
        };
        Ok(())
    }
    pub(super) fn body(
        &mut self,
        d: &Document,
        accounts: &[Account],
        figures: &f::Figures,
        display: &bilibi_datetime::DisplayContext,
    ) -> Result<Html> {
        if d.kind != "invoice" || d.status != "issued" {
            return Ok(Html::EMPTY);
        }
        self.cols.load(&d.write_offs, |w| w.id.to_string());
        let actions = if self.can_submit && d.outstanding.is_some_and(|m| m.is_positive()) {
            render(Button {
                id: "receivables-propose-write-off",
                button_type: "button",
                event: "ask_submit_write_off",
                body: Html::text("Propose write-off"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let mut body = render(SectionHeading {
            title: "Bad-debt journals",
            description: "Approved write-offs reduce the invoice balance without changing revenue or tax. Reverse before recording a recovery receipt.",
            actions,
            ..SectionHeading::default()
        })?;
        let mut rows = vec![];
        for w in &d.write_offs {
            let mut buttons = Html::EMPTY;
            if self.can_approve {
                let actions: &[(&str, &str)] = match w.status() {
                    "submitted" => &[("approve", "Approve"), ("reject", "Reject")],
                    "approved" => &[("reverse", "Reverse")],
                    _ => &[],
                };
                for (action, label) in actions {
                    buttons.push(&render(Button {
                        id: &format!("write-off-{action}-{}", w.id),
                        button_type: "button",
                        event: &format!("ask_{action}_write_off"),
                        name: "write_off_id",
                        value: &w.id.to_string(),
                        body: Html::text(label),
                        ..Button::default()
                    })?);
                }
            }
            let mut history = render(Facts {
                id: &format!("write-off-{}-assessment", w.id),
                items: vec![
                    f::fact("Reason", &w.reason),
                    f::fact("Evidence reference", &w.evidence),
                ],
            })?;
            history.push(&Html::text(&format!(
                "Submitted · {} · ",
                f::actor(Some(w.submitted_by_user_id), w.submitted_by_impersonator_id)
            )));
            history.push(&f::time(
                display,
                &format!("write-off-{}-submitted", w.id),
                &w.submitted_at,
            )?);
            for e in &w.events {
                history.push(&Html::text(&format!(
                    "; {} {} · {} · {} · ",
                    e.event,
                    e.effective_date,
                    f::actor(Some(e.actor_user_id), e.actor_impersonator_id),
                    e.reason
                )));
                history.push(&f::time(
                    display,
                    &format!("write-off-{}-{}", w.id, e.event),
                    &e.occurred_at,
                )?);
                if let Some(id) = e.ledger_entry_id {
                    history.push(&Html::text(" · "));
                    history.push(&f::link(
                        &format!("/finance/ledger/journals/entry-{id}"),
                        &format!("Entry #{}", e.ledger_entry_number.unwrap_or_default()),
                    )?);
                }
            }
            rows.push(
                FlexRow::new(w.id.to_string())
                    .cell("date", Cell::text(&w.entry_date))
                    .slot("amount", f::tabular(&figures.money(w.amount))?)
                    .slot(
                        "account",
                        super::account_html(accounts, w.offset_account_id)?,
                    )
                    .cell(
                        "status",
                        Cell::text(match w.status() {
                            "submitted" => "Awaiting approval",
                            "approved" => "Written off",
                            "rejected" => "Rejected",
                            _ => "Reversed",
                        }),
                    )
                    .slot(
                        "history",
                        render(Fold {
                            id: &format!("write-off-{}-history", w.id),
                            title: "History",
                            summary: "",
                            open: false,
                            body: history,
                        })?,
                    )
                    .slot("actions", f::buttons(buttons)?),
            );
        }
        body.push(&f::table(
            &self.cols,
            "write_off_grid",
            "Bad-debt journals",
            rows,
            "No bad-debt journals for this invoice.",
        )?);
        render(Card {
            id: "receivables-write-offs",
            class: "mt-4",
            body,
            ..Card::default()
        })
    }
    pub(super) fn dialog(
        &self,
        accounts: &[Account],
        flash: &Flash,
        figures: &f::Figures,
    ) -> Result<Option<Html>> {
        let Some(d) = &self.draft else {
            return Ok(None);
        };
        let fields: &[(&str, &str)] = match d.action.as_str() {
            "submit" => &[
                ("entry_date", "Journal date"),
                ("amount", "Amount"),
                ("offset_account_id", "Allowance or expense account"),
                ("reason", "Reason"),
                ("evidence", "Evidence reference"),
            ],
            "reverse" => &[("reversal_date", "Reversal date"), ("reason", "Reason")],
            _ => &[("reason", "Review reason")],
        };
        let labels: Vec<_> = fields
            .iter()
            .map(|(key, label)| {
                (
                    key.to_string(),
                    format!("write-off-{key}"),
                    label.to_string(),
                )
            })
            .collect();
        let mut field_errors = d.errors.clone();
        let mut general = BTreeMap::new();
        field_errors.0.retain(|path, messages| {
            if labels.iter().any(|(field, _, _)| field == path) {
                return true;
            }
            let label = match path.as_str() {
                "write_off_id" => "bad-debt journal",
                "entry_date" => "journal_date",
                "offset_account_id" => "allowance_or_expense_account",
                _ => path.as_str(),
            };
            general.insert(label.to_owned(), messages.clone());
            false
        });
        let mut body = render(field_errors.summary("write-off-errors", &labels))?;
        if !general.is_empty() {
            body.push(&render(Alert {
                id: "write-off-refusal",
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(&f::sentences(&general)),
            })?);
        }
        if let Some(journal) = &d.journal {
            body.push(&render(Facts {
                id: "write-off-review-details",
                items: vec![
                    f::fact("Journal date", &journal.entry_date),
                    f::fact("Amount", &figures.money(journal.amount)),
                    f::fact_html(
                        "Allowance or expense",
                        super::account_html(accounts, journal.offset_account_id)?,
                    ),
                    f::fact("Reason", &journal.reason),
                    f::fact("Evidence reference", &journal.evidence),
                ],
            })?);
        }
        for (key, label) in fields {
            let mut field = Field::in_form("bad_debt", key)
                .id(format!("write-off-{key}"))
                .label(*label)
                .value(d.values.get(*key).cloned().unwrap_or_default())
                .required()
                .errors(d.errors.under(key, label));
            if *key == "offset_account_id" {
                body.push(&render(
                    Combobox::new(field.id.clone(), field.name.clone())
                        .label(format!("{label} *"))
                        .aria_label(*label)
                        .value(field.value.clone())
                        .placeholder("Search accounts")
                        .options(
                            accounts
                                .iter()
                                .filter(|a| self.eligible.contains(&a.id))
                                .map(Account::choice)
                                .collect(),
                        )
                        .errors(field.errors.clone())
                        .attrs(Attrs {
                            required: true,
                            ..Attrs::default()
                        }),
                )?);
                continue;
            }
            field = match *key {
                "entry_date" | "reversal_date" => field.kind(InputKind::Date),
                "reason" | "evidence" => field.kind(InputKind::Textarea).attr("maxlength", "500"),
                _ => field.attr("inputmode", "decimal"),
            };
            body.push(&render(field)?);
        }
        let mut buttons = render(Button {
            id: "write-off-cancel",
            button_type: "button",
            event: "cancel_write_off",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "write-off-submit",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            body: Html::text(verb(&d.action)),
            working: "Saving…",
            ..Button::default()
        })?);
        let form = render(Form {
            id: "write-off-form",
            event: "save_write_off",
            change: false,
            class: "mt-4",
            body,
            actions: f::actions(buttons)?,
        })?;
        Ok(Some(f::modal(
            "write-off-modal",
            verb(&d.action),
            "Keep the collection evidence with this invoice. This records no tax refund or filing.",
            "cancel_write_off",
            ModalWidth::Narrow,
            flash,
            form,
        )?))
    }
    pub(super) async fn event(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        v: &Value,
        record: Option<&Document>,
        flash: &mut Flash,
    ) -> Result<()> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        if event == "write_off_grid" {
            self.cols.handle(&params::string_values(v));
            return Ok(());
        }
        if event == "cancel_write_off" {
            self.draft = None;
            return Ok(());
        }
        if let Some(action) = event
            .strip_prefix("ask_")
            .and_then(|s| s.strip_suffix("_write_off"))
        {
            let cap = if action == "submit" {
                SUBMIT_WRITE_OFFS
            } else {
                APPROVE_WRITE_OFFS
            };
            if !pages.allowed(scope, cap).await? {
                flash.set(
                    StatusKind::Error,
                    "You do not have permission for this bad-debt journal.",
                );
                return Ok(());
            }
            let Some(record) = record else { return Ok(()) };
            let figures = pages.figures(scope).await;
            let mut values = BTreeMap::new();
            values.insert("entry_date".into(), pages.today(scope).await);
            values.insert("reversal_date".into(), pages.today(scope).await);
            values.insert(
                "amount".into(),
                record
                    .outstanding
                    .map(|m| figures.shown(m))
                    .unwrap_or_default(),
            );
            let selected = v["write_off_id"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok());
            self.draft = Some(Draft {
                action: action.into(),
                id: selected,
                journal: record
                    .write_offs
                    .iter()
                    .find(|w| Some(w.id) == selected)
                    .cloned(),
                values,
                errors: f::Errors::default(),
            });
            return Ok(());
        }
        if event != "save_write_off" {
            return Err(Error::Invalid);
        }
        let Some(d) = &mut self.draft else {
            return Ok(());
        };
        d.values.extend(params::form_values(v, "bad_debt"));
        let mut input = json!({});
        for (key, value) in &d.values {
            input[key] = json!(value.trim());
        }
        if let Some(id) = d.id {
            input["write_off_id"] = json!(id);
        } else {
            input["document_id"] = json!(record.ok_or(Error::NotFound)?.id);
        }
        if d.action == "submit" {
            input["offset_account_id"] = json!(d
                .values
                .get("offset_account_id")
                .and_then(|s| s.parse::<i64>().ok()));
        }
        // Only send the fields the selected contract accepts.
        let keys: &[&str] = match d.action.as_str() {
            "submit" => &[
                "document_id",
                "entry_date",
                "amount",
                "offset_account_id",
                "reason",
                "evidence",
            ],
            "reverse" => &["write_off_id", "reversal_date", "reason"],
            _ => &["write_off_id", "reason"],
        };
        input
            .as_object_mut()
            .ok_or(Error::Invalid)?
            .retain(|key, _| keys.contains(&key.as_str()));
        let contract = match d.action.as_str() {
            "submit" => "finance.receivables.write_off.submit",
            "approve" => "finance.receivables.write_off.approve",
            "reject" => "finance.receivables.write_off.reject",
            _ => "finance.receivables.write_off.reverse",
        };
        match pages
            .write(scope, credential, "save_write_off", contract, input)
            .await?
        {
            Written::Done(_) => {
                self.draft = None;
                flash.set(StatusKind::Success, "Bad-debt journal recorded.");
            }
            Written::Invalid(fields) => d.errors = f::Errors(fields),
            Written::Refused(message) => flash.set(StatusKind::Error, message),
        }
        Ok(())
    }
}
