//! One payment request: what was asked, who decided, the payment it became and
//! its reversal. The decisions are the contracts the agent doors call:
//! someone other than the submitter approves (or rejects with a reason), and
//! a posted payment is reversed by date.
use super::{company, format as f, render, Pages, Written};
use crate::{Document, Payment, PaymentRequest, PAYMENT_SOURCE, POST_PAYMENTS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, MinorUnits};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PanelSpec},
    params, BuiltinColumn, ButtonVariant, Cell, EmptyState, Facts, Field, FlashMap, FlexRow,
    FlexTable, Html, InputKind, ListColumns, StatusKind, NO_FLASH,
};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq)]
enum Dialog {
    Reject,
    Reverse,
}

pub(super) struct Request {
    pages: Pages,
    id: i64,
    request: Option<PaymentRequest>,
    payment: Option<Payment>,
    reversal: Option<Payment>,
    bills: HashMap<i64, Document>,
    accounts: Vec<Account>,
    units: Option<MinorUnits>,
    can_decide: bool,
    dialog: Option<Dialog>,
    reason: String,
    date: String,
    description: String,
    flash: Option<(StatusKind, String)>,
    allocations: ListColumns,
}

impl Request {
    pub(super) fn new(pages: Pages, path: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            request: None,
            payment: None,
            reversal: None,
            bills: HashMap::new(),
            accounts: vec![],
            units: None,
            can_decide: false,
            dialog: None,
            reason: String::new(),
            date: String::new(),
            description: String::new(),
            flash: None,
            allocations: ListColumns::mount(
                "payables-payment-allocations-table",
                [
                    ("number", "#"),
                    ("bill", "Bill"),
                    ("party_number", "Supplier's number"),
                    ("amount", "Paid"),
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
                .collect(),
            )
            .map_err(|_| Error::Internal)?,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.can_decide = self.pages.allowed(scope, POST_PAYMENTS).await?;
        self.request = None;
        self.payment = None;
        self.reversal = None;
        self.bills.clear();
        let Some(company) = company(scope) else {
            return Ok(());
        };
        self.request = self
            .pages
            .payables
            .get_payment_request(scope, company, self.id)
            .await
            .ok();
        let Some(request) = &self.request else {
            return Ok(());
        };
        if let Some(id) = request.decision.as_ref().and_then(|d| d.payment_id) {
            self.payment = self
                .pages
                .payables
                .get_payment(scope, company, id)
                .await
                .ok();
        }
        if let Some(id) = self.payment.as_ref().and_then(|p| p.reversed_by_payment_id) {
            self.reversal = self
                .pages
                .payables
                .get_payment(scope, company, id)
                .await
                .ok();
        }
        for a in &request.allocations {
            if let Ok(d) = self
                .pages
                .payables
                .get_document(scope, company, a.document_id)
                .await
            {
                self.bills.insert(d.id, d);
            }
        }
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
        self.allocations
            .load(&request.allocations, |a| a.line_number.to_string());
        Ok(())
    }

    pub(super) fn title(&self) -> Option<String> {
        self.request
            .as_ref()
            .map(|r| format!("Payment #{} to {}", r.request_number, r.party_name))
    }

    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Info => flash.info = message,
            }
        }
        let Some(r) = &self.request else {
            let mut missing = EmptyState::missing(
                "Payment not found",
                "There is no such payment for this company, or it cannot be read with your current access.",
            );
            missing.id = "payables-payment-missing".into();
            missing.action = render(AllPayments)?;
            return Missing {
                flash,
                missing: render(missing)?,
            }
            .render()
            .map_err(|_| Error::Internal);
        };
        let display = self.pages.display(scope).await?;
        let account = self
            .accounts
            .iter()
            .find(|a| a.id == r.paid_from_account_id)
            .map_or_else(
                || "Account unavailable".into(),
                |a| format!("{} {}", a.code, a.name),
            );
        let amount = format!("{} {}", r.amount.display(self.units), r.currency_code);
        let mut items = vec![
            f::fact("Supplier", &format!("{} · {}", r.party_code, r.party_name)),
            f::fact("Payment date", &r.payment_date.to_string()),
            f::fact("Paid from", &account),
            f::fact("Amount", &amount),
            f::fact("Reference", f::optional(&r.reference)),
            f::fact("Description", f::optional(&r.description)),
        ];
        items.push(bilibi_ui::Fact {
            title: "Requested",
            value: f::actor_at(
                &display,
                Some(r.submitted_by_user_id),
                r.submitted_by_impersonator_id,
                Some(&r.submitted_at),
                "payables-payment-submitted-at",
                " · ",
            )?,
            ..bilibi_ui::Fact::default()
        });
        if let Some(d) = &r.decision {
            let mut value = f::actor_at(
                &display,
                Some(d.actor_user_id),
                d.actor_impersonator_id,
                Some(&d.occurred_at),
                "payables-payment-decided-at",
                " · ",
            )?;
            if let Some(reason) = &d.reason {
                value.push(&Html::text(&format!(" · {reason}")));
            }
            items.push(bilibi_ui::Fact {
                title: if d.event == "rejected" {
                    "Rejected"
                } else {
                    "Approved"
                },
                value,
                ..bilibi_ui::Fact::default()
            });
        }
        let facts = render(Facts {
            id: "payables-payment-facts",
            items,
        })?;
        let payment = match &self.payment {
            Some(p) => {
                let mut items = vec![
                    f::fact("Payment", &format!("#{}", p.payment_number)),
                    f::fact("Ledger entry", &format!("#{}", p.entry_number)),
                ];
                if let Some(re) = &self.reversal {
                    items.push(bilibi_ui::Fact {
                        title: "Reversed",
                        value: f::actor_at(
                            &display,
                            Some(re.posted_by_user_id),
                            re.posted_by_impersonator_id,
                            Some(&re.posted_at),
                            "payables-payment-reversed-at",
                            " · ",
                        )?,
                        ..bilibi_ui::Fact::default()
                    });
                    items.push(f::fact("Reversal date", &re.payment_date.to_string()));
                    items.push(f::fact("Reversal entry", &format!("#{}", re.entry_number)));
                }
                Some(render(Facts {
                    id: "payables-payment-posted-facts",
                    items,
                })?)
            }
            None => None,
        };
        let mut rows = Vec::new();
        for a in &r.allocations {
            let bill = self.bills.get(&a.document_id);
            let mut row = FlexRow::new(a.line_number.to_string());
            row.dom_id = Some(format!("payables-payment-allocation-{}", a.line_number));
            rows.push(
                row.cell("number", Cell::text(a.line_number.to_string()))
                    .slot(
                        "bill",
                        render(BillLink {
                            id: a.document_id,
                            text: bill.map_or_else(
                                || format!("Bill record {}", a.document_id),
                                |d| {
                                    format!(
                                        "{} {}",
                                        f::kind(&d.document_type),
                                        f::number(d.document_number)
                                    )
                                },
                            ),
                        })?,
                    )
                    .cell(
                        "party_number",
                        Cell::text(bill.map_or("—", |d| f::optional(&d.party_document_number))),
                    )
                    .cell("amount", Cell::text(a.amount.display(self.units))),
            );
        }
        let mut table = FlexTable::new(&self.allocations.id, self.allocations.flex_columns(), rows);
        table.event = "allocations_grid";
        table.caption = "Bills paid";
        table.framed = false;
        table.zoom = self.allocations.zoom;
        table.suggestions = self.allocations.flex_suggestions();
        table.add_query = &self.allocations.add_query;
        let allocations = render(table)?;
        let dialog = match self.dialog {
            Some(kind) => Some(render(self.dialog_view(kind, r))?),
            None => None,
        };
        let waiting = r.status == "submitted";
        let can_reverse = self
            .payment
            .as_ref()
            .is_some_and(|p| p.reversed_by_payment_id.is_none() && p.reverses_payment_id.is_none());
        let title = format!("Payment #{}", r.request_number);
        View {
            flash,
            title_actions: bilibi_ui::Html::EMPTY,
            title,
            status: f::request_status(&r.status)?,
            actions: render(Actions {
                approve: self.can_decide && waiting,
                reject: self.can_decide && waiting,
                reverse: self.can_decide && can_reverse,
            })?,
            facts,
            payment,
            allocations,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn dialog_view(&self, kind: Dialog, r: &PaymentRequest) -> DialogView {
        let reverse = kind == Dialog::Reverse;
        let base = if reverse {
            "payables-payment-reverse"
        } else {
            "payables-payment-reject"
        };
        let mut fields = Vec::new();
        if reverse {
            fields.push(
                Field::in_form("dialog", "date")
                    .id(format!("{base}-date"))
                    .kind(InputKind::Date)
                    .label("Reversal date")
                    .value(self.date.clone())
                    .hint(format!("Not before {}.", r.payment_date))
                    .required(),
            );
            fields.push(
                Field::in_form("dialog", "description")
                    .id(format!("{base}-description"))
                    .label("Description (optional)")
                    .value(self.description.clone())
                    .attr("maxlength", "500"),
            );
        } else {
            fields.push(
                Field::in_form("dialog", "reason")
                    .id(format!("{base}-reason"))
                    .kind(InputKind::Textarea)
                    .label("Reason (1 to 500 characters)")
                    .value(self.reason.clone())
                    .attr("maxlength", "500")
                    .required(),
            );
        }
        DialogView {
            modal_id: format!("{base}-modal"),
            form_id: format!("{base}-form"),
            close_id: format!("{base}-close"),
            confirm_id: format!("{base}-confirm"),
            title: if reverse {
                "Reverse this payment"
            } else {
                "Reject this request"
            },
            description: if reverse {
                "Posts the exact opposite entry once, and the bills are owed again from the reversal date. It cannot be undone."
            } else {
                "The request stays on record as rejected and nothing is paid; it cannot be undone."
            },
            event: if reverse {
                "reverse_payment"
            } else {
                "reject_payment"
            },
            cancel: if reverse {
                "Keep payment"
            } else {
                "Keep request"
            },
            confirm: if reverse {
                "Reverse payment"
            } else {
                "Reject request"
            },
            working: if reverse {
                "Reversing…"
            } else {
                "Rejecting…"
            },
            variant: ButtonVariant::Danger,
            fields,
        }
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<bool> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        match event {
            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| v["key"].as_str() == Some(kind.name()))
                {
                    self.flash = None;
                }
                return Ok(false);
            }
            "allocations_grid" => {
                self.allocations.handle(&params::string_values(v));
                return Ok(false);
            }
            "close_dialog" => {
                self.dialog = None;
                return Ok(false);
            }
            "dialog_edit" => {
                let values = params::form_values(v, "dialog");
                if let Some(text) = values.get("reason") {
                    self.reason = text.clone();
                }
                if let Some(text) = values.get("date") {
                    self.date = text.clone();
                }
                if let Some(text) = values.get("description") {
                    self.description = text.clone();
                }
                return Ok(false);
            }
            "open_dialog" => {
                if !self.can_decide {
                    return Ok(false);
                }
                self.flash = None;
                self.reason.clear();
                self.description.clear();
                self.date = self.pages.today(scope).await?.to_string();
                self.dialog = match v["dialog"].as_str() {
                    Some("reject") => Some(Dialog::Reject),
                    Some("reverse") => Some(Dialog::Reverse),
                    _ => None,
                };
                return Ok(false);
            }
            "approve_payment" | "reject_payment" | "reverse_payment" => {}
            _ => return Err(Error::Invalid),
        }
        let Some(company) = company(scope) else {
            return Ok(false);
        };
        let (contract, input, doing) = match event {
            "approve_payment" => (
                "finance.payables.payment.approve",
                json!({"company_id": company, "request_id": self.id}),
                "The approval",
            ),
            "reject_payment" => {
                let values = params::form_values(v, "dialog");
                self.reason = values.get("reason").cloned().unwrap_or_default();
                (
                    "finance.payables.payment.reject",
                    json!({"company_id": company, "request_id": self.id, "reason": self.reason.trim()}),
                    "The rejection",
                )
            }
            _ => {
                let values = params::form_values(v, "dialog");
                self.date = values.get("date").cloned().unwrap_or_default();
                self.description = values.get("description").cloned().unwrap_or_default();
                let Some(payment) = self.payment.as_ref().map(|p| p.id) else {
                    return Ok(false);
                };
                let mut input = json!({"company_id": company, "payment_id": payment, "payment_date": self.date.trim()});
                if !self.description.trim().is_empty() {
                    input["description"] = json!(self.description.trim());
                }
                ("finance.payables.payment.reverse", input, "The reversal")
            }
        };
        let written = self
            .pages
            .write(scope, credential, event, contract, input)
            .await?;
        self.flash = Some(match written {
            Written::Done(_) => (
                StatusKind::Success,
                match event {
                    "approve_payment" => "Payment approved and posted.".to_owned(),
                    "reject_payment" => "Payment rejected.".to_owned(),
                    _ => "Payment reversed.".to_owned(),
                },
            ),
            Written::Forbidden => (
                StatusKind::Error,
                match event {
                    "approve_payment" | "reject_payment" => {
                        "You do not have permission to decide payments.".to_owned()
                    }
                    _ => "You do not have permission to reverse payments.".to_owned(),
                },
            ),
            Written::Changed => (
                StatusKind::Error,
                format!(
                    "{doing} did not go through because the request changed. Review it and try again."
                ),
            ),
            Written::Fields(fields) => (StatusKind::Error, f::refused(doing, &fields)),
        });
        self.dialog = None;
        self.reason.clear();
        Ok(true)
    }
}

