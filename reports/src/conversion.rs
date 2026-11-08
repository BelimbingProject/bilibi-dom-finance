//! Domain-owned conversion evidence. Compose the owners, freeze their answers;
//! never infer a clean conversion merely because the trial balance balances.
use crate::{
    authorize, issued, json as encode, pack_owner, Reports, ReportsError, Result, ISSUE, MODULE,
    SIGN_OFF, VIEW,
};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_finance_ledger::{Approval, Money, Preparer, TotalsQuery};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use tokio_postgres::{GenericClient, Transaction};

/// Accounting boundaries for both subledgers in a conversion snapshot.
/// Supply every field to replay a snapshot, or leave every field unset.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ConversionPins {
    pub through_entry_number: Option<i64>,
    pub receivables_opening_batch_id: Option<i64>,
    pub receivables_settlement_number: Option<i64>,
    pub payables_opening_batch_id: Option<i64>,
    pub payables_settlement_number: Option<i64>,
}

pub(crate) const KIND: &str = "conversion";
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct SignOff {
    pub reviewer_reference: String,
    pub note: String,
    pub attestation: String,
}

impl Reports {
    /// A coherent conversion preview. Issuance collects the same facts on the
    /// reviewed write's transaction, then stores them rather than just pins.
    pub async fn conversion_report(
        &self,
        scope: &Scope,
        company: i64,
        as_of: Date,
        pins: &ConversionPins,
    ) -> Result<Value> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let tx = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await
            .map_err(Error::from)?;
        let report = self.conversion_on(&tx, scope, company, as_of, pins).await?;
        tx.commit().await.map_err(Error::from)?;
        Ok(report)
    }

    pub(crate) async fn conversion_access(
        &self,
        c: &(impl GenericClient + Sync),
        s: &Scope,
        company: i64,
        capability: &str,
    ) -> Result<()> {
        authorize(c, s, company, capability).await?;
        pack_owner::authorize_conversion_sources(
            c,
            s,
            OrganisationId::new(company).map_err(|_| ReportsError::CompanyNotFound)?,
        )
        .await
        .map_err(crate::refusal)
    }

    pub(crate) async fn conversion_on(
        &self,
        c: &Transaction<'_>,
        s: &Scope,
        company: i64,
        as_of: Date,
        pins: &ConversionPins,
    ) -> Result<Value> {
        self.conversion_access(c, s, company, VIEW).await?;
        let supplied = [
            pins.through_entry_number,
            pins.receivables_opening_batch_id,
            pins.receivables_settlement_number,
            pins.payables_opening_batch_id,
            pins.payables_settlement_number,
        ]
        .into_iter()
        .filter(Option::is_some)
        .count();
        if supplied != 0 && supplied != 5 {
            return Err(ReportsError::InvalidOption);
        }
        let opening = self.ledger.opening_context_on(c, s, company).await?;
        if as_of < opening.entry.entry_date {
            return Err(ReportsError::Ledger(
                bilibi_finance_ledger::LedgerError::Invalid(vec![
                    bilibi_finance_ledger::FieldError::new(
                        "as_of",
                        "The reporting date cannot precede the conversion date.",
                    ),
                ]),
            ));
        }
        let target = self
            .ledger
            .trial_balance_on(c, s, company, as_of, pins.through_entry_number)
            .await?;
        let through = target.through_entry_number;

        let chart = self.ledger.list_accounts_on(c, s, company).await?;
        let query = TotalsQuery {
            to: Some(as_of),
            through_entry_number: Some(through),
            ..Default::default()
        };
        let owners = self
            .ledger
            .account_totals_by_owner_on(c, s, company, &query)
            .await?;
        let ar_pins = if pins.through_entry_number.is_some() {
            bilibi_finance_receivables::BalancePins {
                through_entry_number: Some(through),
                through_opening_batch_id: pins.receivables_opening_batch_id,
                through_settlement_number: pins.receivables_settlement_number,
            }
        } else {
            Default::default()
        };
        let ap_pins = if pins.through_entry_number.is_some() {
            bilibi_finance_payables::BalancePins {
                through_entry_number: Some(through),
                through_opening_batch_id: pins.payables_opening_batch_id,
                through_settlement_number: pins.payables_settlement_number,
            }
        } else {
            Default::default()
        };
        let ar = self
            .sources
            .receivables
            .tie_out_control_accounts_on(c, s, company, as_of, &ar_pins)
            .await
            .map_err(source_error)?;
        let ap = self
            .sources
            .payables
            .tie_out_control_accounts_on(c, s, company, as_of, &ap_pins)
            .await
            .map_err(source_error)?;
        let captured_ar_pins = ar.pins();
        let captured_ap_pins = ap.pins();
        let ar_detail = self
            .sources
            .receivables
            .aged_receivables_on(
                c,
                s,
                company,
                as_of,
                &bilibi_finance_receivables::AgingOptions {
                    through_entry_number: captured_ar_pins.through_entry_number,
                    through_opening_batch_id: captured_ar_pins.through_opening_batch_id,
                    through_settlement_number: captured_ar_pins.through_settlement_number,
                    ..Default::default()
                },
            )
            .await
            .map_err(source_error)?;
        let ap_detail = self
            .sources
            .payables
            .aged_payables_on(
                c,
                s,
                company,
                as_of,
                &bilibi_finance_payables::AgingQuery {
                    through_entry_number: captured_ap_pins.through_entry_number,
                    through_opening_batch_id: captured_ap_pins.through_opening_batch_id,
                    through_settlement_number: captured_ap_pins.through_settlement_number,
                    ..Default::default()
                },
            )
            .await
            .map_err(source_error)?;
        let assets = self
            .sources
            .fixed_assets
            .register_report_on(
                c,
                s,
                company,
                as_of,
                &bilibi_finance_fixed_assets::ReportPins {
                    through_entry_number: Some(through),
                    ..Default::default()
                },
            )
            .await
            .map_err(source_error)?;
        let mut asset_snapshot = encode(&assets)?;
        for row in asset_snapshot["rows"]
            .as_array_mut()
            .ok_or(Error::Internal)?
        {
            // A conversion reconciles posted register balances. Prospective
            // depreciation projections are neither source nor posted history.
            row.as_object_mut()
                .ok_or(Error::Internal)?
                .remove("schedule");
        }
        let comparisons: Vec<_> = chart.iter().filter(|a| !a.is_group).map(|a| {
            let source_balance=opening.balances.get(&a.id).copied().unwrap_or(Money::ZERO);
            let target_balance=target.rows.iter().find(|r|r.account.id==a.id).map(|r|r.debit_balance-r.credit_balance).unwrap_or(Money::ZERO);
            json!({"account_id":a.id,"code":a.code,"name":a.name,"source_balance":source_balance,"source_debit_balance":source_balance.max(Money::ZERO),"source_credit_balance":(-source_balance).max(Money::ZERO),"target_balance":target_balance,"movement":target_balance-source_balance})
        }).collect();
        let mut evidence = serde_json::Map::new();
        let mut retained_bytes = 0usize;
        // Each source owns both authorization and its frozen explicit field lists.
        // Never silently cut a register at one evidence page.
        macro_rules! retain {
            ($key:literal,$owner:expr,$datasets:expr) => {{
                let mut datasets=serde_json::Map::new();
                for dataset in $datasets {
                    let mut after=None; let mut rows:Vec<Value>=Vec::new();
                    let fields=loop {
                        let page=$owner.evidence_page_on(c,s,company,dataset,Some(500),after).await.map_err(source_error)?;
                        for row in page.rows {
                            let row=evidence_row(&page.fields,&row)?;
                            retained_bytes=retained_bytes.checked_add(serialized_len(&row)?).ok_or(ReportsError::InvalidOption)?;
                            if retained_bytes>MAX_BYTES {return Err(ReportsError::InvalidOption);}
                            rows.push(row);
                        }
                        if page.next.is_none() {break page.fields;}
                        after=page.next;
                    };
                    datasets.insert(dataset.to_string(),json!({"fields":fields,"rows":rows,"through_id":rows.last().and_then(|r|r["id"].as_i64()).unwrap_or(0),"sha256":serialized_hash(&rows)?}));
                }
                evidence.insert($key.into(),Value::Object(datasets));
            }};
        }
        retain!(
            "ledger",
            self.ledger,
            [
                "opening_balances",
                "opening_recognition",
                "opening_recognition_replacement",
                "journal_event",
                "closing_event"
            ]
        );
        retain!(
            "receivables",
            self.sources.receivables,
            [
                "opening_batch",
                "opening_item",
                "opening_decision",
                "opening_stage",
                "setup_change"
            ]
        );
        retain!(
            "payables",
            self.sources.payables,
            [
                "opening_batch",
                "opening_item",
                "opening_decision",
                "opening_stage",
                "settings_change"
            ]
        );
        retain!(
            "fixed_assets",
            self.sources.fixed_assets,
            [
                "opening_batch",
                "opening_item",
                "opening_decision",
                "opening_stage",
                "category_revision"
            ]
        );
        retain!(
            "bank",
            self.sources.reconciliation,
            [
                "opening_bridge",
                "opening_item",
                "match_opening_item",
                "bank_account",
                "bank_account_change",
                "statement",
                "statement_line",
                "match",
                "match_entry",
                "match_event",
                "match_line",
                "reconciliation_record",
                "reconciliation_event",
                "reconciliation_item"
            ]
        );
        retain!(
            "tax",
            self.sources.sst,
            bilibi_finance_sst::Sst::evidence_datasets()
        );
        retain!(
            "payroll",
            self.sources.payroll,
            bilibi_finance_payroll::Payroll::evidence_datasets()
        );
        retain!(
            "reports",
            self,
            ["source_period", "source_period_correction"]
        );
        let mut register_coverage = serde_json::Map::new();
        let mut pending_sources = Vec::new();
        for module in ["receivables", "payables", "fixed_assets"] {
            let batches = evidence[module]["opening_batch"]["rows"]
                .as_array()
                .ok_or(Error::Internal)?;
            let items = evidence[module]["opening_item"]["rows"]
                .as_array()
                .ok_or(Error::Internal)?;
            let stages = evidence[module]["opening_stage"]["rows"]
                .as_array()
                .ok_or(Error::Internal)?;
            register_coverage.insert(module.into(),json!({"state":if batches.is_empty(){"no_opening_register_retained"}else{"opening_register_retained"},"retained_batches":batches.len(),"retained_source_rows":items.len(),"note":"Retained rows include superseded sources; the dated control report selects the effective register. Absence is not an accountant decision that the register is unnecessary."}));
            let mut pending = std::collections::BTreeSet::new();
            for stage in stages {
                let reference = stage["source_reference"].as_str().unwrap_or("");
                if !batches
                    .iter()
                    .any(|b| b["source_reference"].as_str() == Some(reference))
                {
                    pending.insert(reference);
                }
            }
            for reference in pending {
                pending_sources.push(json!({"area":module,"source_reference":reference,"reason":"Staged source parts have not been recognized as an opening register."}));
            }
        }
        let mut bank_bridges = Vec::new();
        let bank_accounts = evidence["bank"]["bank_account"]["rows"]
            .as_array()
            .ok_or(Error::Internal)?;
        let bank_records = evidence["bank"]["reconciliation_record"]["rows"]
            .as_array()
            .ok_or(Error::Internal)?;
        for account in bank_accounts {
            let bank_id = account["id"].as_i64().ok_or(Error::Internal)?;
            let ledger_id = account["ledger_account_id"]
                .as_i64()
                .ok_or(Error::Internal)?;
            let records = bank_records.iter().filter(|r| {
                r["bank_account_id"].as_i64() == Some(bank_id)
                    && r["end_date"]
                        .as_str()
                        .is_some_and(|d| d <= as_of.to_string().as_str())
            });
            let mut bridge = None;
            for record in records {
                let read = self
                    .sources
                    .reconciliation
                    .get_reconciliation_on(
                        c,
                        s,
                        company,
                        record["id"].as_i64().ok_or(Error::Internal)?,
                    )
                    .await
                    .map_err(source_error)?;
                if read.status != bilibi_finance_reconciliation::ReconciliationStatus::Abandoned
                    && bridge.as_ref().is_none_or(
                        |b: &bilibi_finance_reconciliation::BankReconciliation| {
                            (b.end_date, b.id) < (read.end_date, read.id)
                        },
                    )
                {
                    bridge = Some(read);
                }
            }
            let current_balance = target
                .rows
                .iter()
                .find(|r| r.account.id == ledger_id)
                .map(|r| r.debit_balance - r.credit_balance)
                .unwrap_or(Money::ZERO);
            let (end_balance, changed) = if let Some(bridge) = &bridge {
                let totals = self
                    .ledger
                    .account_totals_on(
                        c,
                        s,
                        company,
                        &TotalsQuery {
                            to: Some(bridge.end_date),
                            through_entry_number: Some(through),
                            ..Default::default()
                        },
                    )
                    .await?;
                let end = totals
                    .rows
                    .iter()
                    .find(|r| r.account.id == ledger_id)
                    .map(|r| r.debits - r.credits)
                    .unwrap_or(Money::ZERO);
                (
                    Some(end),
                    bridge.figures.as_ref().map(|f| end - f.book_balance),
                )
            } else {
                (None, None)
            };
            let source_boundary = self
                .sources
                .reconciliation
                .activity_boundary_on(c, s, company, bank_id, as_of)
                .await
                .map_err(source_error)?;
            let finalised_boundary = bridge
                .as_ref()
                .filter(|b| {
                    b.status == bilibi_finance_reconciliation::ReconciliationStatus::Finalised
                })
                .and_then(|b| b.figures.as_ref())
                .map(|f| {
                    json!({"through_entry_number":f.through_entry_number,"through_statement_number":f.through_statement_number,"through_match_event_number":f.through_match_event_number})
                });
            let source_activity_after_finalisation = bridge.as_ref().is_some_and(|b| {
                b.status == bilibi_finance_reconciliation::ReconciliationStatus::Finalised
                    && b.figures.as_ref().is_some_and(|f| {
                        source_boundary.through_entry_number > f.through_entry_number
                            || source_boundary.through_statement_number > f.through_statement_number
                            || source_boundary.through_match_event_number
                                > f.through_match_event_number
                    })
            });
            bank_bridges.push(json!({"account":account,"target_balance":current_balance,"bridge_end_balance":end_balance,"change_since_bridge":changed,"source_boundary":source_boundary,"finalised_boundary":finalised_boundary,"source_activity_after_finalisation":source_activity_after_finalisation,"bridge":bridge}));
        }
        let mut differences = pending_sources;
        for row in &ar.accounts {
            if !row.difference.is_zero() {
                differences.push(json!({"area":"receivables","account_id":row.account_id,"difference":row.difference}));
            }
        }
        for row in &ap.accounts {
            if row.difference != Money::ZERO {
                differences.push(json!({"area":"payables","account_id":row.account_id,"difference":row.difference}));
            }
        }
        for row in &assets.reconciliation {
            if row.difference != Money::ZERO {
                differences.push(json!({"area":"fixed_assets","account_id":row.account_id,"kind":row.kind,"difference":row.difference}));
            }
        }
        for bank in &bank_bridges {
            let incomplete = bank["bridge"].is_null()
                || bank["bridge"]["status"] != "finalised"
                || bank["bridge"]["end_date"].as_str() != Some(as_of.to_string().as_str());
            let changed = bank["change_since_bridge"]
                .as_str()
                .and_then(|v| Money::parse(v, target.minor_units).ok())
                .is_some_and(|m| m != Money::ZERO);
            let stale = bank["source_activity_after_finalisation"] == true;
            if incomplete || changed || stale {
                differences.push(if stale {
                    json!({"area":"bank","kind":"bank_bridge_stale","bank_account_id":bank["account"]["id"],"reason":"Ledger, statement or match activity through the report date was recorded after this bridge was finalised. Finalise a new reconciliation that includes it."})
                } else {
                    json!({"area":"bank","bank_account_id":bank["account"]["id"],"reason":"Review the bridge's status, end date and changes since finalisation; it does not establish a complete tie-out at the report date."})
                });
            }
        }
        for area in ["bank", "tax", "payroll"] {
            differences.push(json!({"area":area,"kind":"schedule_assessment","reason":"The accountant has not assessed this schedule's applicability or completeness."}));
        }
        let report = json!({"pins":ConversionPins{through_entry_number:Some(through),receivables_opening_batch_id:Some(ar.through_opening_batch_id),receivables_settlement_number:Some(ar.through_settlement_number),payables_opening_batch_id:Some(ap.through_opening_batch_id),payables_settlement_number:Some(ap.through_settlement_number)},"version":3,"company_id":company,"as_of":as_of.to_string(),"conversion_date":opening.entry.entry_date.to_string(),"currency_code":target.currency_code,"minor_units":target.minor_units,"through_entry_number":through,
            "opening":opening,"source_trial_balance":{"rows":comparisons,"basis":"Approved source opening journal only; signed debit less credit balances."},"target_trial_balance":target,"postings_by_owner":owners,
            "receivables":{"detail":ar_detail,"control":ar},"payables":{"detail":ap_detail,"control":ap},"assets":asset_snapshot,"bank_bridges":bank_bridges,"sources":evidence,
            "unresolved_differences":differences,"register_coverage":register_coverage,
            "excluded_history":["Transactions before conversion other than the approved opening journal and retained opening registers.","Previously settled invoice and bill history, historical asset depreciation entries and legacy bank matches are not imported."],
            "schedule_limits":["Bank evidence retains statement and match activity with finalised reconciliation bridges. Open or stale bridges require a new reconciliation; no conversion bank bridge is inferred.","Tax and payroll evidence is retained where recorded. Legacy returns, payroll year-to-date and filing completeness require the accountant's external schedules and reviewer references."],
            "schedule_reviews":{"bank":{"status":"not_assessed"},"tax":{"status":"not_assessed"},"payroll":{"status":"not_assessed"}},
            "assessment":"Unassessed. Zero numerical differences do not establish completeness or accountant approval. Other-owner control postings remain visible; opening recognition is evidence, not a reclassification."});
        if serialized_len(&report)? > MAX_BYTES {
            return Err(ReportsError::InvalidOption);
        }
        Ok(report)
    }

    pub(crate) async fn issue_conversion_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        content: Value,
    ) -> Result<Value> {
        self.conversion_access(tx.sql(), s, company, ISSUE).await?;
        bilibi_company::require_writable(tx.sql(), s, company, true)
            .await
            .map_err(|e| {
                ReportsError::Ledger(bilibi_finance_ledger::LedgerError::Invalid(vec![
                    bilibi_finance_ledger::FieldError::new("base", e.to_string()),
                ]))
            })?;
        let new = issued::NewIssued {
            tenant_id: s.tenant(),
            company_id: company,
            kind: KIND,
            implementation_version: "conversion-3",
            through_entry_number: content["through_entry_number"]
                .as_i64()
                .ok_or(Error::Internal)?,
            chart: content["target_trial_balance"]["rows"].clone(),
            layout: json!({}),
            membership: json!({}),
            content,
            issued_by_user_id: s.user(),
            issued_by_impersonator_id: s.impersonator().map(|i| i.user()),
        };
        let row = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = issued::insert(sql, &new).await?;
                    let change = Change {
                        kind: "finance.reports.issued".into(),
                        id: row.id,
                        old: Value::Null,
                        new: issued::summary(&row),
                    };
                    Ok((row, vec![change]))
                })
            })
            .await?;
        encode(&row)
    }

    pub(crate) async fn conversion_issued_on(
        &self,
        c: &(impl GenericClient + Sync),
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<Value> {
        self.conversion_access(c, s, company, VIEW).await?;
        let row = issued::find(c, s.tenant(), company, id)
            .await?
            .filter(|r| r.kind == KIND)
            .ok_or(ReportsError::ReportNotFound)?;
        let signs=c.query("SELECT to_jsonb(s) FROM finance_reports_conversion_signoffs s WHERE tenant_id=$1 AND company_id=$2 AND report_id=$3 ORDER BY id",&[&s.tenant(),&company,&id]).await.map_err(Error::from)?.iter().map(|r|r.get::<_,Value>(0)).collect::<Vec<_>>();
        Ok(json!({"report":row,"signoffs":signs}))
    }

    pub(crate) async fn sign_conversion_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        report: &Value,
        sign_off: SignOff,
    ) -> Result<Value> {
        self.conversion_access(tx.sql(), s, company, SIGN_OFF)
            .await?;
        bilibi_company::require_writable(tx.sql(), s, company, true)
            .await
            .map_err(|e| {
                ReportsError::Ledger(bilibi_finance_ledger::LedgerError::Invalid(vec![
                    bilibi_finance_ledger::FieldError::new("base", e.to_string()),
                ]))
            })?;
        let SignOff {
            reviewer_reference: reference,
            note,
            attestation,
        } = sign_off;
        let id = report["report"]["id"].as_i64().ok_or(Error::Internal)?;
        if tx.sql().query_opt("SELECT id FROM finance_reports_conversion_signoffs WHERE tenant_id=$1 AND company_id=$2 AND report_id=$3",&[&s.tenant(),&company,&id]).await.map_err(Error::from)?.is_some(){return Err(ReportsError::Ledger(bilibi_finance_ledger::LedgerError::Invalid(vec![bilibi_finance_ledger::FieldError::new("report_id","This report is already signed.")])));}
        let approval = Self::conversion_approval(report, None)?;
        self.ledger
            .check_approval(tx, s, company, &approval)
            .await?;
        let tenant = s.tenant();
        let user = s.user();
        let impersonator = s.impersonator().map(|i| i.user());
        Ok(tx.mutate(move|sql|Box::pin(async move {
            let row:Value=sql.query_one("INSERT INTO finance_reports_conversion_signoffs(tenant_id,company_id,report_id,reviewer_reference,note,attestation,signed_by_user_id,signed_by_impersonator_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING to_jsonb(finance_reports_conversion_signoffs)",&[&tenant,&company,&id,&reference,&note,&attestation,&user,&impersonator]).await?.get(0);
            let change=Change {kind:"finance.reports.conversion_signoff".into(),id:row["id"].as_i64().ok_or(Error::Internal)?,old:Value::Null,new:row.clone()};
            Ok((row,vec![change]))
        })).await?)
    }

    pub(crate) fn conversion_approval(report: &Value, reason: Option<String>) -> Result<Approval> {
        let row = &report["report"];
        let mut prepared = vec![Preparer {
            user_id: row["issued_by_user_id"].as_i64().ok_or(Error::Internal)?,
            impersonator_id: row["issued_by_impersonator_id"].as_i64(),
        }];
        let opening = &row["content"]["opening"]["prepared_by"];
        prepared.push(Preparer {
            user_id: opening["user_id"].as_i64().ok_or(Error::Internal)?,
            impersonator_id: opening["impersonator_id"].as_i64(),
        });
        if let Some(sources) = row["content"]["sources"].as_object() {
            for datasets in sources.values().filter_map(Value::as_object) {
                for dataset in datasets.values() {
                    for source in dataset["rows"].as_array().into_iter().flatten() {
                        for (person, impersonator) in [
                            ("recorded_by_user_id", "recorded_by_impersonator_id"),
                            ("prepared_by_user_id", "prepared_by_impersonator_id"),
                            ("opened_by_user_id", "opened_by_impersonator_id"),
                            ("actor_user_id", "actor_impersonator_id"),
                            ("finalised_by_user_id", "finalised_by_impersonator_id"),
                            ("imported_by_user_id", "imported_by_impersonator_id"),
                            ("changed_by_user_id", "changed_by_impersonator_id"),
                            ("matched_by_user_id", "matched_by_impersonator_id"),
                        ] {
                            if let Some(user_id) = source[person].as_i64() {
                                prepared.push(Preparer {
                                    user_id,
                                    impersonator_id: source[impersonator].as_i64(),
                                });
                            }
                        }
                    }
                }
            }
        }
        prepared.sort_by_key(|p| (p.user_id, p.impersonator_id));
        prepared.dedup_by_key(|p| (p.user_id, p.impersonator_id));
        Ok(Approval {
            source: MODULE.into(),
            action: "conversion_signoff".into(),
            subject_type: "issued_report".into(),
            subject_id: row["id"].as_i64().ok_or(Error::Internal)?,
            subject: "Conversion report sign-off".into(),
            prepared_by: prepared,
            override_reason: reason,
        })
    }
}

struct ByteCounter(usize);
impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialized value is too large"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_len(value: &impl Serialize) -> Result<usize> {
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| Error::Internal)?;
    Ok(counter.0)
}

struct DigestWriter(Sha256);
impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_hash(value: &impl Serialize) -> Result<String> {
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| Error::Internal)?;
    Ok(writer
        .0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn source_error<E: std::fmt::Display>(error: E) -> ReportsError {
    ReportsError::Ledger(bilibi_finance_ledger::LedgerError::Invalid(vec![
        bilibi_finance_ledger::FieldError::new("base", error.to_string()),
    ]))
}

// Owners may export objects or field-ordered arrays. Keep one named, explicit
// representation in the issued report without coupling to either storage.
fn evidence_row(fields: &[&str], row: &impl serde::Serialize) -> Result<Value> {
    match encode(row)? {
        Value::Array(values) if values.len() == fields.len() => Ok(Value::Object(
            fields
                .iter()
                .zip(values)
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )),
        value @ Value::Object(_) => Ok(value),
        _ => Err(ReportsError::ControlTotalsMismatch),
    }
}
