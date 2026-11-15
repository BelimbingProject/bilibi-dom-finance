//! The Posting accounts section of the E-invoice setup page: each ledger
//! account a valid e-invoice posts to, chosen in place from the company's
//! chart with the searchable combobox and saved on its own. A posting
//! refused for a missing account links here ("Choose accounts").
use crate::{Einvoice, EinvoiceError, PostingAccount, PostingAccounts as Chosen};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, AccountName};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    Choice, Combobox, CommitStates, CommitStatus, Fact, Facts, Html, InlineChoice, InlineField,
};
use serde_json::Value;

/// The section's element, which "Choose accounts" links to.
pub(crate) const ANCHOR: &str = "einvoice-posting";
/// The live event one choice raises.
pub(super) const SAVE: &str = "save_posting_account";

const DENIED: &str = "You do not have permission to do that.";

/// What the section holds between events.
#[derive(Default)]
pub(super) struct PostingAccounts {
    /// The accounts that take postings.
    accounts: Vec<Account>,
    /// Whether the chart could be read; when not, a chosen code is shown as
    /// it is instead of being called unknown.
    readable: bool,
    statuses: CommitStates,
}

impl PostingAccounts {
    pub(super) async fn load(&mut self, einvoice: &Einvoice, scope: &Scope, company: i64) {
        let chart = einvoice.posting_choices(scope, company).await;
        self.readable = chart.is_ok();
        self.accounts = chart.unwrap_or_default();
    }

    /// One account chosen in place: saved, or refused beside its field.
    pub(super) async fn save(
        &mut self,
        einvoice: &Einvoice,
        scope: &Scope,
        company: i64,
        value: &Value,
    ) -> Result<()> {
        let field = value["field"].as_str().ok_or(Error::Invalid)?;
        let account = PostingAccount::of_field(field).ok_or(Error::Invalid)?;
        let submitted = value["value"].as_str().ok_or(Error::Invalid)?.trim();
        self.statuses.clear_saved();
        let status = if submitted.is_empty() {
            CommitStatus::refusal(account.label(), submitted, &["must be chosen"])
        } else {
            match einvoice
                .choose_posting_account(scope, company, account, submitted)
                .await
            {
                Ok(()) => CommitStatus::Saved,
                Err(EinvoiceError::Invalid(errors)) => {
                    let reasons: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                    CommitStatus::refusal(account.label(), submitted, &reasons)
                }
                Err(EinvoiceError::Unauthorized) => CommitStatus::Error(DENIED.into()),
                Err(EinvoiceError::CompanyArchived) => {
                    CommitStatus::Error("This company is archived and cannot be changed.".into())
                }
                Err(EinvoiceError::Failed(error)) => return Err(error),
                Err(_) => CommitStatus::Error(
                    "Nothing was changed. Reload the page and try again.".into(),
                ),
            }
        };
        self.statuses.put(field, status);
        Ok(())
    }

    /// The accounts as facts, each a choice in place.
    pub(super) fn facts(&self, chosen: &Chosen) -> Result<Html> {
        let options: Vec<Choice> = self
            .accounts
            .iter()
            .map(|account| Choice::new(&account.name, &account.code).detail(&account.code))
            .collect();
        let mut items = Vec::new();
        for account in PostingAccount::ALL {
            let field = account.field();
            let code = chosen.get(account).unwrap_or_default();
            let display = match self.accounts.iter().find(|entry| entry.code == code) {
                Some(entry) => Html::render(&AccountName {
                    name: &entry.name,
                    code: &entry.code,
                })
                .map_err(|_| Error::Internal)?,
                None if code.is_empty() => Html::text("Choose an account"),
                None if !self.readable => Html::text(code),
                // A code that no longer names an account that takes postings.
                None => Html::text(&format!("{code} · not an active account")),
            };
            let mut inline = InlineField::new(
                format!("einvoice-posting-{}", field.replace('_', "-")),
                field,
                account.label(),
                code,
            )
            .version(0)
            .status(self.statuses.get(field));
            inline.id_value = String::new();
            inline.save_event = SAVE.into();
            let editor = Combobox::new(format!("{}-input", inline.id), &inline.name)
                .aria_label(&inline.label)
                .placeholder("Search accounts")
                .wrapper_class("mb-0")
                .value(code)
                .options(options.clone());
            let editor = Html::render(&editor).map_err(|_| Error::Internal)?;
            items.push(Fact {
                title: account.label(),
                id: "",
                value: Html::render(&InlineChoice {
                    field: inline,
                    display,
                    editor,
                })
                .map_err(|_| Error::Internal)?,
            });
        }
        Html::render(&Facts {
            id: "einvoice-posting-facts",
            items,
        })
        .map_err(|_| Error::Internal)
    }
}
