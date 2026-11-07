//! The Payroll posting accounts page: the two accounts owed to staff chosen
//! in place (before setup the first waits for the second, so both save
//! together), the "+ Mapping" dialog, each mapping's accounts chosen in place
//! on its row, and the change history folded away.
use super::*;

impl State {
    /// One posting account chosen in place. Once set up, it is saved alone;
    /// before that it waits on the page until the other is chosen, then both
    /// are saved together, so nothing half-set is kept.
    pub(super) async fn save_account(
        &mut self,
        scope: &Scope,
        company: i64,
        value: &Value,
    ) -> Result<()> {
        let field = value["field"].as_str().ok_or(Error::Invalid)?;
        let label = SETTINGS_ACCOUNTS
            .iter()
            .find(|(key, _)| *key == field)
            .map(|(_, label)| *label)
            .ok_or(Error::Invalid)?;
        let key = inline_key(value);
        let submitted = value["value"]
            .as_str()
            .ok_or(Error::Invalid)?
            .trim()
            .to_owned();
        let Some(id) = params::positive_id(Some(&submitted)) else {
            self.statuses.put(
                key,
                CommitStatus::refusal(label, &submitted, &["choose an account"]),
            );
            return Ok(());
        };
        let payroll = self.pages.payroll.clone();
        let configured = match payroll.get_settings(scope, company).await {
            Ok(_) => true,
            Err(PayrollError::PayrollNotConfigured) => false,
            Err(e) => {
                self.statuses.put(key, CommitStatus::Error(e.to_string()));
                return Ok(());
            }
        };
        let change = if configured {
            let mut change = SettingsChange {
                net_pay_account_id: None,
                claims_payable_account_id: None,
                reason: None,
            };
            if field == "net_pay_account_id" {
                change.net_pay_account_id = Some(id);
            } else {
                change.claims_payable_account_id = Some(id);
            }
            change
        } else {
            self.settings.insert(field.to_owned(), id.to_string());
            let held = |k: &str| self.settings.get(k).and_then(|v| v.parse::<i64>().ok());
            match (
                held("net_pay_account_id"),
                held("claims_payable_account_id"),
            ) {
                (Some(net), Some(claims)) => SettingsChange {
                    net_pay_account_id: Some(net),
                    claims_payable_account_id: Some(claims),
                    reason: None,
                },
                // The first choice waits on the page for the other.
                _ => {
                    self.statuses.put(key, CommitStatus::None);
                    return Ok(());
                }
            }
        };
        match payroll.put_settings(scope, company, &change).await {
            Ok(_) => {
                self.settings.clear();
                for (other, _) in SETTINGS_ACCOUNTS {
                    self.statuses
                        .put(format!("settings-{other}"), CommitStatus::None);
                }
                self.statuses.put(key, CommitStatus::Saved);
            }
            Err(PayrollError::Invalid(errors)) => {
                for (other, other_label) in SETTINGS_ACCOUNTS {
                    let reasons: Vec<&str> = errors
                        .iter()
                        .filter(|e| e.field == other)
                        .map(|e| e.message.as_str())
                        .collect();
                    if !reasons.is_empty() {
                        let shown = self.settings.get(other).cloned().unwrap_or_default();
                        let shown = if other == field {
                            submitted.clone()
                        } else {
                            shown
                        };
                        self.statuses.put(
                            format!("settings-{other}"),
                            CommitStatus::refusal(other_label, &shown, &reasons),
                        );
                    }
                }
            }
            Err(e) => self.statuses.put(key, CommitStatus::Error(e.to_string())),
        }
        Ok(())
    }

