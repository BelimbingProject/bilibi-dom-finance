//! Bilimbi Reports.AuditorPack and Web.AuditorPackController. All source reads
//! share one repeatable-read transaction; no sibling table is read here.
use crate::{authorize, json, pack_owner, Reports, ReportsError, Result, EXPORT};
use bilibi_artifacts::{Metadata, Reference};
use bilibi_database::Error;
use bilibi_finance_attachments::Attachments;
use bilibi_finance_einvoice::Einvoice;
use bilibi_finance_fixed_assets::FixedAssets;
use bilibi_finance_ledger::{EntryQuery, Ledger, Money, TotalsQuery};
use bilibi_finance_payables::{AgingQuery, Payables};
use bilibi_finance_payroll::Payroll;
use bilibi_finance_receivables::{AgingOptions, Receivables};
use bilibi_finance_reconciliation::Reconciliation;
use bilibi_finance_sst::Sst;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::Scope;
use bilibi_ui::download::{Download, DownloadFuture, DownloadRequest, FileBody};
use jiff::civil::Date;
use pack_owner::Coverage;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    sync::Arc,
};
use tokio_postgres::Transaction;
use zip::{write::SimpleFileOptions, ZipWriter};

pub(crate) struct Sources {
    pub attachments: Arc<Attachments>,
    pub receivables: Arc<Receivables>,
    pub payables: Arc<Payables>,
    pub reconciliation: Arc<Reconciliation>,
    pub einvoice: Arc<Einvoice>,
    pub payroll: Arc<Payroll>,
    pub sst: Arc<Sst>,
    pub fixed_assets: Arc<FixedAssets>,
}

/// Private artifact delivery; the filename is derived from its trusted reference.
pub struct AuditorPack {
    pub bytes: Vec<u8>,
    pub filename: String,
}

