//! One account: read first, edit in place. Its name and group change on the
//! spot; deactivating it and marking it as the retained-earnings account ask
//! first, with the ledger's own review of what will change.
use super::{
    account_name, accounts::INDEX, event_type, no_company, render, shell, unavailable, view,
    Failure, Flash, Pages, Read, Review,
};
use crate::MANAGE_ACCOUNTS;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    Alert, BackLink, Badge, BadgeKind, Button, Card, Choice, Combobox, CommitStates, CommitStatus,
    ConfirmDialog, EmptyState, Fact, Facts, Header, Html, InlineChoice, InlineEdit, InlineField,
    PageVariant, SectionHeading, StatusKind, Toggle,
};
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    Deactivate,
    Designate,
}

pub(super) struct Account {
    pages: Pages,
    id: i64,
    company: Option<i64>,
    flash: Flash,
    statuses: CommitStates,
    asking: Option<(Ask, Review)>,
}

impl Account {
    pub(super) fn new(pages: Pages, id: i64, company: Option<i64>) -> Self {
        Self {
            pages,
            id,
            company,
            flash: Flash::default(),
            statuses: CommitStates::default(),
            asking: None,
        }
    }

    fn inline(&self, key: &str, label: &str, value: &str, editable: bool) -> InlineField {
        let mut field = InlineField::new(format!("ledger-account-{key}"), key, label, value)
            .version("1")
            .status(self.statuses.get(key));
        field.id_value = self.id.to_string();
        // "No group" is a real choice.
        if key == "parent" {
            field = field.allow_empty();
        }
        if !editable {
            field = field.readonly();
        }
        field
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        if self.company.is_none() {
            return self.page("Account", no_company("ledger-account-no-company")?);
        }
        let company = self.company.ok_or(Error::Invalid)?;
        let chart = match self.pages.chart_in(scope, company).await? {
            Ok(chart) => chart,
            Err(refused) => {
                return self.page(
                    "Account",
                    unavailable("ledger-account-refused", "this account", &refused)?,
                );
            }
        };
        if let Some(group) = chart.iter().find(|a| a.id == self.id && a.is_group) {
            return self.page(
                "Account",
                render(
                    EmptyState::missing(
                        "This is an account group",
                        "A group takes no postings. Its page shows what it holds and its total.",
                    )
                    .id("ledger-account-is-group")
                    .action(render(Button {
                        id: "ledger-account-group-link",
                        href: &format!("{}/{}?company={company}", super::groups::INDEX, group.id),
                        body: Html::text("Open the group"),
                        ..Button::default()
                    })?),
                )?,
            );
        }
        let Some(account) = chart.iter().find(|a| a.id == self.id).cloned() else {
            return self.page(
                "Account",
                render(
                    EmptyState::missing(
                        "This account does not exist",
                        "It may belong to another company. Go back to the chart of accounts.",
                    )
                    .id("ledger-account-missing"),
                )?,
            );
        };
        let company_name = self.pages.company_name_in(scope, company).await?;
        let can_manage = self.pages.allowed(scope, MANAGE_ACCOUNTS).await?;
        let units = self
            .pages
            .book_in(scope, company)
            .await?
            .ok()
            .and_then(|b| b.book)
            .map(|b| b.minor_units);

        // The balance today, from the trial balance contract.
        let balance = match units {
            Some(units) => {
                let today = self.pages.today(scope).await?;
                match self
                    .pages
                    .read_in(
                        scope,
                        company,
                        "finance.ledger.trial_balance.get",
                        json!({"as_of": today.to_string()}),
                    )
                    .await?
                {
                    Read::Found(value) => view::parse::<view::TrialBalance>(value).map(|tb| {
                        tb.rows
                            .iter()
                            .find(|r| r.id == account.id)
                            .map_or("0".to_owned(), |r| {
                                let net = view::decimal(&r.debit_balance)
                                    - view::decimal(&r.credit_balance);
                                let side = if net.is_zero() {
                                    ""
                                } else if net.is_sign_positive() {
                                    " debit"
                                } else {
                                    " credit"
                                };
                                format!("{}{side}", view::amount(&net.abs().to_string(), units))
                            })
                    }),
                    _ => None,
                }
            }
            None => None,
        };

        let editable = can_manage;
        // The ledger refuses to switch off an account that still has a
        // balance (a debit or credit side), so the toggle says so up front.
        let has_balance = balance
            .as_deref()
            .is_some_and(|b| b.ends_with(" debit") || b.ends_with(" credit"));
        // An account sits in an account group of its own type.
        let groups = view::by_name(
            chart
                .iter()
                .filter(|a| a.is_group && a.account_type == account.account_type),
        );
        let parent_display = match account
            .parent_id
            .and_then(|id| chart.iter().find(|a| a.id == id))
        {
            Some(parent) => account_name(parent)?,
            None => Html::text("None"),
        };
        let mut options = vec![Choice::new("No group", "")];
        options.extend(groups.iter().map(|a| view::account_choice(a)));
        let editor = render(
            Combobox::new("ledger-account-parent-input", "parent")
                .aria_label("Group")
                .placeholder("No group")
                .wrapper_class("mb-0")
                .value(account.parent_id.map(|p| p.to_string()).unwrap_or_default())
                .options(options),
        )?;
        let items = vec![
            Fact {
                title: "Code",
                id: "ledger-account-code",
                value: Html::text(&account.code),
            },
            Fact {
                title: "Name",
                id: "ledger-account-name",
                value: render(InlineEdit {
                    field: self.inline("name", "Name", &account.name, editable),
                })?,
            },
            Fact {
                title: "Type",
                id: "ledger-account-type",
                value: Html::text(view::type_label(&account.account_type)),
            },
            Fact {
                title: "Group",
                id: "ledger-account-group",
                value: render(InlineChoice {
                    field: self.inline(
                        "parent",
                        "Group",
                        &account.parent_id.map(|p| p.to_string()).unwrap_or_default(),
                        editable,
                    ),
                    display: parent_display,
                    editor,
                })?,
            },
            Fact {
                title: "Active",
                id: "ledger-account-status",
                // Switching it off asks first, because nothing can be posted to
                // an inactive account; switching it on does not.
                value: if can_manage {
                    render(Toggle {
                        on: account.active,
                        label: "Active",
                        id: "ledger-account-active",
                        event: if account.active {
                            "ask_deactivate"
                        } else {
                            "activate"
                        },
                        disabled: account.active && has_balance,
                        ..Toggle::default()
                    })?
                } else {
                    bilibi_ui::active_badge(account.active)
                },
            },
            Fact {
                title: "Retained earnings",
                id: "ledger-account-retained",
                value: if account.retained_earnings {
                    render(Badge::status(BadgeKind::Success, "Yes"))?
                } else {
                    Html::text("No")
                },
            },
            Fact {
                title: "Balance today",
                id: "ledger-account-balance",
                value: Html::text(&match balance.as_deref() {
                    Some(b) if can_manage && account.active && has_balance => {
                        format!("{b}. It can be switched off once this is zero.")
                    }
                    Some(b) => b.to_owned(),
                    None => "—".to_owned(),
                }),
            },
        ];
        let mut card = render(SectionHeading {
            id: "ledger-account-heading",
            title: "Account",
            ..SectionHeading::default()
        })?;
        card.push(&render(Facts {
            id: "ledger-account-facts",
            items,
        })?);
        let mut content = render(Card {
            id: "ledger-account-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-account-heading",
            body: card,
            ..Card::default()
        })?;
        // The entries list follows the session's current company. Never send
        // a cross-company account there with only its account filter.
        if Some(company) == Pages::company(scope) {
            content.push(&render(Button {
                id: "ledger-account-entries",
                href: &format!("/finance/ledger/journals?account={}", account.id),
                class: "mt-4",
                body: Html::text("See this account's entries"),
                ..Button::default()
            })?);
        } else {
            content.push(&render(Alert {
                id: "ledger-account-entries-company",
                kind: StatusKind::Info,
                body: Html::text(&format!(
                    "To see this account's entries, choose {company_name} in the top bar."
                )),
                ..Alert::default()
            })?);
        }

        let mut actions = Html::EMPTY;
        if can_manage
            && account.account_type == "equity"
            && account.active
            && !account.retained_earnings
        {
            actions.push(&render(Button {
                id: "ledger-account-designate",
                button_type: "button",
                event: "ask_designate",
                body: Html::text("Use for retained earnings"),
                ..Button::default()
            })?);
        }
        actions.push(&render(BackLink {
            href: &format!("{INDEX}?company={company}"),
            id: "ledger-account-back",
            title: "Back to the chart of accounts",
            ..BackLink::default()
        })?);
        let mut page = render(Header {
            title: account_name(&account)?,
            subtitle: &company_name,
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        page.push(&render(super::Announce {
            kind: crate::MODULE,
            record_id: &account.id.to_string(),
        })?);
        page.push(&content);
        if let Some((ask, review)) = &self.asking {
            let (consequence, detail, confirm, working, event) = match ask {
                Ask::Deactivate => (
                    format!("Deactivate {} ({})?", account.name, account.code),
                    "No one can post to it until you activate it again. Its history stays. An account with a balance cannot be deactivated.".to_owned(),
                    "Deactivate",
                    "Deactivating…",
                    "deactivate",
                ),
                Ask::Designate => (
                    format!(
                        "Use {} ({}) for retained earnings?",
                        account.name, account.code
                    ),
                    designate_detail(&review.facts),
                    "Use for retained earnings",
                    "Saving…",
                    "designate",
                ),
            };
            page.push(&render(ConfirmDialog {
                id: "ledger-account-confirm",
                consequence: &consequence,
                detail: &format!("{company_name}. {detail}"),
                confirm,
                working,
                confirm_event: event,
                cancel_event: "cancel_ask",
            })?);
        }
        Ok(LiveBody {
            html: shell(
                "ledger-account-page",
                PageVariant::Detail,
                &self.flash,
                page,
            )?,
            query: self
                .company
                .map(|id| std::collections::HashMap::from([("company".into(), id.to_string())]))
                .unwrap_or_default(),
            ..LiveBody::default()
        })
    }

    fn page(&self, title: &str, body: Html) -> Result<LiveBody> {
        let mut page = render(Header {
            title: Html::text(title),
            ..Header::default()
        })?;
        page.push(&body);
        Ok(LiveBody {
            html: shell(
                "ledger-account-page",
                PageVariant::Detail,
                &self.flash,
                page,
            )?,
            query: self
                .company
                .map(|id| std::collections::HashMap::from([("company".into(), id.to_string())]))
                .unwrap_or_default(),
            ..LiveBody::default()
        })
    }

    async fn save_field(&mut self, r: &LiveRequest<'_, Scope>, value: &Value) -> Result<()> {
        let field = value["field"].as_str().ok_or(Error::Invalid)?;
        let submitted = value["value"].as_str().ok_or(Error::Invalid)?;
        if !["name", "parent"].contains(&field)
            || value["id"].as_str() != Some(&self.id.to_string())
        {
            return Err(Error::Invalid);
        }
        let mut input = json!({"account_id": self.id});
        match field {
            "name" => input["name"] = json!(submitted.trim()),
            _ => match submitted.parse::<i64>() {
                Ok(parent) => input["parent_id"] = json!(parent),
                Err(_) => input["clear_parent"] = json!(true),
            },
        }
        let label = if field == "name" { "Name" } else { "Group" };
        let status = match self
            .pages
            .submit_in(
                r.scope,
                self.company.ok_or(Error::Invalid)?,
                r.credential,
                "save_field",
                "finance.ledger.account.update",
                input,
            )
            .await?
        {
            Ok(_) => CommitStatus::Saved,
            Err(Failure::Refused(refusal)) => {
                let mut reasons = refusal.field(if field == "name" { "name" } else { "parent_id" });
                reasons.extend(refusal.alert(&["name", "parent_id"]));
                let reasons: Vec<&str> = reasons.iter().map(String::as_str).collect();
                CommitStatus::refusal(label, submitted, &reasons)
            }
            Err(Failure::Denied) => CommitStatus::Error(super::DENIED.into()),
            Err(Failure::Changed) => CommitStatus::Error(
                "The account changed while you were editing it. Reload the page and try again."
                    .into(),
            ),
            Err(Failure::Failed) => CommitStatus::failure(),
        };
        self.statuses.put(field, status);
        Ok(())
    }

    async fn ask(&mut self, r: &LiveRequest<'_, Scope>, ask: Ask) -> Result<()> {
        let input = match ask {
            Ask::Deactivate => json!({"account_id": self.id, "active": false}),
            Ask::Designate => json!({"account_id": self.id, "retained_earnings": true}),
        };
        let event = match ask {
            Ask::Deactivate => "ask_deactivate",
            Ask::Designate => "ask_designate",
        };
        match self
            .pages
            .review_in(
                r.scope,
                self.company.ok_or(Error::Invalid)?,
                r.credential,
                event,
                "finance.ledger.account.update",
                input,
            )
            .await?
        {
            Ok(review) => self.asking = Some((ask, review)),
            Err(failure) => self.flash.error(failure.text("The account")),
        }
        Ok(())
    }

    async fn confirm(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some((ask, review)) = self.asking.take() else {
            return Ok(());
        };
        let event = match ask {
            Ask::Deactivate => "deactivate",
            Ask::Designate => "designate",
        };
        match self
            .pages
            .commit(
                r.scope,
                r.credential,
                event,
                "finance.ledger.account.update",
                review,
            )
            .await?
        {
            Ok(_) => self.flash.success(match ask {
                Ask::Deactivate => "The account is inactive.",
                Ask::Designate => "The account now holds retained earnings.",
            }),
            Err(failure) => self.flash.error(failure.text("The account")),
        }
        Ok(())
    }

    async fn activate(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        match self
            .pages
            .submit_in(
                r.scope,
                self.company.ok_or(Error::Invalid)?,
                r.credential,
                "activate",
                "finance.ledger.account.update",
                json!({"account_id": self.id, "active": true}),
            )
            .await?
        {
            Ok(_) => self.flash.success("The account is active."),
            Err(failure) => self.flash.error(failure.text("The account")),
        }
        Ok(())
    }
}

fn designate_detail(changes: &Value) -> String {
    let released = &changes["released_retained_earnings"];
    match (released["code"].as_str(), released["name"].as_str()) {
        (Some(code), Some(name)) => format!(
            "The year-end close will post profit or loss to this account. {name} ({code}) stops being the retained earnings account."
        ),
        _ => "The year-end close will post profit or loss to this account.".to_owned(),
    }
}

impl LiveState<Scope, Error> for Account {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Params(query) = r.input {
                let company = super::target_company(query, Some(r.scope))?;
                if company != self.company {
                    self.company = company;
                    self.statuses = CommitStates::default();
                    self.asking = None;
                }
            }
            if let LiveInput::Event(value) = r.input {
                match event_type(value)? {
                    "save_field" => self.save_field(&r, value).await?,
                    "ask_deactivate" => self.ask(&r, Ask::Deactivate).await?,
                    "ask_designate" => self.ask(&r, Ask::Designate).await?,
                    "deactivate" | "designate" => self.confirm(&r).await?,
                    "cancel_ask" => self.asking = None,
                    "activate" => self.activate(&r).await?,
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}
