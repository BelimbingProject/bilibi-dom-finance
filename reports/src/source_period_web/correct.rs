//! Previewing and confirming a schedule through its contract (a new one, a
//! correction that replaces a retained one, or a withdrawal), and the
//! correction and withdraw dialogs. A correction opens the "+ Schedule"
//! dialog filled from the retained schedule: its details and its rows, pasted
//! back in, so the person changes what was wrong and gives a new reference.
use super::paste::Paste;
use super::*;
use crate::source_period::{REPLACE, WITHDRAW};

/// The typed fields the dialogs show beside themselves; any other refusal
/// is said in the dialog's own alert.
const FIELDS: [&str; 6] = [
    "source_reference",
    "source_system",
    "period_start",
    "period_end",
    "accounting_timezone",
    "reason",
];

/// A retained schedule's rows as a pasted table with a heading row, which
/// the column step reads back the same way.
fn rows_as_paste(record: &Value) -> String {
    let rows = record["rows"].as_array().cloned().unwrap_or_default();
    let clean = |v: &Value| v.as_str().unwrap_or("").replace(['\t', '\n', '\r'], " ");
    let mut text = String::new();
    let accounts = rows.iter().any(|r| r["account_id"].is_i64());
    if accounts {
        text.push_str("Row reference\tAccount\tDebit\tCredit\n");
        for r in &rows {
            let amount = clean(&r["amount"]);
            let (debit, credit) = if r["side"] == "debit" {
                (amount, String::new())
            } else {
                (String::new(), amount)
            };
            text.push_str(&format!(
                "{}\t{}\t{debit}\t{credit}\n",
                clean(&r["source_id"]),
                clean(&r["account_code"])
            ));
        }
    } else {
        text.push_str("Row reference\tReference\tAuthority\tDate\tAmount\n");
        for r in &rows {
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                clean(&r["source_id"]),
                clean(&r["reference"]),
                clean(&r["authority"]),
                clean(&r["event_date"]),
                clean(&r["amount"])
            ));
        }
    }
    text
}

impl State {
    /// Opens the "+ Schedule" dialog as a correction of the shown schedule.
    pub(super) fn open_correction(&mut self) -> Result<()> {
        let record = self.records.first().cloned().ok_or(Error::NotFound)?;
        if !record["correction"].is_null() {
            self.message = Some("This schedule has already been withdrawn or replaced.".into());
            return Ok(());
        }
        let id = record["id"].as_i64().ok_or(Error::NotFound)?;
        let reference = record["source_reference"].as_str().unwrap_or("").to_owned();
        self.values.clear();
        for key in [
            "source_system",
            "period_start",
            "period_end",
            "accounting_timezone",
        ] {
            self.values
                .insert(key.into(), record[key].as_str().unwrap_or("").to_owned());
        }
        self.errors.clear();
        self.dialog_message = None;
        self.paste_text = rows_as_paste(&record);
        self.paste = Some(Paste::new(
            &self.paste_text,
            self.kind == "comparative" || self.kind == "movement",
        ));
        self.replacing = Some((id, reference));
        self.adding = true;
        Ok(())
    }

    /// Reviews the pasted schedule through `contract`: the import, or a
    /// replacement of the schedule being corrected.
    pub(super) async fn preview(
        &mut self,
        r: &LiveRequest<'_, Scope>,
        contract: &'static str,
        event: &'static str,
        confirm: &'static str,
    ) -> Result<()> {
        self.review = None;
        self.errors.clear();
        self.message = None;
        self.dialog_message = None;
        let company = Self::company(r.scope)?;
        let (mut input, numbers) = match self.schedule(r.scope, company).await? {
            Ok(schedule) => schedule,
            Err(why) => {
                self.dialog_message = Some(why);
                return Ok(());
            }
        };
        if contract == REPLACE {
            let Some((id, _)) = &self.replacing else {
                return Err(Error::Invalid);
            };
            input["replaces_id"] = json!(id);
            input["reason"] = json!(self.values.get("reason").cloned().unwrap_or_default());
        }
        let call = Call {
            scope: r.scope,
            credential: r.credential,
            service: PAGES,
            event,
            contract,
        };
        self.reviewed(&call, input, &numbers, confirm).await;
        Ok(())
    }

    /// Reviews a withdrawal of the shown schedule.
    pub(super) async fn preview_withdraw(
        &mut self,
        r: &LiveRequest<'_, Scope>,
        value: &Value,
    ) -> Result<()> {
        self.review = None;
        self.errors.clear();
        self.dialog_message = None;
        self.values =
            params::form_values_checked(value, "withdraw", 1024).map_err(Error::refused)?;
        let company = Self::company(r.scope)?;
        let id = self.id.ok_or(Error::NotFound)?;
        let input = json!({"company_id":company,"kind":self.kind,"id":id,"reason":self.values.get("reason").cloned().unwrap_or_default()});
        let call = Call {
            scope: r.scope,
            credential: r.credential,
            service: PAGES,
            event: "preview_withdraw",
            contract: WITHDRAW,
        };
        self.reviewed(&call, input, &[], "do_withdraw").await;
        Ok(())
    }

