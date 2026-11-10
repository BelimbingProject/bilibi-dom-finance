//! Payment requests, newest first: the supplier payments waiting for a second
//! person, the posted ones and the rejected ones. Bilimbi's payables had no
//! page for them; the requests are read through the facade and a new one is
//! made on the request form.
use super::{company, format as f, render, Pages, NEW_PAYMENT, PAYMENTS};
use crate::{Payment, PaymentRequest, PaymentRequestPageQuery, SUBMIT_PAYMENTS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, MinorUnits};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BadgeKind, BuiltinColumn, Cell, Choice, CreateButton, EmptyState, Filter,
    FilterToolbar, FlexRow, FlexTable, Html, ListColumns, ListSpec, ListState, PageCounts,
    Pagination, RecordLink, TableEmpty, ToolbarControl,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const SIZES: [u64; 3] = [25, 50, 100];

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: SIZES.to_vec(),
        filters: [(
            "status".into(),
            Filter::one_of(&["", "submitted", "posted", "rejected"], ""),
        )]
        .into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("payment requests list spec")
});

pub(super) struct Payments {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    requests: Vec<PaymentRequest>,
    payments: HashMap<i64, Payment>,
    accounts: Vec<Account>,
    units: Option<MinorUnits>,
    total: u64,
    can_submit: bool,
    unavailable: bool,
    framed: bool,
}