    /// A mapping's expense or liability account chosen in place on its row;
    /// its kind, code and direction stay as they were saved.
    pub(super) async fn save_mapping_account(
        &mut self,
        scope: &Scope,
        company: i64,
        value: &Value,
    ) -> Result<()> {
        let id = params::positive_id(value["id"].as_str()).ok_or(Error::Invalid)?;
        let field = value["field"].as_str().ok_or(Error::Invalid)?;
        let label = match field {
            "expense_account_id" => "Expense account",
            "liability_account_id" => "Liability account",
            _ => return Err(Error::Invalid),
        };
        let key = inline_key(value);
        let submitted = value["value"]
            .as_str()
            .ok_or(Error::Invalid)?
            .trim()
            .to_owned();
        let Some(account) = params::positive_id(Some(&submitted)) else {
            self.statuses.put(
                key,
                CommitStatus::refusal(label, &submitted, &["choose an account"]),
            );
            return Ok(());
        };
        let payroll = self.pages.payroll.clone();
        let mapping = match payroll.list_mappings(scope, company).await {
            Ok(mappings) => mappings.into_iter().find(|m| m.id == id),
            Err(e) => {
                self.statuses.put(key, CommitStatus::Error(e.to_string()));
                return Ok(());
            }
        };
        let Some(m) = mapping else {
            self.statuses.put(
                key,
                CommitStatus::Error("This mapping is no longer here.".into()),
            );
            return Ok(());
        };
        let change = MappingChange {
            kind: m.kind,
            code: m.code,
            direction: m.direction,
            expense_account_id: if field == "expense_account_id" {
                Some(account)
            } else {
                m.expense_account_id
            },
            liability_account_id: if field == "liability_account_id" {
                Some(account)
            } else {
                m.liability_account_id
            },
            reason: None,
        };
        let status = match payroll.put_mapping(scope, company, &change).await {
            Ok(_) => CommitStatus::Saved,
            Err(PayrollError::Invalid(errors)) => {
                let reasons: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                CommitStatus::refusal(label, &submitted, &reasons)
            }
            Err(e) => CommitStatus::Error(e.to_string()),
        };
        self.statuses.put(key, status);
        Ok(())
    }

    /// One value edited in place, keyed per row so each keeps its own
    /// outcome, and read-only for someone who cannot manage the setup.
    fn inline(
        &self,
        key: &str,
        field: &str,
        label: &str,
        value: String,
        id_value: String,
        can_manage: bool,
    ) -> InlineField {
        let mut inline = InlineField::new(format!("payroll-{key}"), field, label, value)
            .version(0)
            .status(self.statuses.get(key));
        inline.id_value = id_value;
        // A settings account saves alone; a mapping's, with its row's id.
        inline.save_event = if id_value_is_row(&inline.id_value) {
            "save_mapping_account"
        } else {
            "save_account"
        }
        .into();
        if !can_manage {
            inline = inline.readonly();
        }
        inline
    }

    /// An account chosen in place with the searchable combobox, the name
    /// first and the code muted.
    fn account_choice(
        &self,
        inline: InlineField,
        accounts: &[Account],
        current: Option<i64>,
        options: Vec<Choice>,
        can_manage: bool,
    ) -> Result<Html> {
        let display = match current {
            Some(id) => account_html(accounts, id)?,
            None if can_manage => Html::text("Choose an account"),
            None => Html::text("Not set"),
        };
        let editor = Combobox::new(format!("{}-input", inline.id), &inline.name)
            .aria_label(&inline.label)
            .placeholder("Search accounts")
            .wrapper_class("mb-0")
            .value(current.map(|id| id.to_string()).unwrap_or_default())
            .options(options);
        render(InlineChoice {
            field: inline,
            display,
            editor: render(editor)?,
        })
    }

