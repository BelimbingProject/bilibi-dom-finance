//! Publishing and proposing reusable charts from the existing chart page.
use super::*;
use crate::MANAGE_TEMPLATES;

pub(super) struct Publication {
    company: i64,
    company_name: String,
    proposal: bool,
    key: String,
    name: String,
    description: String,
    groups: Vec<view::Account>,
    chosen: Vec<String>,
    options: Vec<Choice>,
    review: Option<Review>,
    refusal: Refusal,
    notice: String,
    columns: ListColumns,
}
impl Publication {
    fn contract(&self) -> &'static str {
        if self.proposal {
            "finance.ledger.chart_template.propose"
        } else {
            "finance.ledger.chart_template.save"
        }
    }
    fn input(&self) -> Value {
        if self.proposal {
            if self.key.is_empty() {
                json!({"clear":true})
            } else {
                json!({"template":self.key})
            }
        } else {
            json!({"key":self.key,"name":self.name,"description":self.description,"optional_group_codes":self.chosen})
        }
    }
    fn form(&mut self) -> Result<Html> {
        let errors = |field: &str| {
            self.refusal
                .field(field)
                .iter()
                .map(|m| error_text(field, m))
                .collect::<Vec<_>>()
        };
        let mut body = Html::EMPTY;
        let messages = self.refusal.alert(&[
            "key",
            "name",
            "description",
            "optional_group_codes",
            "template",
        ]);
        if !messages.is_empty() || !self.notice.is_empty() {
            body.push(&render(Alert {
                id: "ledger-publication-alert",
                kind: StatusKind::Error,
                body: Html::text(&format!("{} {}", messages.join(" "), self.notice)),
                ..Alert::default()
            })?);
        }
        if let Some(review) = &self.review {
            let after = &review.facts["after"];
            body.push(&render(SectionHeading {
                id: "ledger-publication-review",
                title: after["name"].as_str().unwrap_or("No proposal"),
                description: after["description"]
                    .as_str()
                    .unwrap_or("New books will have no proposed starter chart."),
                ..SectionHeading::default()
            })?);
            if self.proposal {
                body.push(&render(Note { id:"ledger-proposal-review-note",text:"This starter chart is offered to every new book. Applying it to a company needs its own preview and confirmation." })?);
            } else {
                let rows: Vec<crate::ChartTemplateRow> =
                    view::parse(after["rows"].clone()).unwrap_or_default();
                let sections: Vec<crate::ChartTemplateSection> =
                    view::parse(after["sections"].clone()).unwrap_or_default();
                let rows = rows
                    .iter()
                    .map(|row| {
                        FlexRow::new(row.code.clone())
                            .cell("code", Cell::text(&row.code))
                            .cell("name", Cell::text(&row.name))
                            .cell(
                                "type",
                                Cell::text(view::type_label(row.account_type.as_str())),
                            )
                            .cell(
                                "kind",
                                Cell::text(if row.is_group { "Group" } else { "Account" }),
                            )
                            .cell(
                                "group",
                                Cell::text(row.parent_code.as_deref().unwrap_or("—")),
                            )
                            .cell(
                                "section",
                                Cell::text(
                                    row.section
                                        .as_ref()
                                        .and_then(|key| {
                                            sections.iter().find(|section| &section.key == key)
                                        })
                                        .map_or("Core", |section| section.name.as_str()),
                                ),
                            )
                            .cell(
                                "retained",
                                Cell::text(if row.retained_earnings { "Yes" } else { "—" }),
                            )
                    })
                    .collect();
                let mut table = FlexTable::new(
                    "ledger-publication-table",
                    self.columns.flex_columns(),
                    rows,
                );
                table.caption = "Chart to save";
                table.framed = false;
                table.event = "publication_grid";
                table.zoom = self.columns.zoom;
                table.suggestions = self.columns.flex_suggestions();
                table.add_query = &self.columns.add_query;
                body.push(&render(table)?);
                body.push(&render(Note {id:"ledger-publication-review-note",text:"Saves a copy of the active accounts and groups for other companies to start from. Balances, currency and report layout are not included. Optional sections start unselected when applying."})?);
            }
        } else if self.proposal {
            body.push(&render(
                Combobox::new("ledger-proposal-choice", "publish[key]")
                    .label("Proposed starter chart")
                    .value(&self.key)
                    .options(self.options.clone())
                    .placeholder("No proposal")
                    .hint("Clear to propose none. Existing books and accounts are unchanged.")
                    .errors(errors("template")),
            )?);
        } else {
            for (key, label, value) in [
                ("key", "Chart key", &self.key),
                ("name", "Name", &self.name),
                ("description", "Description", &self.description),
            ] {
                body.push(&render(
                    Field::in_form("publish", key)
                        .id(format!("ledger-publish-{key}"))
                        .label(label)
                        .value(value)
                        .required()
                        .errors(errors(key)),
                )?);
            }
            body.push(&render(SectionHeading {id:"ledger-publish-optional",title:"Optional sections",description:"Pick the groups a company can leave out. Everything under a picked group becomes optional too.",..SectionHeading::default()})?);
            for group in &self.groups {
                body.push(&render(
                    Field::in_form("publish_groups", &group.id.to_string())
                        .id(format!("ledger-publish-group-{}", group.id))
                        .label(format!("{} · {}", group.name, group.code))
                        .kind(InputKind::Checkbox)
                        .value("true")
                        .checked(self.chosen.contains(&group.code))
                        .errors(errors("optional_group_codes")),
                )?);
            }
        }
        let mut actions = render(Button {
            id: "ledger-publish-cancel",
            event: "close_publication",
            button_type: "button",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "ledger-publish-review",
            event: if self.review.is_some() {
                "commit_publication"
            } else {
                "review_publication"
            },
            button_type: "button",
            variant: ButtonVariant::Primary,
            body: Html::text(if self.review.is_none() {
                "Review"
            } else if self.proposal {
                "Save proposal"
            } else {
                "Publish template"
            }),
            ..Button::default()
        })?);
        let form = if self.review.is_some() {
            // FlexTable owns its column-search form. A review has no draft
            // fields, so keep it outside Form rather than nesting forms.
            body.push(&dialog_actions(actions)?);
            body
        } else {
            render(Form {
                id: "ledger-publication-form",
                event: "validate_publication",
                change: true,
                class: "mt-4",
                body,
                actions: dialog_actions(actions)?,
            })?
        };
        render(Modal {
            id: "ledger-publication-modal",
            width: if self.proposal {
                bilibi_ui::ModalWidth::Narrow
            } else {
                bilibi_ui::ModalWidth::Wide
            },
            title: if self.proposal {
                "Proposed starter chart"
            } else {
                "Save reusable chart"
            },
            description: &format!(
                "Saves the chart of {} so other companies can start from it.",
                self.company_name
            ),
            cancel_event: "close_publication",
            body: form,
            ..Modal::default()
        })
    }
}
impl Accounts {
    pub(super) async fn publication_event(
        &mut self,
        r: &LiveRequest<'_, Scope>,
        value: &Value,
        event: &str,
    ) -> Result<()> {
        if !self.pages.allowed(r.scope, MANAGE_TEMPLATES).await? {
            self.flash
                .error("You do not have permission to manage chart templates.");
            return Ok(());
        }
        if matches!(event, "publish_chart" | "propose_chart") {
            let Some(company) = Pages::company(r.scope) else {
                return Ok(());
            };
            let chart = self.pages.chart(r.scope).await?.unwrap_or_default();
            let listed = match self
                .pages
                .read(r.scope, "finance.ledger.chart_template.list", json!({}))
                .await?
            {
                Read::Found(v) => v,
                _ => return Ok(()),
            };
            let options = listed["records"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| Some(Choice::new(v["name"].as_str()?, v["key"].as_str()?)))
                .collect();
            let columns = [
                "code", "name", "type", "kind", "group", "section", "retained",
            ]
            .into_iter()
            .map(|key| {
                BuiltinColumn::new(
                    key,
                    match key {
                        "code" => "Code",
                        "name" => "Name",
                        "type" => "Type",
                        "kind" => "Kind",
                        "group" => "Group code",
                        "section" => "Section",
                        "retained" => "Retained earnings",
                        _ => "",
                    },
                )
            })
            .collect();
            self.publication = Some(Publication {
                company,
                company_name: self.pages.company_name(r.scope).await?,
                proposal: event == "propose_chart",
                key: if event == "propose_chart" {
                    listed["proposal"]["configured"]
                        .as_str()
                        .unwrap_or_default()
                        .into()
                } else {
                    String::new()
                },
                name: String::new(),
                description: String::new(),
                groups: chart.into_iter().filter(|a| a.is_group).collect(),
                chosen: Vec::new(),
                options,
                review: None,
                refusal: Refusal::default(),
                notice: String::new(),
                columns: ListColumns::mount("ledger-publication-table", columns)
                    .map_err(|_| Error::Internal)?,
            });
            return Ok(());
        }
        if event == "close_publication" {
            self.publication = None;
            return Ok(());
        }
        let Some(draft) = &mut self.publication else {
            return Ok(());
        };
        if Pages::company(r.scope) != Some(draft.company) {
            self.publication = None;
            self.flash
                .error("The current company changed. Review the template again.");
            return Ok(());
        }
        match event {
            "validate_publication" if draft.review.is_none() => {
                let fields = params::form_values(value, "publish");
                draft.key = fields.get("key").cloned().unwrap_or_default();
                draft.name = fields.get("name").cloned().unwrap_or_default();
                draft.description = fields.get("description").cloned().unwrap_or_default();
                draft.chosen = draft
                    .groups
                    .iter()
                    .filter(|g| value[format!("publish_groups[{}]", g.id)].as_str() == Some("true"))
                    .map(|g| g.code.clone())
                    .collect();
                draft.refusal = Refusal::default();
                draft.notice.clear();
            }
            "review_publication" => {
                match self
                    .pages
                    .review(
                        r.scope,
                        r.credential,
                        event,
                        draft.contract(),
                        draft.input(),
                    )
                    .await?
                {
                    Ok(review) => {
                        let rows: Vec<crate::ChartTemplateRow> =
                            view::parse(review.facts["after"]["rows"].clone()).unwrap_or_default();
                        draft.columns.load(&rows, |r| r.code.clone());
                        draft.review = Some(review);
                    }
                    Err(Failure::Refused(refusal)) => draft.refusal = refusal,
                    Err(failure) => draft.notice = failure.text("The template"),
                }
            }
            "commit_publication" => {
                if let Some(review) = draft.review.take() {
                    match self
                        .pages
                        .commit(r.scope, r.credential, event, draft.contract(), review)
                        .await?
                    {
                        Ok(_) => {
                            self.flash.success(if draft.proposal {
                                "Starter chart proposal saved."
                            } else {
                                "Reusable chart published."
                            });
                            self.publication = None;
                        }
                        Err(Failure::Refused(refusal)) => draft.refusal = refusal,
                        Err(failure) => draft.notice = failure.text("The template"),
                    }
                }
            }
            "publication_grid" => {
                if let Some(draft) = &mut self.publication {
                    draft.columns.handle(&params::string_values(value));
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn publication_dialog(&mut self) -> Result<Html> {
        self.publication
            .as_mut()
            .map_or(Ok(Html::EMPTY), Publication::form)
    }
}