impl LiveState<Scope, Error> for Request {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Refresh | LiveInput::Params(_) => true,
                LiveInput::Event(v) => self.event(r.scope, r.credential, v).await?,
            };
            if reload {
                self.reload(r.scope).await?;
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                ..LiveBody::default()
            })
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        if self.request.is_none() {
            return vec![];
        }
        vec![PanelSpec {
            id: "payables-payment-attachments".into(),
            embed: bilibi_finance_attachments::PANEL_EMBED.into(),
            path: [
                ("panel".into(), "payables-payment-attachments".into()),
                ("source".into(), PAYMENT_SOURCE.into()),
                ("document".into(), self.id.to_string()),
            ]
            .into(),
            query: HashMap::new(),
        }]
    }
}

#[derive(Template)]
#[template(path = "payment.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    title_actions: Html,
    title: String,
    status: Html,
    actions: Html,
    facts: Html,
    payment: Option<Html>,
    allocations: Html,
    dialog: Option<Html>,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% import "feedback.html" as feedback %}{% call feedback::flash_group(messages=flash) %}{% endcall %}{% call ui::page(id="payables-payment", variant=bilibi_ui::PageVariant::Detail) %}{{ missing|safe }}{% endcall %}"#,
    ext = "html"
)]
struct Missing<'a> {
    flash: FlashMap<'a>,
    missing: Html,
}

#[derive(Template)]
#[template(
    source = r#"<a href="/finance/payables/bills/{{ id }}" class="text-action hover:underline">{{ text }}</a>"#,
    ext = "html"
)]
struct BillLink {
    id: i64,
    text: String,
}

#[derive(Template)]
#[template(
    source = r#"<a href="/finance/payables/payments">All payments</a>"#,
    ext = "html"
)]
struct AllPayments;

#[derive(Template)]
#[template(path = "payment_actions.html")]
struct Actions {
    approve: bool,
    reject: bool,
    reverse: bool,
}

#[derive(Template)]
#[template(path = "payment_dialog.html")]
struct DialogView {
    modal_id: String,
    form_id: String,
    close_id: String,
    confirm_id: String,
    title: &'static str,
    description: &'static str,
    event: &'static str,
    cancel: &'static str,
    confirm: &'static str,
    working: &'static str,
    variant: ButtonVariant,
    fields: Vec<Field>,
}