    pub(super) async fn setup_parts(&mut self, scope: &Scope) -> Result<Vec<Html>> {
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        let settings = match payroll.get_settings(scope, company).await {
            Ok(s) => Some(s),
            Err(PayrollError::PayrollNotConfigured) => None,
            Err(_) => {
                return Ok(vec![render(
                    EmptyState::forbidden("view payroll posting accounts")
                        .id("payroll-setup-forbidden"),
                )?]);
            }
        };
        if settings.is_some() {
            self.settings.clear();
        }
        // Payroll-only readers still see their own records if Ledger is withheld.
        let accounts = payroll
            .ledger
            .list_accounts(scope, company)
            .await
            .unwrap_or_default();
        let can_manage = self.allowed(scope, MANAGE_SETTINGS).await?;
        let liability = account_choices(&accounts, true);
        let expense = account_choices(&accounts, false);
        let mut current = Html::default();
        if settings.is_none() {
            current.push(&render(Alert {
                kind: StatusKind::Info,
                id: "payroll-settings-notice",
                class: "mb-3",
                body: Html::text(if !can_manage {
                    "Payroll posting is not set up yet. Someone who manages payroll posting must choose both accounts."
                } else if liability.is_empty() {
                    "Payroll posting is not set up yet, and the chart has no active liability account to choose. Add one on the Chart of accounts."
                } else {
                    "Payroll posting is not set up yet. Choose both accounts; they are saved together."
                }),
            })?);
        }
        let mut items = Vec::new();
        for (field, label) in SETTINGS_ACCOUNTS {
            let saved = settings.as_ref().map(|s| {
                if field == "net_pay_account_id" {
                    s.net_pay_account_id
                } else {
                    s.claims_payable_account_id
                }
            });
            let held = self.settings.get(field).and_then(|v| v.parse::<i64>().ok());
            let value = held.or(saved);
            let key = format!("settings-{field}");
            let inline = self.inline(
                &key,
                field,
                label,
                value.map(|id| id.to_string()).unwrap_or_default(),
                String::new(),
                can_manage,
            );
            items.push(Fact {
                title: label,
                id: if field == "net_pay_account_id" {
                    "payroll-settings-net-pay"
                } else {
                    "payroll-settings-claims-payable"
                },
                value: self.account_choice(
                    inline,
                    &accounts,
                    value,
                    liability.clone(),
                    can_manage,
                )?,
            });
        }
        current.push(&render(Facts {
            id: "payroll-settings-current",
            items,
        })?);
        let mut parts = vec![section(
            "payroll-settings",
            "payroll-settings-heading",
            "Owed to staff",
            "Liability accounts: a run's net pay is credited to the first, an approved claim to the second, until they are paid.",
            None,
            current,
        )?];
        let mappings = payroll
            .list_mappings(scope, company)
            .await
            .unwrap_or_default();
        let mut rows = Vec::new();
        for m in &mappings {
            let mut row = row(
                &format!("payroll-mapping-{}", m.id),
                &[
                    (
                        "kind",
                        if m.kind == "component" {
                            "Payroll component"
                        } else {
                            "Claim category"
                        }
                        .into(),
                    ),
                    ("code", m.code.clone()),
                    ("direction", direction_name(&m.direction).into()),
                ],
            );
            let (expense_needed, liability_needed) = needed(&m.direction);
            for (column, field, label, needed, current, options) in [
                (
                    "expense",
                    "expense_account_id",
                    "Expense account",
                    expense_needed,
                    m.expense_account_id,
                    &expense,
                ),
                (
                    "liability",
                    "liability_account_id",
                    "Liability account",
                    liability_needed,
                    m.liability_account_id,
                    &liability,
                ),
            ] {
                let cell = if needed {
                    let key = format!("mapping-{}-{field}", m.id);
                    let inline = self.inline(
                        &key,
                        field,
                        &format!("{label}, for {}", m.code),
                        current.map(|id| id.to_string()).unwrap_or_default(),
                        m.id.to_string(),
                        can_manage,
                    );
                    self.account_choice(inline, &accounts, current, options.clone(), can_manage)?
                } else {
                    Html::text("Not used")
                };
                row = row.slot(column, cell);
            }
            rows.push(row);
        }
        let table = self.table_with(
            0,
            rows,
            "Account mappings",
            EmptyState::missing(
                "No mappings yet",
                "A run or claim posts once each of its components or categories is mapped.",
            )
            .id("payroll-mappings-empty")
            .action(if can_manage {
                render(CreateButton {
                    noun: "Mapping",
                    id: "payroll-mapping-new-empty",
                    event: "new_mapping",
                    button_type: "button",
                    ..CreateButton::default()
                })?
            } else {
                Html::EMPTY
            }),
            false,
        )?;
        parts.push(section("payroll-mappings", "payroll-mappings-heading", "Account mappings", "A payroll component is a pay item code with the direction of its lines; a claim category is its code.", Some(mappings.len()), table)?);
        if can_manage && self.adding {
            parts.push(self.mapping_dialog(&expense, &liability)?);
        }
        let display = {
            let client = payroll.db.get().await?;
            let locale = self
                .pages
                .locales
                .resolve(&**client, scope, None)
                .await?
                .locale;
            self.pages
                .display
                .display(&**client, scope, None, locale)
                .await?
        };
        let settings_changes = payroll
            .list_settings_changes(scope, company)
            .await
            .unwrap_or_default();
        let mapping_changes = payroll
            .list_mapping_changes(scope, company)
            .await
            .unwrap_or_default();
        let people = self
            .pages
            .people(
                scope,
                settings_changes
                    .iter()
                    .chain(&mapping_changes)
                    .map(|c| c["changed_by_user_id"].as_i64()),
            )
            .await?;
        let mut history = Html::default();
        for (index, changes, caption, empty, prefix) in [
            (
                1,
                &settings_changes,
                "Posting account changes",
                "The posting accounts have not been set.",
                "payroll-settings-change",
            ),
            (
                2,
                &mapping_changes,
                "Mapping changes",
                "No mapping has been changed.",
                "payroll-mapping-change",
            ),
        ] {
            let mut rows = Vec::new();
            for c in changes {
                rows.push(history_row(
                    c,
                    prefix,
                    &accounts,
                    &mappings,
                    &people,
                    &display,
                    index == 1,
                )?);
            }
            history.push(&self.table(index, rows, caption, empty, false)?);
        }
        let count = settings_changes.len() + mapping_changes.len();
        let summary = match count {
            0 => "No changes yet".to_owned(),
            1 => "1 change".to_owned(),
            n => format!("{n} changes"),
        };
        parts.push(render(Card {
            id: "payroll-changes",
            class: "mt-6",
            body: render(Fold {
                id: "payroll-changes-fold",
                title: "History",
                summary: &summary,
                open: false,
                body: history,
            })?,
            ..Card::default()
        })?);
        Ok(parts)
    }

