//! The bank accounts list's two dialogs: "+ Bank account" (only the name
//! and the ledger account; the bank's columns are matched on the first
//! import) and Edit (name, counter accounts, match window, reason; the
//! ledger account never changes).
use super::render;
use crate::{BankAccount, FieldError};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, AccountType};
use bilibi_ui::{
    Alert, Attrs, Button, ButtonVariant, Choice, Combobox, Field, Form, Html, InputKind, Modal,
    StatusKind,
};

fn errors(errors: &[FieldError], field: &str) -> Vec<String> {
    errors
        .iter()
        .filter(|e| e.field == field)
        .map(|e| bilibi_ui::error_sentence(&label(field), &e.message))
        .collect()
}
fn label(field: &str) -> String {
    match field {
        "name" => "Name",
        "ledger_account_id" => "Ledger account",
        "charges_account_id" => "Bank charges post to",
        "interest_account_id" => "Interest posts to",
        "match_window_days" => "Match window",
        other => other,
    }
    .into()
}
/// A refusal no field of the dialog shows, said in its own alert.
fn unplaced(errors: &[FieldError], fields: &[&str]) -> Vec<String> {
    errors
        .iter()
        .filter(|e| !fields.contains(&e.field))
        .map(|e| bilibi_ui::error_sentence(&label(e.field), &e.message))
        .collect()
}
pub(super) fn optional_id(text: &str) -> Option<i64> {
    text.trim().parse::<i64>().ok().filter(|id| *id > 0)
}
pub(super) fn choices(chart: &[Account], kind: AccountType, except: &[i64]) -> Vec<Choice> {
    let mut accounts: Vec<&Account> = chart
        .iter()
        .filter(|a| a.postable() && a.account_type == kind && !except.contains(&a.id))
        .collect();
    accounts.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.code.cmp(&b.code))
    });
    accounts
        .into_iter()
        .map(|a| Choice::new(&a.name, a.id.to_string()).detail(&a.code))
        .collect()
}

/// An account as a sentence names it: "Cash at bank · 1000".
fn account_label(chart: &[Account], id: i64) -> String {
    chart.iter().find(|a| a.id == id).map_or_else(
        || "an account no longer readable".into(),
        |a| format!("{} · {}", a.name, a.code),
    )
}

