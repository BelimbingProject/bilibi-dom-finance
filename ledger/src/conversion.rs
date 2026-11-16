//! Opening-state recognition extends Bilimbi's owned-posting boundary without
//! changing the original journal, its owner or its evidence.
use crate::*;
use bilibi_agent_api::OperationClient;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct OpeningContext {
    pub source_reference: String,
    pub journal_id: i64,
    pub prepared_by: Preparer,
    pub entry: Entry,
    pub balances: BTreeMap<i64, Money>,
}

impl Ledger {
    /// Validate or capture a company's committed posting boundary on the handed snapshot.
    pub async fn entry_boundary_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        through: Option<i64>,
    ) -> Result<i64> {
        agent_operations::authorize_on(client, scope, company, VIEW).await?;
        let maximum:i64=client.query_opt("SELECT COALESCE(max(entry_number),0) FROM finance_ledger_entries WHERE tenant_id=$1 AND company_id=$2",&[&scope.tenant(),&company]).await?.ok_or(Error::Internal)?.get(0);
        let pin = through.unwrap_or(maximum);
        if pin < 0 || pin > maximum {
            return Err(LedgerError::InvalidOption);
        }
        Ok(pin)
    }

    /// The approved, unreversed conversion journal on the caller's connection.
    /// A waiting journal is not an opening balance against which items can tie.
    pub async fn opening_context_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
    ) -> Result<OpeningContext> {
        agent_operations::authorize_on(client, scope, company, VIEW).await?;
        let row = client.query_opt("SELECT o.source_reference,o.journal_id,e.entry_id FROM finance_ledger_opening_balances o JOIN finance_ledger_journal_events e ON e.journal_id=o.journal_id AND e.tenant_id=o.tenant_id AND e.company_id=o.company_id AND e.event='approved' JOIN finance_ledger_entries p ON p.id=e.entry_id AND p.tenant_id=o.tenant_id AND p.company_id=o.company_id WHERE o.tenant_id=$1 AND o.company_id=$2 AND NOT EXISTS(SELECT 1 FROM finance_ledger_entries r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.reverses_entry_id=p.id) ORDER BY o.id DESC LIMIT 1", &[&scope.tenant(), &company]).await?
            .ok_or_else(|| LedgerError::Invalid(vec![FieldError::new("opening", "Opening balances must be approved first.")]))?;
        let entry = self
            .get_entry_on(client, scope, company, row.get(2))
            .await?;
        let journal = journals::find(client, scope.tenant(), company, row.get(1))
            .await?
            .ok_or(LedgerError::JournalNotFound)?;
        let balances = client.query("SELECT account_id,sum(debit-credit) FROM finance_ledger_entry_lines WHERE tenant_id=$1 AND company_id=$2 AND entry_id=$3 GROUP BY account_id ORDER BY account_id", &[&scope.tenant(),&company,&entry.id]).await?.iter().map(|row|(row.get(0),Money::stored(row.get(1)))).collect();
        Ok(OpeningContext {
            source_reference: row.get(0),
            journal_id: row.get(1),
            prepared_by: Preparer {
                user_id: journal.submitted_by_user_id,
                impersonator_id: journal.submitted_by_impersonator_id,
            },
            entry,
            balances,
        })
    }

    /// Recognize a complete opening account in the owner's audited transaction.
    /// `balance` is debit-positive. This locks the source against reversal while
    /// opening records depend on it; it never changes entry ownership.
    pub async fn recognize_opening_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        owner: &EntryOwner,
        account: i64,
        balance: Money,
    ) -> Result<()> {
        self.replace_opening_recognition_on(tx, scope, company, owner, None, account, balance)
            .await
    }

    /// Transfer recognition to a reconciled replacement without altering the
    /// journal or removing prior recognition. Only the current owner can transfer.
    pub async fn replace_opening_recognition_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        owner: &EntryOwner,
        replaces_record: Option<&str>,
        account: i64,
        balance: Money,
    ) -> Result<()> {
        agent_operations::authorize_on(tx.sql(), scope, company, VIEW).await?;
        lock(tx, scope, company).await?;
        let context = self.opening_context_on(tx.sql(), scope, company).await?;
        let eligible = self
            .list_accounts_on(tx.sql(), scope, company)
            .await?
            .iter()
            .any(|a| {
                a.id == account
                    && !a.is_group
                    && [AccountType::Asset, AccountType::Liability].contains(&a.account_type)
            });
        if !entries::owner_valid(owner)
            || !eligible
            || context
                .balances
                .get(&account)
                .copied()
                .unwrap_or(Money::ZERO)
                != balance
        {
            return Err(LedgerError::Invalid(vec![FieldError::new(
                "items",
                "must reconcile exactly to the approved opening account balance",
            )]));
        }
        let tenant = scope.tenant();
        let existing=tx.sql().query_opt("SELECT r.id,r.owner_module,r.owner_record FROM finance_ledger_opening_recognitions r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.account_id=$3 AND NOT EXISTS(SELECT 1 FROM finance_ledger_opening_recognition_replacements x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_recognition_id=r.id)",&[&tenant,&company,&account]).await.map_err(Error::from)?;
        let replaces = match (existing, replaces_record) {
            (None, None) => None,
            (Some(row), Some(record))
                if row.get::<_, String>(1) == owner.module
                    && row.get::<_, String>(2) == record
                    && record != owner.record =>
            {
                Some(row.get::<_, i64>(0))
            }
            _ => {
                return Err(LedgerError::Invalid(vec![FieldError::new(
                    "items",
                    "the opening account has already been imported; a replacement must name its current owner",
                )]));
            }
        };
        let entry = context.entry.id;
        let module = owner.module.clone();
        let reference = owner.record.clone();
        tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one("INSERT INTO finance_ledger_opening_recognitions(tenant_id,company_id,entry_id,account_id,owner_module,owner_record,balance) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id", &[&tenant,&company,&entry,&account,&module,&reference,&balance.amount()]).await?;
            let id:i64=row.get(0);
            let mut changes=vec![Change {kind:"finance.ledger.opening_recognition".into(),id,old:Value::Null,new:json!({"entry_id":entry,"account_id":account,"owner_module":module,"owner_record":reference,"balance":balance})}];
            if let Some(replaced)=replaces {
                let row=sql.query_one("INSERT INTO finance_ledger_opening_recognition_replacements(tenant_id,company_id,recognition_id,replaces_recognition_id) VALUES($1,$2,$3,$4) RETURNING id",&[&tenant,&company,&id,&replaced]).await?;
                changes.push(Change{kind:"finance.ledger.opening_recognition_replacement".into(),id:row.get(0),old:Value::Null,new:json!({"recognition_id":id,"replaces_recognition_id":replaced})});
            }
            Ok(((),changes))
        })).await?;
        Ok(())
    }
}

pub(crate) async fn refuse_recognized_reversal(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    entry: i64,
) -> Result<()> {
    if client.query_opt("SELECT id FROM finance_ledger_opening_recognitions WHERE tenant_id=$1 AND company_id=$2 AND entry_id=$3 LIMIT 1", &[&tenant,&company,&entry]).await?.is_some() {
        return Err(LedgerError::Invalid(vec![FieldError::new("entry_id", "opening items depend on this entry; its opening balances cannot be reversed")]));
    }
    Ok(())
}

impl OpeningContext {
    /// Import approval approves the source preparer's balances, with the same
    /// impersonation-aware duty separation as the opening journal.
    pub fn approval(&self, module: &str, reason: Option<String>) -> Approval {
        Approval {
            source: module.into(),
            action: "import_opening".into(),
            subject_type: "opening".into(),
            subject_id: self.journal_id,
            subject: format!("Opening items: {}", self.source_reference),
            prepared_by: vec![self.prepared_by],
            override_reason: reason,
        }
    }
}