    /// The "+ Mapping" dialog: a payroll component (with the direction of
    /// its lines) or a claim category, and only the accounts its direction
    /// posts to. Its kind, code and direction are fixed once saved; its
    /// accounts are changed in place on its row afterwards.
    fn mapping_dialog(&self, expense: &[Choice], liability: &[Choice]) -> Result<Html> {
        let mut fields = vec![render(
            self.field(
                "mapping",
                "kind",
                "payroll-mapping-kind",
                "Kind",
                &self.mapping,
            )
            .kind(InputKind::Select)
            .options(choices(&[
                ("Payroll component", "component"),
                ("Claim category", "claim_category"),
            ])),
        )?];
        fields.push(render(
            self.field(
                "mapping",
                "code",
                "payroll-mapping-code",
                "Code",
                &self.mapping,
            )
            .hint("As the payroll or claims source names it.")
            .required()
            .attr("maxlength", "60"),
        )?);
        if get(&self.mapping, "kind") != "claim_category" {
            fields.push(render(
                self.field(
                    "mapping",
                    "direction",
                    "payroll-mapping-direction",
                    "Direction",
                    &self.mapping,
                )
                .kind(InputKind::Select)
                .options(choices(&[
                    ("Earning", "earning"),
                    ("Deduction", "deduction"),
                    ("Employer contribution", "employer"),
                ])),
            )?);
        }
        let (expense_needed, liability_needed) = needed(get(&self.mapping, "direction"));
        for (needed, field, label, options) in [
            (
                expense_needed,
                "expense_account_id",
                "Expense account",
                expense,
            ),
            (
                liability_needed,
                "liability_account_id",
                "Liability account",
                liability,
            ),
        ] {
            if needed {
                fields.push(render(
                    Combobox::new(
                        format!("payroll-mapping-{field}"),
                        format!("mapping[{field}]"),
                    )
                    .label(format!("{label} *"))
                    .aria_label(label)
                    .placeholder("Search accounts")
                    .value(get(&self.mapping, field))
                    .options(options.to_vec())
                    .errors(self.field_errors("mapping", field)),
                )?);
            }
        }
        let mut body = Html::EMPTY;
        for field in fields {
            body.push(&field);
        }
        render(DialogView {
            id: "payroll-mapping-modal",
            title: "New mapping",
            description: "Which accounts a payroll component or a claim category posts to.",
            cancel_event: "close_mapping",
            alert: self.dialog_alert("payroll-mapping-alert")?,
            form: render(HtmlFormView {
                id: "payroll-mapping-form",
                change: "change_mapping",
                submit: "save_mapping",
                body,
                cancel_event: "close_mapping",
                button_id: "payroll-mapping-save",
                label: "Save mapping",
                working: "Saving…",
            })?,
        })
    }
}