pub(super) fn new_dialog(
    company_name: &str,
    chart: &[Account],
    taken: &[i64],
    name: &str,
    ledger: &str,
    errors_list: &[FieldError],
) -> Result<Html> {
    let mut body = Html::EMPTY;
    let base = unplaced(errors_list, &["name", "ledger_account_id"]);
    if !base.is_empty() {
        body.push(&render(Alert {
            id: "bank-account-new-alert",
            kind: StatusKind::Error,
            body: Html::text(&base.join(" ")),
            ..Alert::default()
        })?);
    }
    body.push(&render(
        Field::in_form("bank_account", "name")
            .id("bank-account-name")
            .label("Name")
            .value(name)
            .attr("maxlength", "200")
            .required()
            .hint("As people call it, such as the bank and the last digits of the account.")
            .errors(errors(errors_list, "name")),
    )?);
    let options = choices(chart, AccountType::Asset, taken);
    let hint = if options.is_empty() {
        "Every asset account already has a bank account. Add an asset account to the chart of accounts first."
    } else {
        "The asset account this bank account's money is kept in. It cannot be changed later."
    };
    body.push(&render(
        Combobox::new(
            "bank-account-ledger-account",
            "bank_account[ledger_account_id]",
        )
        .label("Ledger account *")
        .aria_label("Ledger account")
        .placeholder("Choose an account")
        .value(ledger)
        .options(options)
        .hint(hint)
        .errors(errors(errors_list, "ledger_account_id"))
        .attrs(Attrs {
            required: true,
            ..Attrs::default()
        }),
    )?);
    let form = Form {
        id: "bank-account-new-form",
        event: "new_change",
        change: true,
        class: "mt-4",
        body,
        actions: dialog_actions("bank-account-new", "Add bank account", "Adding…")?,
    }
    .submit_event("create")
    .map_err(|_| Error::Internal)?;
    render(Modal {
        id: "bank-account-new-modal",
        title: "New bank account",
        description: &format!(
            "For {company_name}: name it and choose its ledger account; its bank's columns are matched when you import its first statement."
        ),
        cancel_event: "close_dialog",
        body: form,
        ..Modal::default()
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn edit_dialog(
    chart: &[Account],
    account: &BankAccount,
    name: &str,
    charges: &str,
    interest: &str,
    window: &str,
    reason: &str,
    errors_list: &[FieldError],
) -> Result<Html> {
    let mut body = Html::EMPTY;
    let fields = [
        "name",
        "charges_account_id",
        "interest_account_id",
        "match_window_days",
        "reason",
    ];
    let base = unplaced(errors_list, &fields);
    if !base.is_empty() {
        body.push(&render(Alert {
            id: "bank-account-edit-alert",
            kind: StatusKind::Error,
            body: Html::text(&base.join(" ")),
            ..Alert::default()
        })?);
    }
    body.push(&render(
        Field::in_form("bank_account", "name")
            .id("bank-account-edit-name")
            .label("Name")
            .value(name)
            .attr("maxlength", "200")
            .required()
            .errors(errors(errors_list, "name")),
    )?);
    for (key, dom, title, kind, value, hint) in [
        (
            "charges_account_id",
            "bank-account-charges-account",
            "Bank charges post to",
            AccountType::Expense,
            charges,
            "The expense account a bank charge on the statement is posted to.",
        ),
        (
            "interest_account_id",
            "bank-account-interest-account",
            "Interest posts to",
            AccountType::Income,
            interest,
            "The income account interest on the statement is posted to.",
        ),
    ] {
        body.push(&render(
            Combobox::new(dom, format!("bank_account[{key}]"))
                .label(title)
                .placeholder("None")
                .value(value)
                .options(choices(chart, kind, &[]))
                .hint(hint)
                .errors(errors(errors_list, key)),
        )?);
    }
    body.push(&render(
        Field::in_form("bank_account", "match_window_days")
            .id("bank-account-window")
            .label("Match window (days)")
            .kind(InputKind::Number)
            .value(window)
            .attr("min", "0")
            .attr("max", "90")
            .hint("How many days apart a statement line and a ledger entry may be for a suggested match.")
            .errors(errors(errors_list, "match_window_days")),
    )?);
    body.push(&render(
        Field::in_form("bank_account", "reason")
            .id("bank-account-reason")
            .label("Reason for the change")
            .value(reason)
            .attr("maxlength", "500"),
    )?);
    let form = Form {
        id: "bank-account-edit-form",
        event: "edit_change",
        change: true,
        class: "mt-4",
        body,
        actions: dialog_actions("bank-account-edit", "Save changes", "Saving…")?,
    }
    .submit_event("save")
    .map_err(|_| Error::Internal)?;
    let ledger = account_label(chart, account.ledger_account_id);
    render(Modal {
        id: "bank-account-edit-modal",
        title: &format!("Edit {}", account.name),
        description: &format!("It keeps its ledger account, {ledger}."),
        cancel_event: "close_dialog",
        body: form,
        ..Modal::default()
    })
}

/// Cancel, then the dialog's one primary verb at the end of the row.
fn dialog_actions(id: &str, verb: &str, working: &str) -> Result<Html> {
    let mut buttons = render(Button {
        id: &format!("{id}-cancel"),
        button_type: "button",
        event: "close_dialog",
        body: Html::text("Cancel"),
        ..Button::default()
    })?;
    buttons.push(&render(Button {
        id: &format!("{id}-submit"),
        button_type: "submit",
        variant: ButtonVariant::Primary,
        working,
        body: Html::text(verb),
        ..Button::default()
    })?);
    super::dialog_row(buttons)
}