    /// Keeps a valid review, or puts each refusal beside its field, a row's
    /// refusal under the pasted row it came from, and the rest in the
    /// dialog's alert.
    async fn reviewed(
        &mut self,
        call: &Call<'_>,
        input: Value,
        numbers: &[usize],
        confirm: &'static str,
    ) {
        match self.pages.contracts.preview(call, input.clone()).await {
            Ok(answer) => {
                let result = &answer["results"][0];
                if result["valid"] == true {
                    self.review = Some(Review {
                        input,
                        facts: result["preview"]["result"]["changes"].clone(),
                        contract: call.contract,
                        confirm,
                    });
                    return;
                }
                let fields: BTreeMap<String, Vec<String>> =
                    serde_json::from_value(result["error"]["fields"].clone()).unwrap_or_default();
                let mut other = Vec::new();
                for (path, messages) in fields {
                    let pasted = path
                        .strip_prefix("rows.")
                        .and_then(|rest| rest.split('.').next())
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| numbers.get(i));
                    if let Some(number) = pasted {
                        other.push(format!("Row {number}: {}", messages.join(" ")));
                    } else if FIELDS.contains(&path.as_str()) {
                        self.errors.insert(path, messages);
                    } else {
                        other.extend(messages);
                    }
                }
                self.dialog_message = Some(if other.is_empty() {
                    "Check the fields marked above and preview again.".into()
                } else {
                    other.join(" ")
                });
            }
            Err(_) => {
                self.dialog_message = Some(
                    "The schedule could not be reviewed. Check your access and try again.".into(),
                )
            }
        }
    }

    /// Confirms the review through the contract it was reviewed with.
    pub(super) async fn commit(&mut self, r: &LiveRequest<'_, Scope>, event: &str) -> Result<()> {
        let Some(review) = self.review.take() else {
            return Ok(());
        };
        if review.confirm != event {
            return Err(Error::Invalid);
        }
        let call = Call {
            scope: r.scope,
            credential: r.credential,
            service: PAGES,
            event: review.confirm,
            contract: review.contract,
        };
        match self.pages.contracts.commit(&call, review.input).await {
            Ok(_) => {
                self.message = Some(match review.contract {
                    REPLACE => format!(
                        "{} was retained and this schedule is now marked replaced. Open the new schedule to attach its supporting report.",
                        review.facts["source_reference"].as_str().unwrap_or("The correction")
                    ),
                    WITHDRAW => "The schedule was withdrawn. It stays here as evidence and no longer counts.".into(),
                    _ => "The source schedule was retained. Open it to attach its supporting report.".into(),
                });
                self.values.clear();
                self.paste_text.clear();
                self.paste = None;
                self.adding = false;
                self.replacing = None;
                self.withdrawing = false;
            }
            Err(_) => self.dialog_message = Some("The schedule changed. Preview it again.".into()),
        }
        Ok(())
    }

    /// The withdraw dialog: why, then a review.
    pub(super) fn withdraw_modal(&self) -> Result<Html> {
        let reference = self
            .records
            .first()
            .and_then(|r| r["source_reference"].as_str())
            .unwrap_or("");
        let mut fields = render(
            Field::in_form("withdraw", "reason")
                .id("withdraw-reason")
                .label("Why it is withdrawn")
                .kind(InputKind::Textarea)
                .value(self.values.get("reason").cloned().unwrap_or_default())
                .hint(
                    "For example, retained twice or for the wrong period. The auditor reads this.",
                )
                .errors(self.errors.get("reason").cloned().unwrap_or_default())
                .attr("rows", "3")
                .required(),
        )?;
        if let Some(message) = &self.dialog_message {
            fields.push(&render(Alert {
                id: "source-period-withdraw-alert",
                kind: StatusKind::Error,
                body: Html::text(message),
                ..Alert::default()
            })?);
        }
        let mut buttons = render(Button {
            button_type: "button",
            event: "close_withdraw",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "source-period-withdraw-preview",
            button_type: "submit",
            event: "preview_withdraw",
            variant: ButtonVariant::Primary,
            body: Html::text("Review withdrawal"),
            ..Button::default()
        })?);
        render(Modal {
            id: "source-period-withdraw-modal",
            title: "Withdraw source schedule",
            description: &format!(
                "{reference} stays as evidence, marked withdrawn, and no longer counts."
            ),
            cancel_event: "close_withdraw",
            body: render(Form {
                id: "source-period-withdraw-form",
                event: "preview_withdraw",
                class: "mt-4",
                body: fields,
                actions: render(Actions { buttons })?,
                ..Form::default()
            })?,
            ..Modal::default()
        })
    }

    /// The review of a withdrawal, confirmed by "Withdraw schedule".
    pub(super) fn withdraw_review_modal(&self, review: &Review) -> Result<Html> {
        let facts = &review.facts;
        let schedule = &facts["schedule"];
        let mut content = render(Alert {
            kind: StatusKind::Warning,
            body: Html::text(facts["notice"].as_str().unwrap_or("")),
            ..Alert::default()
        })?;
        let fact = |title: &'static str, value: String| Fact {
            title,
            value: Html::text(&value),
            ..Fact::default()
        };
        content.push(&render(Facts {
            id: "source-period-withdraw-facts",
            items: vec![
                fact(
                    "Schedule",
                    schedule["source_reference"].as_str().unwrap_or("").into(),
                ),
                fact(
                    "Period",
                    format!(
                        "{} – {}",
                        schedule["period_start"].as_str().unwrap_or(""),
                        schedule["period_end"].as_str().unwrap_or("")
                    ),
                ),
                fact("Reason", facts["reason"].as_str().unwrap_or("").into()),
            ],
        })?);
        let mut buttons = render(Button {
            button_type: "button",
            event: "cancel_review",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            button_type: "button",
            event: review.confirm,
            variant: ButtonVariant::Primary,
            body: Html::text("Withdraw schedule"),
            ..Button::default()
        })?);
        content.push(&render(super::form::Footer {
            actions: render(Actions { buttons })?,
        })?);
        render(Modal {
            id: "source-period-withdraw-review-modal",
            title: "Review withdrawal",
            description: "Withdraw this source schedule for the current company.",
            cancel_event: "cancel_review",
            body: content,
            ..Modal::default()
        })
    }
}