/// An account in a sentence ("Trade payables · 2000").
fn account_label(accounts: &[Account], id: Option<i64>) -> String {
    match id {
        None => "—".into(),
        Some(id) => accounts.iter().find(|a| a.id == id).map_or_else(
            || "Account unavailable".into(),
            |a| format!("{} · {}", a.name, a.code),
        ),
    }
}
/// An account as a cell shows it: the name, then the code muted.
fn account_html(accounts: &[Account], id: i64) -> Result<Html> {
    match accounts.iter().find(|a| a.id == id) {
        Some(a) => render(AccountName {
            name: &a.name,
            code: &a.code,
        }),
        None => Ok(Html::text("Account unavailable")),
    }
}
/// The accounts a choice may name, as the combobox lists them: the name
/// first, the code muted and searched alike.
fn account_choices(accounts: &[Account], liability: bool) -> Vec<Choice> {
    accounts
        .iter()
        .filter(|a| {
            a.postable()
                && if liability {
                    a.account_type == AccountType::Liability
                } else {
                    matches!(a.account_type, AccountType::Asset | AccountType::Expense)
                }
        })
        .map(|a| Choice::new(&a.name, a.id.to_string()).detail(&a.code))
        .collect()
}
fn history_row(
    c: &Value,
    prefix: &str,
    accounts: &[Account],
    mappings: &[crate::Mapping],
    people: &People,
    display: &DisplayContext,
    settings: bool,
) -> Result<FlexRow> {
    let pair = |old: &str, new: &str| {
        format!(
            "{} → {}",
            account_label(accounts, c[old].as_i64()),
            account_label(accounts, c[new].as_i64())
        )
    };
    let mut cells = vec![
        ("by", people.who(c["changed_by_user_id"].as_i64(), None)),
        ("reason", c["reason"].as_str().unwrap_or("—").into()),
    ];
    if settings {
        cells.extend([
            (
                "net",
                pair("old_net_pay_account_id", "new_net_pay_account_id"),
            ),
            (
                "claims",
                pair(
                    "old_claims_payable_account_id",
                    "new_claims_payable_account_id",
                ),
            ),
        ]);
    } else {
        cells.extend([
            (
                "mapping",
                mappings
                    .iter()
                    .find(|m| Some(m.id) == c["mapping_id"].as_i64())
                    .map_or_else(
                        || "A removed mapping".into(),
                        |m| format!("{} ({})", m.code, direction_name(&m.direction)),
                    ),
            ),
            (
                "expense",
                pair("old_expense_account_id", "new_expense_account_id"),
            ),
            (
                "liability",
                pair("old_liability_account_id", "new_liability_account_id"),
            ),
        ]);
    }
    let id = format!("{prefix}-{}", c["id"]);
    let mut row = row(&id, &cells);
    if let Some(at) = c["changed_at"]
        .as_str()
        .and_then(|s| s.parse::<Timestamp>().ok())
    {
        let r = display.render(at, Format::DateTime, Precision::Minute);
        row = row.cell(
            "when",
            bilibi_ui::flex_table::Cell::instant(bilibi_ui::flex_table::Instant::new(
                &r.iso,
                &r.text,
                &r.company,
                &r.utc,
                r.mode.as_str(),
            )),
        );
    }
    Ok(row)
}

/// Whether an in-place value belongs to a mapping row (its id) rather than
/// to the settings (no id).
fn id_value_is_row(id_value: &str) -> bool {
    !id_value.is_empty()
}
