//! Download the displayed management report at its ledger cutoff.
use crate::{Column, Group, Header, Money, Period, Reports, Section};
use askama::Template;
use bilibi_artifacts::{HtmlFuture, HtmlOwner, Operation, Owner, Reference};
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use bilibi_ui::download::{Download, DownloadFuture, DownloadRequest, FileBody};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

pub(crate) struct Downloads {
    pub reports: Arc<Reports>,
}

#[derive(Template)]
#[template(
    source = r#"<!doctype html><html><head><meta charset="utf-8"><title>{{ title }}</title></head><body><h1>{{ title }}</h1><p>Management reports, not audited statements. {{ currency }}</p><table><thead><tr>{% for cell in headers %}<th>{{ cell }}</th>{% endfor %}</tr></thead><tbody>{% for row in pdf_rows %}<tr>{% for cell in row %}<td>{{ cell }}</td>{% endfor %}</tr>{% endfor %}</tbody></table></body></html>"#,
    ext = "html"
)]
struct Sheet {
    title: String,
    currency: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    pdf_rows: Vec<Vec<String>>,
}
impl Sheet {
    fn new(title: &str, h: &Header) -> Self {
        let mut headers = vec!["Account".into()];
        headers.extend(h.columns.iter().map(|c| match c {
            Column::Period { from, to } => format!(
                "{} to {}",
                bilibi_finance_ledger::dates::display(*from),
                bilibi_finance_ledger::dates::display(*to)
            ),
            Column::AsOf { as_of } => {
                format!("As of {}", bilibi_finance_ledger::dates::display(*as_of))
            }
        }));
        Self {
            title: title.into(),
            currency: h.currency_code.clone(),
            headers,
            rows: Vec::new(),
            pdf_rows: Vec::new(),
        }
    }
    fn line(&mut self, name: &str, values: &[Money], h: &Header) {
        let mut row = vec![name.into()];
        row.extend(values.iter().map(|v| v.shown(h.minor_units)));
        self.rows.push(row);
        let mut pdf_row = vec![name.into()];
        pdf_row.extend(values.iter().map(|v| v.display(h.minor_units)));
        self.pdf_rows.push(pdf_row);
    }
    fn sections(&mut self, sections: &[Section], h: &Header) {
        for section in sections {
            if let Some(name) = &section.name {
                let mut row = vec![name.clone()];
                row.resize(h.columns.len() + 1, String::new());
                self.pdf_rows.push(row.clone());
                self.rows.push(row);
            }
            for row in &section.rows {
                self.line(&format!("{} · {}", row.name, row.code), &row.amounts, h);
            }
            if let Some(name) = &section.name {
                self.line(&format!("Total {name}"), &section.totals, h);
            }
        }
    }
    fn groups(&mut self, groups: &[Group], h: &Header) {
        for group in groups {
            self.sections(&group.sections, h);
            self.line(
                &format!("Total {}", group.account_type.as_str()),
                &group.totals,
                h,
            );
        }
    }
    fn csv(&self) -> Vec<u8> {
        fn cell(s: &str, textual: bool) -> String {
            let mut text = s.to_owned();
            if textual
                && text
                    .trim_start()
                    .starts_with(['=', '+', '-', '@', '\t', '\r'])
            {
                text.insert(0, '\'');
            }
            format!("\"{}\"", text.replace('"', "\"\""))
        }
        std::iter::once(&self.headers)
            .chain(self.rows.iter())
            .map(|row| {
                row.iter()
                    .enumerate()
                    .map(|(i, v)| cell(v, i == 0))
                    .collect::<Vec<_>>()
                    .join(",")
                    + "\r\n"
            })
            .collect::<String>()
            .into_bytes()
    }
}
struct Printable {
    html: String,
}
impl Owner for Printable {
    fn owner_id(&self) -> &'static str {
        "finance/reports"
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            if !matches!(operation, Operation::Create | Operation::Read)
                || !reference
                    .is_some_and(|r| r.kind == "management-report" && r.subject == "report")
            {
                return Err(Error::Forbidden);
            }
            bilibi_company::authorize_work_target(client, scope, company.get(), crate::VIEW)
                .await
                .map(|_| ())
        })
    }
}
impl HtmlOwner for Printable {
    fn render_html<'a>(
        &'a self,
        _: &'a Scope,
        _: OrganisationId,
        _: &'a Reference,
        _: &'a Value,
    ) -> HtmlFuture<'a> {
        Box::pin(async move { Ok(self.html.clone()) })
    }
}
impl Downloads {
    async fn sheet(&self, scope: &Scope, query: &HashMap<String, String>) -> Result<Sheet, Error> {
        let company = scope.current_company().ok_or(Error::NotFound)?.get();
        let parse_date = |name: &str| {
            query
                .get(name)
                .and_then(|s| s.parse().ok())
                .ok_or(Error::Invalid)
        };
        let period = Period {
            from: parse_date("from")?,
            to: parse_date("to")?,
        };
        let through = query
            .get("through")
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n >= 0);
        let comparison = match query.get("compare").map(String::as_str) {
            Some("none") => None,
            Some("previous") => Some(crate::previous_period(period).map_err(|_| Error::Invalid)?),
            _ => Some(crate::previous_year(period).map_err(|_| Error::Invalid)?),
        };
        let periods: Vec<_> = std::iter::once(period).chain(comparison).collect();
        let dates: Vec<_> = periods.iter().map(|p| p.to).collect();
        let view = query.get("view").map(String::as_str).unwrap_or("overview");
        match view {
            "profit-and-loss" => {
                let r = self
                    .reports
                    .profit_and_loss(scope, company, &periods, through)
                    .await
                    .map_err(|_| Error::Forbidden)?;
                let mut s = Sheet::new("Profit and loss", &r.header);
                s.groups(&r.groups, &r.header);
                s.line("Net income", &r.net_income, &r.header);
                Ok(s)
            }
            "balance-sheet" => {
                let r = self
                    .reports
                    .balance_sheet(scope, company, &dates, through)
                    .await
                    .map_err(|_| Error::Forbidden)?;
                let mut s = Sheet::new("Balance sheet", &r.header);
                for group in &r.groups {
                    s.sections(&group.sections, &r.header);
                    if group.account_type == bilibi_finance_ledger::AccountType::Equity {
                        if let (Some(current), Some(earlier)) =
                            (&r.current_year_earnings, &r.earlier_years_earnings)
                        {
                            s.line("Current year earnings", current, &r.header);
                            s.line(
                                "Earlier years' earnings, not yet closed",
                                earlier,
                                &r.header,
                            );
                        } else {
                            s.line(
                                "Income less expenses not yet closed",
                                &r.earnings,
                                &r.header,
                            );
                        }
                    }
                    s.line(
                        &format!("Total {}", group.account_type.as_str()),
                        &group.totals,
                        &r.header,
                    );
                }
                s.line(
                    "Total liabilities and equity",
                    &r.total_liabilities_and_equity,
                    &r.header,
                );
                Ok(s)
            }
            "cash-position" => {
                let r = self
                    .reports
                    .cash_position(scope, company, &dates, through)
                    .await
                    .map_err(|_| Error::Forbidden)?;
                let mut s = Sheet::new("Cash position", &r.header);
                s.sections(&r.sections, &r.header);
                s.line("Total cash", &r.totals, &r.header);
                Ok(s)
            }
            "overview" => {
                let previous =
                    comparison.unwrap_or(crate::previous_year(period).map_err(|_| Error::Invalid)?);
                let r = self
                    .reports
                    .overview_compared(scope, company, period, previous, through)
                    .await
                    .map_err(|_| Error::Forbidden)?;
                let mut s = Sheet::new("Overview", &r.header);
                s.headers.push("Change".into());
                for (name, values) in [
                    ("Income", &r.income),
                    ("Expenses", &r.expense),
                    ("Net income", &r.net_income),
                    ("Assets", &r.assets),
                    ("Liabilities", &r.liabilities),
                    ("Equity", &r.equity),
                    ("Cash", &r.cash),
                ] {
                    s.line(name, values, &r.header);
                    if let Some(row) = s.rows.last_mut() {
                        row.push((values[0] - values[1]).shown(r.header.minor_units));
                    }
                    if let Some(row) = s.pdf_rows.last_mut() {
                        row.push((values[0] - values[1]).display(r.header.minor_units));
                    }
                }
                Ok(s)
            }
            _ => Err(Error::NotFound),
        }
    }
}
impl Download<Scope, Error> for Downloads {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let sheet = self.sheet(request.scope, request.query).await?;
            match request.query.get("format").map(String::as_str) {
                Some("csv") => Ok(FileBody::attachment(sheet.csv(), "text/csv", "report.csv")),
                Some("pdf" | "save") => {
                    let html = sheet.render().map_err(|_| Error::Internal)?;
                    let owner = Printable { html };
                    let company = request.scope.current_company().ok_or(Error::NotFound)?;
                    let mut client = self.reports.db.get().await?;
                    let owned = self
                        .reports
                        .artifacts
                        .for_owner(request.scope, company, &owner);
                    let artifact = Box::pin(owned.render_inline(
                        &mut client,
                        Reference {
                            subject: "report".into(),
                            kind: "management-report".into(),
                        },
                        &serde_json::json!({}),
                    ))
                    .await
                    .map_err(|_| Error::Internal)?;
                    let file = Box::pin(owned.read(&mut client, &artifact.id))
                        .await
                        .map_err(|_| Error::Internal)?;
                    if request
                        .query
                        .get("format")
                        .is_some_and(|format| format == "save")
                    {
                        Ok(FileBody::attachment(
                            file.bytes,
                            "application/pdf",
                            "report.pdf",
                        ))
                    } else {
                        Ok(FileBody::inline(
                            file.bytes,
                            "application/pdf",
                            "report.pdf",
                        ))
                    }
                }
                _ => Err(Error::NotFound),
            }
        })
    }
}