impl Reports {
    /// Collects the complete ledger, trial balance and issued reports at one
    /// snapshot. Adds all Finance source evidence and aging only when every
    /// source view is authorized; otherwise the manifest names the omissions.
    /// Publication and download recheck the stored coverage's grants.
    pub async fn export_auditor_pack(
        &self,
        scope: &Scope,
        company: i64,
        as_of: Date,
    ) -> Result<Metadata> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, EXPORT).await?;
        let company_id = OrganisationId::new(company).map_err(|_| ReportsError::CompanyNotFound)?;
        // Prove the ledger before collecting; source grants choose coverage,
        // never which entries or totals the person is allowed to see.
        Coverage::select(&**client, scope, company_id)
            .await
            .map_err(crate::refusal)?;
        let max = self
            .settings
            .get_global(&**client, "artifacts.max_bytes")
            .await?
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(ReportsError::Artifact("policy"))?;
        // Authz records every decision on this connection (base/authz), so
        // this snapshot permits its access logs; no business records are written.
        let tx = (**client)
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await
            .map_err(Error::from)?;
        authorize(&tx, scope, company, EXPORT).await?;
        let coverage = Coverage::select(&tx, scope, company_id)
            .await
            .map_err(crate::refusal)?;
        let mut files = Files::new()?;
        let manifest = self
            .collect(&tx, scope, company, as_of, coverage, &mut files)
            .await?;
        files.write_json("manifest.json", &manifest, 1)?;
        tx.commit().await.map_err(snapshot_error)?;
        let bytes = files.archive(max)?;
        self.artifacts
            .for_owner(
                scope,
                OrganisationId::new(company).map_err(|_| ReportsError::CompanyNotFound)?,
                &pack_owner::ArtifactOwner,
            )
            .put(
                &mut client,
                Reference {
                    subject: uuid::Uuid::new_v4().to_string(),
                    kind: coverage.kind().into(),
                },
                &bytes,
                "application/zip",
            )
            .await
            .map_err(artifact_error)
    }

    /// Every read asks for export and the stored pack's source grants again,
    /// proves the live company and reads through Base Artifacts (expiry, hash, audit).
    pub async fn read_auditor_pack(
        &self,
        scope: &Scope,
        company: i64,
        artifact_id: &str,
    ) -> Result<AuditorPack> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, EXPORT).await?;
        let document = self
            .artifacts
            .for_owner(
                scope,
                OrganisationId::new(company).map_err(|_| ReportsError::CompanyNotFound)?,
                &pack_owner::ArtifactOwner,
            )
            .read(&mut client, artifact_id)
            .await
            .map_err(artifact_error)?;
        Ok(AuditorPack {
            filename: format!(
                "{}-{}.zip",
                document.metadata.reference.kind, document.metadata.reference.subject
            ),
            bytes: document.bytes,
        })
    }

    async fn collect(
        &self,
        tx: &Transaction<'_>,
        scope: &Scope,
        company: i64,
        as_of: Date,
        coverage: Coverage,
        files: &mut Files,
    ) -> Result<Value> {
        let totals = self
            .ledger
            .account_totals_on(tx, scope, company, &TotalsQuery::default())
            .await?;
        let through = totals.through_entry_number;
        let snapshot = tx.query_one("SELECT pg_current_snapshot()::text,to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')", &[]).await.map_err(snapshot_error)?;
        let trial = self
            .ledger
            .trial_balance_on(tx, scope, company, as_of, Some(through))
            .await?;
        let debit: Money = totals.rows.iter().map(|r| r.debits).sum();
        let credit: Money = totals.rows.iter().map(|r| r.credits).sum();
        let (gl_debit, gl_credit) = self
            .general_ledger(tx, scope, company, through, files)
            .await?;
        if (debit, credit) != (gl_debit, gl_credit) {
            return Err(ReportsError::ControlTotalsMismatch);
        }
        let trial = json(&trial)?;

        files.write_json(
            "trial-balance.json",
            &trial,
            trial["rows"].as_array().map_or(0, Vec::len),
        )?;
        files.csv(
            "trial-balance.csv",
            &[
                "as_of",
                "through_entry_number",
                "id",
                "code",
                "name",
                "account_type",
                "parent_id",
                "debits",
                "credits",
                "debit_balance",
                "credit_balance",
            ],
            &summary_rows(&trial["rows"], as_of, through),
        )?;
        let mut balance_pins = Value::Null;
        if coverage == Coverage::All {
            let ar = self
                .sources
                .receivables
                .aged_receivables_on(tx, scope, company, as_of, &AgingOptions::default())
                .await
                .map_err(source_error)?;
            let ap = self
                .sources
                .payables
                .aged_payables_on(tx, scope, company, as_of, &AgingQuery::default())
                .await
                .map_err(source_error)?;
            balance_pins = json!({"as_of":as_of.to_string(),"through_entry_number":through,"receivables_opening_batch_id":ar.through_opening_batch_id,"receivables_settlement_number":ar.through_settlement_number,"payables_opening_batch_id":ap.through_opening_batch_id,"payables_settlement_number":ap.through_settlement_number});
            let ar = json(&ar)?;
            let ap = json(&ap)?;
            let ar_rows = aging_rows(&ar, true);
            let ap_rows = aging_rows(&ap, false);
            files.write_json("aged-receivables.json", &ar, ar_rows.len())?;
            files.write_json("aged-payables.json", &ap, ap_rows.len())?;
            files.csv(
                "aged-receivables.csv",
                &[
                    "as_of",
                    "through_entry_number",
                    "through_opening_batch_id",
                    "through_settlement_number",
                    "kind",
                    "balance_side",
                    "control_account_id",
                    "customer_id",
                    "customer_code",
                    "customer_name",
                    "document_id",
                    "document_reference",
                    "party_name",
                    "issue_date",
                    "due_date",
                    "total",
                    "outstanding",
                    "days_overdue",
                    "bucket",
                ],
                &with_pin(ar_rows, as_of, through),
            )?;
            files.csv(
                "aged-payables.csv",
                &[
                    "as_of",
                    "through_entry_number",
                    "through_opening_batch_id",
                    "through_settlement_number",
                    "kind",
                    "balance_side",
                    "control_account_id",
                    "id",
                    "document_number",
                    "party_key",
                    "party_name",
                    "party_document_number",
                    "document_date",
                    "due_date",
                    "currency_code",
                    "total",
                    "outstanding",
                    "days_overdue",
                    "bucket",
                ],
                &with_pin(ap_rows, as_of, through),
            )?;
        }
        // Each owner keeps its schema and frozen field catalog private. Only
        // a bounded page is resident, on exactly the transaction passed here.
        macro_rules! evidence {
            ($name:literal, $owner:expr, $datasets:expr) => {
                for dataset in $datasets {
                    let name = format!("{}-{}.csv", $name, dataset);
                    let mut writer = files.csv_writer(&name)?;
                    let mut after = None;
                    loop {
                        let page = $owner
                            .evidence_page_on(tx, scope, company, dataset, None, after)
                            .await
                            .map_err(source_error)?;
                        let mut fields = page.fields;
                        let mut rows: Vec<Value> = page
                            .rows
                            .into_iter()
                            .map(|r| json(&r))
                            .collect::<Result<_>>()?;
                        if $name == "attachments" && dataset == "attachment" {
                            fields.push("import_mapping_ref");
                            for row in &mut rows {
                                let reference =
                                    if row["source"] == "finance/reconciliation/statement" {
                                        json!(format!(
                                            "reconciliation-statement.csv#{}",
                                            row["document_id"]
                                        ))
                                    } else {
                                        Value::Null
                                    };
                                row["import_mapping_ref"] = reference;
                            }
                        }
                        writer.page(&fields, &rows)?;
                        if $name == "reconciliation" && dataset == "statement" {
                            files.statement_sources(&rows)?;
                        }
                        if $name == "einvoice" && dataset == "submission" {
                            files.payload_sources(&rows)?;
                        }
                        match page.next {
                            Some(next) => after = Some(next),
                            None => break,
                        }
                    }
                    files.finish_csv(&name, writer);
                }
            };
        }
        evidence!("ledger", self.ledger, Ledger::evidence_datasets());
        if coverage == Coverage::All {
            evidence!(
                "receivables",
                self.sources.receivables,
                Receivables::evidence_datasets()
            );
            evidence!(
                "payables",
                self.sources.payables,
                self.sources.payables.evidence_datasets()
            );
            evidence!(
                "reconciliation",
                self.sources.reconciliation,
                Reconciliation::evidence_datasets()
            );
            evidence!(
                "einvoice",
                self.sources.einvoice,
                Einvoice::evidence_datasets()
            );
            evidence!(
                "payroll",
                self.sources.payroll,
                Payroll::evidence_datasets().iter().copied()
            );
            evidence!("sst", self.sources.sst, Sst::evidence_datasets());
            evidence!(
                "fixed_assets",
                self.sources.fixed_assets,
                self.sources.fixed_assets.evidence_datasets()
            );
            evidence!(
                "attachments",
                self.sources.attachments,
                Attachments::evidence_datasets().iter().copied()
            );
        }
        if coverage == Coverage::All {
            for (dataset, file) in [
                ("source_period", "reports-source-period.csv"),
                (
                    "source_period_correction",
                    "reports-source-period-correction.csv",
                ),
            ] {
                let mut writer = files.csv_writer(file)?;
                let mut after = None;
                loop {
                    let page = self
                        .evidence_page_on(tx, scope, company, dataset, None, after)
                        .await?;
                    writer.page(&page.fields, &page.rows)?;
                    match page.next {
                        Some(next) => after = Some(next),
                        None => break,
                    }
                }
                files.finish_csv(file, writer);
            }
        }
        // Reports reads its own issued evidence through the same facade contract.
        let mut issued = files.csv_writer("issued-reports.csv")?;
        let mut after = None;
        loop {
            let page = if coverage == Coverage::All {
                self.evidence_page_on(tx, scope, company, "issued", Some(1), after)
                    .await?
            } else {
                // Conversion snapshots contain every source's data. A ledger-only
                // pack explicitly excludes them instead of weakening their grants.
                crate::issued::page(
                    tx,
                    scope.tenant(),
                    company,
                    "issued",
                    Some(1),
                    after,
                    Some(crate::conversion::KIND),
                )
                .await?
            };
            issued.page(&page.fields, &page.rows)?;
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        files.finish_csv("issued-reports.csv", issued);
        if coverage == Coverage::All {
            evidence!("reports", self, ["conversion_signoff"]);
        }
        files.finish_sources()?;
        Ok(json!({
            "generator":"finance/reports/auditor-pack", "generator_version":"11",
            "tenant_id":scope.tenant(), "company_id":company,
            "currency_code":totals.currency_code, "minor_units":totals.minor_units,
            "generated_by_user_id":scope.user(), "generated_by_impersonator_id":scope.impersonator().map(|a|a.user()),
            "captured_at":snapshot.get::<_,String>(1), "timezone":"Etc/UTC", "database_snapshot":snapshot.get::<_,String>(0),
            "through_entry_number":through, "balance_pins":balance_pins,
            "coverage": if coverage == Coverage::All { "all-finance" } else { "ledger-and-reports" },
            "excluded_sources": if coverage == Coverage::All { Vec::<&str>::new() } else { vec!["receivables", "payables", "reconciliation", "einvoice", "payroll", "sst", "fixed_assets", "attachments", "reports.source_period", "reports.source_period_correction", "conversion reports and sign-offs"] },
            "coverage_notice": if coverage == Coverage::All {
                "All Finance source datasets are included."
            } else {
                "Complete ledger, trial balance and issued reports only. Payroll source records, source-period records and other subledger/source evidence are excluded. Payroll journal lines remain in the ledger. This is not a complete audit evidence pack."
            },
            "filters":{"evidence": if coverage == Coverage::All { "all Finance company records, including drafts and reversals" } else { "all ledger company records and issued reports, including drafts and reversals" }, "trial_balance_and_aging_as_of":as_of.to_string()},
            "decimal_encoding":"exact decimal strings; CSV numeric cells are unrounded",
            "csv_text_encoding":"UTF-8; formula-like text is prefixed with an apostrophe",
            "source_files": if coverage == Coverage::All {
                "Source file bytes (bank statements, supporting documents) are not in the pack; the source-file and attachment indexes carry their SHA-256; authority payloads are included."
            } else {
                "Source evidence is excluded. The source-file index is empty; no attachment index or authority payloads are included."
            },
            "report_notice":"Management reports for the company's own use, not statutory financial statements.",
            "files":files.metadata, "control_totals":{"debits":debit,"credits":credit}
        }))
    }

    async fn general_ledger(
        &self,
        tx: &Transaction<'_>,
        scope: &Scope,
        company: i64,
        through: i64,
        files: &mut Files,
    ) -> Result<(Money, Money)> {
        let chart: BTreeMap<_, _> = self
            .ledger
            .list_accounts_on(tx, scope, company)
            .await?
            .into_iter()
            .map(|a| (a.id, a))
            .collect();
        let fields = [
            "entry_number",
            "entry_id",
            "entry_date",
            "currency_code",
            "entry_description",
            "owner_module",
            "owner_record",
            "reference",
            "posted_by_user_id",
            "posted_by_impersonator_id",
            "posted_at",
            "reverses_entry_id",
            "line_number",
            "account_id",
            "account_code",
            "account_name",
            "account_type",
            "description",
            "debit",
            "credit",
        ];
        let mut writer = files.csv_writer("general-ledger.csv")?;
        let mut after = None;
        let (mut debits, mut credits) = (Money::ZERO, Money::ZERO);
        loop {
            let entries = self
                .ledger
                .list_entries_on(
                    tx,
                    scope,
                    company,
                    &EntryQuery {
                        limit: Some(500),
                        after,
                        through_entry_number: Some(through),
                        ..EntryQuery::default()
                    },
                )
                .await?;
            let mut rows = Vec::new();
            for entry in &entries {
                for line in &entry.lines {
                    let account = chart
                        .get(&line.account_id)
                        .ok_or(ReportsError::ControlTotalsMismatch)?;
                    let mut row = json(entry)?;
                    let map = row
                        .as_object_mut()
                        .ok_or(ReportsError::ControlTotalsMismatch)?;
                    map.remove("lines");
                    map.extend(
                        json(line)?
                            .as_object()
                            .ok_or(ReportsError::ControlTotalsMismatch)?
                            .clone(),
                    );
                    map.extend(json!({"entry_id":entry.id,"entry_description":entry.description,"owner_module":entry.owner.as_ref().map(|o|&o.module),"owner_record":entry.owner.as_ref().map(|o|&o.record),"account_code":account.code,"account_name":account.name,"account_type":account.account_type}).as_object().unwrap().clone());
                    rows.push(row);
                    debits = debits + line.debit;
                    credits = credits + line.credit;
                }
            }
            writer.page(&fields, &rows)?;
            if entries.len() < 500 {
                break;
            }
            after = entries.last().map(|e| (e.entry_date, e.id));
        }
        files.finish_csv("general-ledger.csv", writer);
        if let Some(meta) = files.metadata.get_mut("general-ledger.csv") {
            meta["debits"] = json!(debits);
            meta["credits"] = json!(credits);
        }
        Ok((debits, credits))
    }
}

