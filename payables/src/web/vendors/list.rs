//! The Suppliers list (one row per supplier, its terms, accounts, tax and
//! status editable in place), with the page's header, search, pagination and
//! company drop zone.
use super::Vendors;
use crate::web::{company, render};
use crate::Vendor;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::*;
use serde_json::{json, Value};

impl Vendors {
    pub(in crate::web) async fn body(&self, s: &Scope) -> Result<String> {
        let mut actions = Html::EMPTY;
        if self.can_manage {
            actions = render(CreateButton {
                noun: "Supplier",
                id: "vendor-add",
                event: "show_vendor",
                ..CreateButton::default()
            })?;
        }
        let mut body = render(Header {
            title: Html::text("Suppliers"),
            subtitle: "",
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        if let Some((kind, message)) = &self.notice {
            body.push(&render(Alert {
                id: "vendors-notice",
                kind: *kind,
                body: Html::text(message),
                ..Alert::default()
            })?);
        }
        let (accounts, taxes) = self.choices(s).await?;
        let mut rows = vec![];
        body.push(&render(FilterToolbar::new(
            "vendor-filters",
            vec![ToolbarControl::search(
                "vendor-search",
                "search",
                "Search suppliers",
                &self.list.search,
                "Search suppliers",
            )],
        ))?);
        let search = self.list.search.to_lowercase();
        let filtered: Vec<_> = self
            .all
            .iter()
            .filter(|v| {
                v.code.to_lowercase().contains(&search)
                    || v.party.name.to_lowercase().contains(&search)
            })
            .collect();
        let total = filtered.len() as u64;
        let start = ((self.list.page.saturating_sub(1)) * self.list.page_size) as usize;
        for v in filtered
            .into_iter()
            .skip(start)
            .take(self.list.page_size as usize)
        {
            rows.push(self.row(s, v, &accounts, &taxes).await?);
        }
        let mut table = FlexTable::new("payables-vendors-table", self.columns.flex_columns(), rows);
        table.caption = "Suppliers";
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(render(EmptyState::missing(
            "No suppliers yet",
            "Add a supplier before recording a bill.",
        ))?));
        body.push(&render(table)?);
        body.push(&render(Pagination::new(
            "vendor-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &[25, 50, 100],
        ))?);
        if let Some(create) = &self.create {
            body.push(&self.create_dialog(create)?);
        }
        let html = render(bilibi_ui::Page {
            id: "payables-vendors",
            variant: PageVariant::List,
            class: "",
            body,
        })?;
        // Companies and contacts dropped on the page open the same dialog as
        // "Add vendor", so the tile accepts them only while the button shows.
        let zone = self
            .pages
            .records
            .drop_target(crate::web::VENDOR_TARGET)
            .filter(|_| self.can_manage);
        Ok(match zone {
            Some(target) => target
                .zone()
                .wrap("payables-vendors-drop", html)
                .as_str()
                .to_owned(),
            None => html.as_str().to_owned(),
        })
    }

    /// The company's postable accounts and its tax codes, for the editors in each row.
    async fn choices(&self, s: &Scope) -> Result<(Vec<Value>, Vec<Value>)> {
        Ok(if let Some(company) = company(s) {
            let mut input = json!({"company_id":company,"page_size":100});
            let mut accounts = Vec::new();
            loop {
                let a = self
                    .read(s, "finance.ledger.account.list", input.clone())
                    .await;
                let a = match a {
                    Ok(a) => a,
                    Err(Error::Forbidden | Error::Unauthorized) => break,
                    Err(e) => return Err(e),
                };
                for row in a["records"].as_array().into_iter().flatten() {
                    if row["active"] == true && row["is_group"] != true {
                        accounts.push(row.clone());
                    }
                }
                if a["next_after_code"].is_null() {
                    break;
                }
                input["after_code"] = a["next_after_code"].clone();
            }
            let mut tax_input = json!({"company_id":company,"page_size":100});
            let mut taxes = Vec::new();
            loop {
                let page = self
                    .read(s, "finance.payables.tax_code.list", tax_input.clone())
                    .await?;
                taxes.extend(page["records"].as_array().into_iter().flatten().cloned());
                if page["next_after_id"].is_null() {
                    break;
                }
                tax_input["after_id"] = page["next_after_id"].clone();
            }
            (accounts, taxes)
        } else {
            (vec![], vec![])
        })
    }

    /// One supplier's row: its company link and facts, and the editors for its terms, accounts, tax and status.
    async fn row(
        &self,
        s: &Scope,
        v: &Vendor,
        accounts: &[Value],
        taxes: &[Value],
    ) -> Result<FlexRow> {
        let mut row = FlexRow::new(v.id.to_string()).cell("code", Cell::text(&v.code));
        let mut company_body = render(ActionLink {
            href: &v.party.path,
            body: Html::text(&v.party.name),
            ..ActionLink::default()
        })?;
        if self.columns.mode() != RowMode::Compact {
            let mut facts = vec![];
            for (key, label, value) in [
                ("tax_id", "Tax ID", &v.party.tax_id),
                (
                    "registration_number",
                    "Registration number",
                    &v.party.registration_number,
                ),
                ("email", "Email", &v.party.email),
                ("phone", "Phone", &v.party.phone),
            ] {
                let value = if v.party.restricted.iter().any(|r| r == key) {
                    render(Restricted::new(format!("vendor-{}-{key}-restricted", v.id)))?
                } else {
                    Html::text(value.as_deref().unwrap_or("—"))
                };
                facts.push(Fact {
                    title: label,
                    value,
                    ..Fact::default()
                });
            }
            let billing = match self
                .read(
                    s,
                    if v.party.kind == "person" {
                        "core.address.person.list"
                    } else {
                        "core.address.company.list"
                    },
                    if v.party.kind == "person" {
                        json!({"person_id":v.party.id})
                    } else {
                        json!({"company_id":v.party.id})
                    },
                )
                .await
            {
                Ok(rows) => {
                    let lines = bilibi_finance_ledger::party_billing_lines(
                        &rows,
                        &self.pages.today(s).await?.to_string(),
                    );
                    Html::text(&if lines.is_empty() {
                        "No billing address yet".into()
                    } else {
                        lines.join(", ")
                    })
                }
                Err(Error::Forbidden | Error::Unauthorized) => render(
                    Restricted::new(format!("vendor-{}-billing-restricted", v.id))
                        .reason("Viewing addresses needs permission."),
                )?,
                Err(e) => return Err(e),
            };
            facts.push(Fact {
                title: "Billing address",
                value: billing,
                ..Fact::default()
            });
            company_body.push(&render(Fold {
                id: &format!("vendor-{}-company-details", v.id),
                title: "Company details",
                summary: "",
                open: false,
                body: render(Facts {
                    id: &format!("vendor-{}-facts", v.id),
                    items: facts,
                })?,
            })?);
        }
        row = row.slot("company", company_body);
        row = row.slot(
            "terms",
            render(InlineEdit {
                field: self.inline(
                    v,
                    "payment_terms_days",
                    "Payment terms",
                    v.payment_terms_days.to_string(),
                ),
            })?,
        );
        for (key, label, current, kind) in [
            (
                "payable_account_id",
                "Payable account",
                v.payable_account_id,
                "liability",
            ),
            (
                "expense_account_id",
                "Default expense",
                v.expense_account_id,
                "expense",
            ),
        ] {
            let choices = accounts
                .iter()
                .filter(|a| {
                    a["account_type"] == kind || (kind == "expense" && a["account_type"] == "asset")
                })
                .map(|a| {
                    Choice::new(a["name"].as_str().unwrap_or(""), a["id"].to_string())
                        .detail(a["code"].as_str().unwrap_or(""))
                })
                .collect();
            let display = accounts
                .iter()
                .find(|a| a["id"].as_i64() == current)
                .map(|a| {
                    Html::text(&format!(
                        "{} · {}",
                        a["name"].as_str().unwrap_or(""),
                        a["code"].as_str().unwrap_or("")
                    ))
                })
                .unwrap_or_else(|| {
                    Html::text(&current.map_or_else(
                        || {
                            if kind == "liability" {
                                "Company default"
                            } else {
                                "No default"
                            }
                            .into()
                        },
                        |id| format!("Account {id}"),
                    ))
                });
            let editor = render(
                Combobox::new(format!("vendor-{}-{key}-input", v.id), key)
                    .aria_label(label)
                    .value(current.map(|i| i.to_string()).unwrap_or_default())
                    .options(choices),
            )?;
            row = row.slot(
                if kind == "liability" {
                    "payable"
                } else {
                    "expense"
                },
                render(InlineChoice {
                    field: self.inline(
                        v,
                        key,
                        label,
                        current.map(|i| i.to_string()).unwrap_or_default(),
                    ),
                    display,
                    editor,
                })?,
            );
        }
        let options = taxes
            .iter()
            .filter(|t| t["active"] == true)
            .map(|t| {
                Choice::new(t["name"].as_str().unwrap_or(""), t["id"].to_string())
                    .detail(t["code"].as_str().unwrap_or(""))
            })
            .collect();
        row = row.slot(
            "tax",
            render(InlineChoice {
                field: self.inline(
                    v,
                    "tax_code_id",
                    "Default tax",
                    v.tax_code_id.map(|id| id.to_string()).unwrap_or_default(),
                ),
                display: Html::text(
                    taxes
                        .iter()
                        .find(|t| t["id"].as_i64() == v.tax_code_id)
                        .and_then(|t| t["name"].as_str())
                        .unwrap_or("No default"),
                ),
                editor: render(
                    Combobox::new(format!("vendor-{}-tax_code_id-input", v.id), "tax_code_id")
                        .aria_label("Default tax")
                        .value(v.tax_code_id.map(|id| id.to_string()).unwrap_or_default())
                        .options(options),
                )?,
            })?,
        );
        row = row.slot(
            "active",
            render(InlineChoice {
                field: self.inline(v, "active", "Status", v.active.to_string()),
                display: bilibi_ui::active_badge(v.active),
                editor: render(
                    Field::named("active")
                        .id(format!("vendor-{}-active-input", v.id))
                        .label("Status")
                        .kind(InputKind::Select)
                        .value(v.active.to_string())
                        .options(vec![
                            Choice::new("Active", "true"),
                            Choice::new("Inactive", "false"),
                        ]),
                )?,
            })?,
        );
        let relation =
            bilibi_finance_ledger::party_relation(&self.context(), s, &v.party.kind, v.party.id)
                .await?;
        let relation = match relation {
            // Recorded is the normal case and says nothing; only a missing
            // relationship needs the person.
            bilibi_finance_ledger::PartyRelation::Recorded { .. } => Html::EMPTY,
            bilibi_finance_ledger::PartyRelation::Hidden => {
                Html::text("No access to company relationships")
            }
            bilibi_finance_ledger::PartyRelation::Missing if self.can_manage => Self::button(
                &format!("vendor-{}-relationship", v.id),
                "record_vendor_relationship",
                "Record as supplier",
                "vendor",
                &v.id.to_string(),
            )?,
            _ => Html::text("Not recorded"),
        };
        row = row.slot("relationship", relation);
        Ok(row)
    }
}