impl Payments {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = [
            ("number", "Request"),
            ("payment_date", "Date"),
            ("party", "Supplier"),
            ("paid_from", "Paid from"),
            ("amount", "Amount"),
            ("status", "Status"),
            ("payment", "Payment"),
        ]
        .into_iter()
        .map(|(id, label)| {
            let c = BuiltinColumn::new(id, label);
            if id == "amount" {
                c.right()
            } else {
                c
            }
        })
        .collect();
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("payables-payments-table", columns)
                .map_err(|_| Error::Internal)?,
            requests: vec![],
            payments: HashMap::new(),
            accounts: vec![],
            units: None,
            total: 0,
            can_submit: false,
            unavailable: false,
            framed,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.can_submit = self.pages.allowed(scope, SUBMIT_PAYMENTS).await?;
        self.requests.clear();
        self.payments.clear();
        self.total = 0;
        let Some(company) = company(scope) else {
            self.unavailable = true;
            return Ok(());
        };
        let status = params::blank_to_none(self.list.filters.get("status").map(String::as_str));
        self.unavailable = false;
        let page = self
            .pages
            .payables
            .page_payment_requests(
                scope,
                company,
                &PaymentRequestPageQuery {
                    status,
                    search: self.list.search.trim().to_owned(),
                    page: self.list.page.max(1) as i64,
                    page_size: self.list.page_size as i64,
                },
            )
            .await;
        let Ok(page) = page else {
            self.unavailable = true;
            return Ok(());
        };
        self.total = page.total.max(0) as u64;
        self.list.page = page.page.max(1) as u64;
        self.requests = page.requests;
        self.units = self
            .pages
            .ledger
            .get_book(scope, company)
            .await
            .ok()
            .map(|b| b.minor_units);
        self.accounts = self
            .pages
            .ledger
            .list_accounts(scope, company)
            .await
            .unwrap_or_default();
        let paid: Vec<i64> = self
            .requests
            .iter()
            .filter_map(|r| r.decision.as_ref()?.payment_id)
            .collect();
        for id in paid {
            if let Ok(payment) = self.pages.payables.get_payment(scope, company, id).await {
                self.payments.insert(payment.id, payment);
            }
        }
        self.columns.load(&self.requests, |r| r.id.to_string());
        Ok(())
    }

    pub(super) fn body(&self) -> Result<String> {
        let actions = if self.can_submit {
            render(CreateButton {
                noun: "Payment",
                href: NEW_PAYMENT,
                id: "payables-payments-new",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let mut content = Html::EMPTY;
        let mut toolbar = FilterToolbar::new(
            "payables-payments-filters",
            vec![
                ToolbarControl::search(
                    "payables-payments-search",
                    "search",
                    "Search payments",
                    &self.list.search,
                    "Search by supplier, reference or number…",
                ),
                ToolbarControl::select(
                    "payables-payments-status",
                    "status",
                    "Filter by status",
                    &self.list.filters["status"],
                    [
                        ("All statuses", ""),
                        ("Waiting for approval", "submitted"),
                        ("Paid", "posted"),
                        ("Rejected", "rejected"),
                    ]
                    .map(|(l, v)| Choice::new(l, v))
                    .to_vec(),
                ),
            ],
        );
        toolbar.event = "filter";
        content.push(&render(toolbar)?);
        if self.unavailable {
            let mut empty = EmptyState::missing(
                "Payments unavailable",
                "This company's payments cannot be read with your current access.",
            );
            empty.id = "payables-payments-unavailable".into();
            content.push(&render(empty)?);
        } else {
            let mut rows = Vec::new();
            for r in &self.requests {
                let account = self
                    .accounts
                    .iter()
                    .find(|a| a.id == r.paid_from_account_id)
                    .map_or_else(
                        || "Account unavailable".into(),
                        |a| format!("{} · {}", a.name, a.code),
                    );
                let amount = r.amount.display(self.units);
                let payment = r
                    .decision
                    .as_ref()
                    .and_then(|d| d.payment_id)
                    .and_then(|id| self.payments.get(&id));
                let mut row = FlexRow::new(r.id.to_string());
                row.dom_id = Some(format!("payables-payment-request-{}", r.id));
                row = row
                    .slot(
                        "number",
                        render(RecordLink {
                            workspace: self.framed,
                            kind: "finance/payables_payments",
                            record_id: &r.id.to_string(),
                            href: &format!("{PAYMENTS}/{}", r.id),
                            class: "font-medium whitespace-nowrap text-ink-strong hover:underline",
                            body: Html::text(&format!("#{}", r.request_number)),
                        })?,
                    )
                    .cell(
                        "payment_date",
                        Cell::text(bilibi_finance_ledger::dates::display(r.payment_date)),
                    )
                    .cell("party", Cell::text(&r.party_name))
                    .slot(
                        "paid_from",
                        bilibi_finance_ledger::AccountName::label(&account)?,
                    )
                    .slot(
                        "amount",
                        render(Tabular {
                            text: format!("{amount} {}", r.currency_code),
                        })?,
                    )
                    .slot("status", f::request_status(&r.status)?);
                row = row.slot(
                    "payment",
                    match payment {
                        Some(p) if p.reversed_by_payment_id.is_some() => f::badge(
                            &format!("#{} · reversed", p.payment_number),
                            BadgeKind::Warning,
                            false,
                        )?,
                        Some(p) => Html::text(&format!("#{}", p.payment_number)),
                        None => Html::text("—"),
                    },
                );
                rows.push(row);
            }
            let mut table =
                FlexTable::new("payables-payments-table", self.columns.flex_columns(), rows);
            table.event = "payments_grid";
            table.caption = "Payments";
            table.framed = false;
            table.zoom = self.columns.zoom;
            table.suggestions = self.columns.flex_suggestions();
            table.add_query = &self.columns.add_query;
            let filtered = !self.list.search.is_empty() || !self.list.filters["status"].is_empty();
            let mut empty = if filtered {
                EmptyState::missing(
                    "No payments match",
                    "Change the filters to see more payments.",
                )
            } else {
                EmptyState::missing("No payments yet", "Pay a supplier's posted bills.")
            };
            if !filtered && self.can_submit {
                empty.action = render(CreateButton {
                    noun: "Payment",
                    href: NEW_PAYMENT,
                    id: "payables-payments-empty-new",
                    ..CreateButton::default()
                })?;
            }
            table.empty = Some(TableEmpty::Content(render(empty)?));
            content.push(&render(table)?);
            let mut pager = Pagination::new(
                "payables-payments-pagination",
                PageCounts::new(self.list.page, self.list.page_size, self.total),
                &SIZES,
            );
            pager.filters_event = "filter";
            content.push(&render(pager)?);
        }
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
            content,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::page(id="payables-payments") %}{% call ui::header(subtitle="Supplier payments waiting for approval, paid or rejected.", title_actions=title_actions, actions=actions) %}Payments{% endcall %}{{ content|safe }}{% endcall %}"#,
    ext = "html"
)]
struct View {
    title_actions: Html,
    actions: Html,
    content: Html,
}

#[derive(Template)]
#[template(
    source = r#"<span class="tabular-nums">{{ text }}</span>"#,
    ext = "html"
)]
struct Tabular {
    text: String,
}

impl LiveState<Scope, Error> for Payments {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Refresh => true,
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    true
                }
                LiveInput::Event(v) => match v["type"].as_str() {
                    Some("filter") => {
                        self.list.apply_filters(&params::form_values(v, "filters"));
                        true
                    }
                    Some("page") => {
                        self.list.put_page(v["page"].as_str());
                        true
                    }
                    Some("payments_grid") => {
                        self.columns.handle(&params::string_values(v));
                        false
                    }
                    _ => return Err(Error::Invalid),
                },
            };
            if reload {
                self.reload(r.scope).await?;
            }
            Ok(LiveBody {
                html: self.body()?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