fn aging_rows(value: &Value, receivables: bool) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Some(groups) = value[if receivables { "customers" } else { "parties" }].as_array() {
        for group in groups {
            for (stream, credit) in [("documents", false), ("credits", true)] {
                if let Some(documents) = group[stream].as_array() {
                    for document in documents {
                        let mut row = document.clone();
                        row["balance_side"] = json!(if credit == receivables {
                            "credit"
                        } else {
                            "debit"
                        });
                        if receivables {
                            row["customer_id"] = group["customer_id"].clone();
                            row["customer_code"] = group["code"].clone();
                            row["customer_name"] = group["name"].clone();
                        }
                        rows.push(row);
                    }
                }
            }
        }
    }
    if let Some(exceptions) = value["exceptions"].as_array() {
        rows.extend(exceptions.iter().cloned());
    }
    for row in &mut rows {
        row["through_opening_batch_id"] = value["through_opening_batch_id"].clone();
        row["through_settlement_number"] = value["through_settlement_number"].clone();
    }
    rows
}

fn summary_rows(rows: &Value, as_of: Date, through: i64) -> Vec<Value> {
    with_pin(rows.as_array().cloned().unwrap_or_default(), as_of, through)
}
fn with_pin(mut rows: Vec<Value>, as_of: Date, through: i64) -> Vec<Value> {
    for row in &mut rows {
        row["as_of"] = json!(as_of.to_string());
        row["through_entry_number"] = json!(through);
    }
    rows
}

