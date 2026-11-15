//! Choosing the ledger accounts a validated document posts to (the
//! `finance.einvoice.posting.*` settings) from the setup page: a person who
//! manages the e-invoice setup picks each one from the company's own chart,
//! so a posting refused for a missing account is fixed where it is reported.
use crate::{
    clearance::company_target, fiscal::ledger_error, Einvoice, EinvoiceError, FieldError,
    PostingAccount, Result, SETTINGS,
};
use bilibi_database::Error;
use bilibi_finance_ledger::Account;
use bilibi_tenancy::Scope;
use serde_json::json;

impl PostingAccount {
    /// The name a person reads, in the words a refused posting uses.
    pub fn label(self) -> &'static str {
        match self {
            Self::Receivable => "Receivable account",
            Self::Revenue => "Income account",
            Self::Tax => "Tax account",
            Self::Settlement => "Settlement account",
            Self::Payable => "Payable account",
            Self::Expense => "Expense account",
            Self::PurchaseTax => "Purchase tax account",
        }
    }

    /// The posting account a setting field names.
    pub fn of_field(field: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|account| account.field() == field)
    }
}

impl Einvoice {
    /// The accounts of the chart that take postings, for the setup page's
    /// choices (`finance.einvoice.settings.manage`). The chart is read under
    /// that capability, so someone who manages the setup without
    /// `finance.ledger.view` is still offered them.
    pub async fn posting_choices(&self, scope: &Scope, company_id: i64) -> Result<Vec<Account>> {
        let client = self.db.get().await.map_err(Error::from)?;
        let chart = self
            .ledger
            .list_accounts_for_on(&**client, scope, company_id, SETTINGS)
            .await
            .map_err(ledger_error)?;
        Ok(chart.into_iter().filter(Account::postable).collect())
    }

    /// Chooses the ledger account `account` posts to
    /// (`finance.einvoice.settings.manage`): `code` is the code of an active
    /// account of the company's chart that takes postings. The audit action
    /// is `finance.einvoice.posting_account.choose`; Base Settings records
    /// the changed value.
    pub async fn choose_posting_account(
        &self,
        scope: &Scope,
        company_id: i64,
        account: PostingAccount,
        code: &str,
    ) -> Result<()> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                SETTINGS,
                "finance.einvoice.posting_account.choose",
            )
            .await?;
        let code = code.trim();
        let chart = self
            .ledger
            .list_accounts_for_on(tx.sql(), scope, company_id, SETTINGS)
            .await
            .map_err(ledger_error)?;
        if !chart
            .iter()
            .any(|entry| entry.postable() && entry.code == code)
        {
            return Err(EinvoiceError::Invalid(vec![FieldError::new(
                account.field(),
                "must be an active account of the chart that takes postings",
            )]));
        }
        self.settings
            .put(
                &mut tx,
                scope,
                company_target(company_id)?,
                &format!("finance.einvoice.posting.{}", account.field()),
                json!(code),
            )
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