// Disk-backed CSVs keep the bounded evidence contract meaningful. The private
// random directory and all files are removed on success, refusal or cancellation.
struct Files {
    directory: PathBuf,
    metadata: BTreeMap<String, Value>,
    sources: Option<Csv>,
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
impl Files {
    fn new() -> Result<Self> {
        let directory =
            std::env::temp_dir().join(format!("finance-auditor-pack-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(storage_error)?;
        let mut files = Self {
            directory,
            metadata: BTreeMap::new(),
            sources: None,
        };
        let mut index = files.csv_writer("source-file-index.csv")?;
        index.page(
            &["owner", "record_id", "filename", "sha256", "availability"],
            &[],
        )?;
        files.sources = Some(index);
        Ok(files)
    }
    fn file(&self, name: &str) -> Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.directory.join(name))
            .map_err(storage_error)
    }
    fn csv_writer(&self, name: &str) -> Result<Csv> {
        Ok(Csv {
            file: self.file(name)?,
            hash: Sha256::new(),
            rows: 0,
            header: false,
        })
    }
    fn finish_csv(&mut self, name: &str, csv: Csv) {
        self.metadata.insert(
            name.into(),
            json!({"row_count":csv.rows,"sha256":hex(&csv.hash.finalize())}),
        );
    }
    fn csv(&mut self, name: &str, fields: &[&str], rows: &[Value]) -> Result<()> {
        let mut csv = self.csv_writer(name)?;
        csv.page(fields, rows)?;
        self.finish_csv(name, csv);
        Ok(())
    }
    fn write_json(&mut self, name: &str, value: &Value, count: usize) -> Result<()> {
        self.write_bytes(
            name,
            &serde_json::to_vec_pretty(value).map_err(|_| ReportsError::Failed(Error::Internal))?,
            Some(count),
        )
    }
    fn write_bytes(&mut self, name: &str, bytes: &[u8], count: Option<usize>) -> Result<()> {
        self.file(name)?.write_all(bytes).map_err(storage_error)?;
        let mut metadata = json!({"sha256":hash(bytes),"byte_size":bytes.len()});
        if let Some(count) = count {
            metadata["row_count"] = json!(count)
        }
        self.metadata.insert(name.into(), metadata);
        Ok(())
    }
    fn statement_sources(&mut self, rows: &[Value]) -> Result<()> {
        for row in rows {
            self.source(json!({"owner":"finance/reconciliation","record_id":row["id"],"filename":row["file_name"],"sha256":row["file_sha256"],"availability":"not in pack; see attachments-attachment.csv"}))?;
        }
        Ok(())
    }
    fn payload_sources(&mut self, rows: &[Value]) -> Result<()> {
        for row in rows {
            let payload = row["payload"]
                .as_str()
                .ok_or(ReportsError::SourceIntegrityFailure)?;
            if row["payload_sha256"] != hash(payload.as_bytes()) {
                return Err(ReportsError::SourceIntegrityFailure);
            }
            let id = row["id"]
                .as_i64()
                .ok_or(ReportsError::SourceIntegrityFailure)?;
            let name = format!("authority-payload-{id}.json");
            self.write_bytes(&name, payload.as_bytes(), None)?;
            self.source(json!({"owner":"finance/einvoice","record_id":id,"filename":name,"sha256":row["payload_sha256"],"availability":"included"}))?;
        }
        Ok(())
    }
    fn source(&mut self, row: Value) -> Result<()> {
        self.sources
            .as_mut()
            .ok_or(ReportsError::SourceIntegrityFailure)?
            .page(
                &["owner", "record_id", "filename", "sha256", "availability"],
                &[row],
            )
    }
    fn finish_sources(&mut self) -> Result<()> {
        let csv = self
            .sources
            .take()
            .ok_or(ReportsError::SourceIntegrityFailure)?;
        self.finish_csv("source-file-index.csv", csv);
        Ok(())
    }
    fn archive(&self, max: u64) -> Result<Vec<u8>> {
        let mut zip = ZipWriter::new(self.file("auditor-pack.zip")?);
        for name in self.metadata.keys() {
            zip.start_file(name, SimpleFileOptions::default().unix_permissions(0o600))
                .map_err(|_| ReportsError::Artifact("zip"))?;
            let mut input = File::open(self.directory.join(name)).map_err(storage_error)?;
            std::io::copy(&mut input, &mut zip).map_err(storage_error)?;
        }
        let file = zip.finish().map_err(|_| ReportsError::Artifact("zip"))?;
        if file.metadata().map_err(storage_error)?.len() > max {
            return Err(ReportsError::InvalidSize);
        }
        fs::read(self.directory.join("auditor-pack.zip")).map_err(storage_error)
    }
}
struct Csv {
    file: File,
    hash: Sha256,
    rows: usize,
    header: bool,
}
impl Csv {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.write_all(bytes).map_err(storage_error)?;
        self.hash.update(bytes);
        Ok(())
    }
    fn page(&mut self, fields: &[&str], rows: &[Value]) -> Result<()> {
        if !self.header {
            self.bytes(format!("{}\r\n", fields.join(",")).as_bytes())?;
            self.header = true;
        }
        for row in rows {
            let values: Vec<_> = fields
                .iter()
                .enumerate()
                .map(|(i, field)| {
                    if row.is_array() {
                        &row[i]
                    } else {
                        &row[*field]
                    }
                })
                .collect();
            self.bytes(csv_row(fields, &values).as_bytes())?;
            self.rows += 1;
        }
        Ok(())
    }
}
fn csv_row(fields: &[&str], values: &[&Value]) -> String {
    let mut cells = Vec::with_capacity(values.len());
    for (field, value) in fields.iter().zip(values) {
        let text = match value {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let unsafe_text = matches!(text.chars().next(), Some('\t' | '\r' | '\n'))
            || text
                .trim_start_matches(|c: char| c.is_whitespace() || c.is_control())
                .starts_with(['=', '+', '-', '@']);
        // Owners export exact numeric strings. Only known numeric fields
        // may bypass text protection; a numeric-looking account code may not.
        let numeric = matches!(
            *field,
            "debit"
                | "credit"
                | "debits"
                | "credits"
                | "debit_balance"
                | "credit_balance"
                | "total"
                | "outstanding"
                | "quantity"
                | "rate"
                | "tax_rate"
                | "net"
                | "tax"
                | "unit_price"
                | "amount"
                | "subtotal"
                | "tax_total"
                | "total_net"
                | "total_tax"
                | "old_rate"
                | "new_rate"
                | "exchange_rate"
                | "annual_rate"
                | "quantity_tolerance"
                | "price_tolerance"
                | "old_quantity_tolerance"
                | "new_quantity_tolerance"
                | "old_price_tolerance"
                | "new_price_tolerance"
                | "billed_unit_price"
                | "ordered_unit_price"
                | "book_balance"
                | "statement_balance"
                | "opening_balance"
                | "closing_balance"
                | "running_balance"
                | "difference"
                | "tax_payable"
                | "ledger_debits"
                | "ledger_credits"
                | "document_debits"
                | "document_credits"
                | "cost"
                | "residual_value"
                | "proceeds"
                | "accumulated"
        ) || field.ends_with("_amount")
            || field.ends_with("_quantity");
        let number = numeric && text.parse::<rust_decimal::Decimal>().is_ok();
        let prefix = if value.is_string() && unsafe_text && !number {
            "'"
        } else {
            ""
        };
        cells.push(format!("\"{prefix}{}\"", text.replace('"', "\"\"")));
    }
    format!("{}\r\n", cells.join(","))
}
fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn storage_error(_: std::io::Error) -> ReportsError {
    ReportsError::Artifact("storage")
}
fn source_error(error: impl std::fmt::Display) -> ReportsError {
    // Source facades already authorized on the snapshot. An independent owner
    // callback reauthorizes against current grants before publication/download.
    let _ = error;
    ReportsError::Artifact("source_read")
}
fn snapshot_error(error: tokio_postgres::Error) -> ReportsError {
    if error.code() == Some(&tokio_postgres::error::SqlState::T_R_SERIALIZATION_FAILURE) {
        ReportsError::SnapshotChanged
    } else {
        ReportsError::Failed(Error::from(error))
    }
}
fn artifact_error(error: bilibi_artifacts::Error) -> ReportsError {
    match error {
        bilibi_artifacts::Error::Database(Error::Forbidden | Error::Unauthorized) => {
            ReportsError::Unauthorized
        }
        other => ReportsError::Artifact(other.code()),
    }
}

pub(crate) struct Downloads {
    pub reports: Arc<Reports>,
}
impl Download<Scope, Error> for Downloads {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "auditor-pack" {
                return Err(Error::NotFound);
            }
            let company = request
                .scope
                .current_company()
                .ok_or(Error::NotFound)?
                .get();
            let id = request.path.get("artifact_id").ok_or(Error::NotFound)?;
            match self
                .reports
                .read_auditor_pack(request.scope, company, id)
                .await
            {
                Ok(pack) => Ok(FileBody::attachment(
                    pack.bytes,
                    "application/zip",
                    &pack.filename,
                )),
                Err(ReportsError::Unauthorized) => Err(Error::Forbidden),
                Err(ReportsError::Failed(error)) => Err(error),
                Err(_) => Err(Error::NotFound),
            }
        })
    }
}

// Bilimbi ReportsLive's refusal copy, including recoverable storage failures.
pub(crate) fn export_refusal(error: &ReportsError) -> &'static str {
    match error {
        ReportsError::SnapshotChanged => {
            "The company changed while the pack was being generated. Generate it again."
        }
        ReportsError::Unauthorized => "You do not have permission to export this company’s ledger.",
        ReportsError::InvalidSize => {
            "The archive exceeds the configured download size limit. Ask your administrator to increase it."
        }
        ReportsError::Artifact("policy") => {
            "Document retention has not been configured. Ask your administrator to configure downloads."
        }
        _ => {
            "The auditor pack could not be generated. Retry or ask your administrator to check document storage."
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
