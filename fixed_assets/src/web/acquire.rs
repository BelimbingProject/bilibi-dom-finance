//! "+ Asset": register an asset from a purchase the person picks (a posted
//! bill line or journal line on the category's cost account), never from typed
//! record numbers. The draft lives on the server as the fields change.
use super::{input::Draft, *};
use crate::{AcquisitionSource, Category, SourceKind};
use bilibi_ui::{Choice, InputKind};

pub(super) struct Acquire {
    pub draft: Draft,
    categories: Vec<Category>,
    sources: Vec<AcquisitionSource>,
    /// The revision `sources` were read for.
    sources_for: Option<i64>,
    alert: String,
}

/// A purchase as the combobox stores it: `bill:12:1` or `entry:40:2`.
fn key(s: &AcquisitionSource) -> String {
    let kind = match s.source {
        SourceKind::Bill => "bill",
        SourceKind::Entry => "entry",
    };
    format!("{kind}:{}:{}", s.source_id, s.line_number)
}

impl Acquire {
    pub(super) async fn open(pages: &Pages, s: &Scope) -> crate::Result<Self> {
        let c = company(s).ok_or(FixedAssetsError::CompanyNotFound)?;
        let categories = pages
            .read::<Vec<Category>>(
                s,
                "finance.fixed_assets.category.list",
                serde_json::json!({"company_id":c}),
                "records",
            )
            .await?;
        let mut draft = Draft::default();
        // One category is the only choice, so it is chosen.
        if let [only] = categories.as_slice() {
            draft
                .values
                .insert("category_revision_id".into(), only.revision.id.to_string());
        }
        let mut this = Self {
            draft,
            categories,
            sources: Vec::new(),
            sources_for: None,
            alert: String::new(),
        };
        this.load_sources(pages, s).await?;
        Ok(this)
    }

    fn revision(&self) -> Option<i64> {
        self.draft.value("category_revision_id").parse().ok()
    }

    async fn load_sources(&mut self, pages: &Pages, s: &Scope) -> crate::Result<()> {
        let revision = self
            .revision()
            .filter(|r| self.categories.iter().any(|c| c.revision.id == *r));
        if revision == self.sources_for {
            return Ok(());
        }
        self.sources = match revision {
            Some(r) => {
                let c = company(s).ok_or(FixedAssetsError::CompanyNotFound)?;
                pages
                    .read(
                        s,
                        "finance.fixed_assets.acquisition.sources",
                        serde_json::json!({"company_id":c,"category_revision_id":r}),
                        "records",
                    )
                    .await?
            }
            None => Vec::new(),
        };
        self.sources_for = revision;
        Ok(())
    }

    fn source(&self) -> Option<&AcquisitionSource> {
        let chosen = self.draft.value("source");
        self.sources.iter().find(|s| key(s) == chosen)
    }

    /// The person changed a field: read the purchases of a newly chosen
    /// category, drop a purchase it no longer lists, and fill the
    /// description and in-service date from a newly chosen purchase when the
    /// person has not typed their own.
    pub(super) async fn change(
        &mut self,
        pages: &Pages,
        s: &Scope,
        v: &serde_json::Value,
    ) -> crate::Result<()> {
        let before = self.source().cloned();
        let mut posted = Draft::posted(v);
        if let Some(previous) = &before {
            for (field, filled) in [
                ("description", previous.description.clone()),
                ("in_service_date", previous.acquisition_date.to_string()),
            ] {
                if posted.value(field) == filled {
                    posted.values.remove(field);
                }
            }
        }
        self.draft = posted;
        self.alert.clear();
        self.load_sources(pages, s).await?;
        if self.source().is_none() {
            self.draft.values.remove("source");
        }
        if let Some(chosen) = self.source().cloned() {
            if self.draft.value("description").trim().is_empty() {
                self.draft
                    .values
                    .insert("description".into(), chosen.description.clone());
            }
            if self.draft.value("in_service_date").is_empty() {
                self.draft.values.insert(
                    "in_service_date".into(),
                    chosen.acquisition_date.to_string(),
                );
            }
        }
        Ok(())
    }

    pub(super) fn refused(&mut self, e: &FixedAssetsError) {
        self.draft.refused(e);
        // The source's own fields are one choice here: "Bought on".
        for error in &mut self.draft.errors {
            if matches!(
                error.field,
                "source_id"
                    | "line_number"
                    | "entry_id"
                    | "entry_line_number"
                    | "bill_id"
                    | "bill_line_number"
            ) {
                error.field = "source";
            }
        }
        self.alert = match e {
            FixedAssetsError::Invalid(fields)
            | FixedAssetsError::Ledger(bilibi_finance_ledger::LedgerError::Invalid(fields)) => {
                fields
                    .iter()
                    .filter(|f| f.field == "base")
                    .map(|f| f.message.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            FixedAssetsError::BillNotPosted
            | FixedAssetsError::BillLineNotFound
            | FixedAssetsError::EntryLineNotFound
            | FixedAssetsError::NotAcquisitionDebit
            | FixedAssetsError::AcquisitionReversed => {
                self.draft
                    .errors
                    .push(bilibi_finance_ledger::FieldError::new(
                        "source",
                        "is no longer a purchase this category can register; choose another",
                    ));
                String::new()
            }
            e => refusal_text(e),
        };
    }

    /// Registers the asset through the same contract the agent doors call.
    pub(super) async fn submit(
        &self,
        pages: &Pages,
        s: &Scope,
        credential: &str,
    ) -> crate::Result<String> {
        let c = company(s).ok_or(FixedAssetsError::CompanyNotFound)?;
        let d = &self.draft;
        let units = pages.minor_units(s, c).await?;
        let mut errors = Vec::new();
        let source = self.source();
        if source.is_none() {
            errors.push(bilibi_finance_ledger::FieldError::new(
                "source",
                "can't be blank",
            ));
        }
        let acquisition = match d.acquisition(units) {
            Ok(a) => Some(a),
            Err(FixedAssetsError::Invalid(fields)) => {
                errors.extend(fields);
                None
            }
            Err(e) => return Err(e),
        };
        if !errors.is_empty() {
            return Err(FixedAssetsError::Invalid(errors));
        }
        let (Some(source), Some(acquisition)) = (source, acquisition) else {
            return Err(FixedAssetsError::Invalid(vec![]));
        };
        let mut input = serde_json::to_value(&acquisition).map_err(|_| Error::Internal)?;
        input["source"] = serde_json::to_value(source.source).map_err(|_| Error::Internal)?;
        input["source_id"] = source.source_id.into();
        input["line_number"] = source.line_number.into();
        pages
            .submit(
                s,
                credential,
                "register_asset",
                "finance.fixed_assets.asset.acquire",
                input,
            )
            .await?;
        Ok(acquisition.description)
    }

    pub(super) fn render(&self, units: Option<MinorUnits>) -> Result<Html> {
        let d = &self.draft;
        if self.categories.is_empty() {
            return Dialog {
                id: "asset-modal",
                title: "New fixed asset",
                description: "",
                change: "",
                submit: "register_asset",
                cancel: "close_asset",
                verb: "Register asset",
                working: "Registering…",
                alert: "",
                fields: vec![],
                extra: render(
                    EmptyState::missing(
                        "No asset categories yet",
                        "A category sets the accounts and useful life for its assets.",
                    )
                    .id("asset-modal-no-category")
                    .action(render(Button {
                        id: "asset-modal-add-category",
                        href: "/finance/fixed-assets/categories?new=1",
                        variant: ButtonVariant::Primary,
                        body: Html::text("Add a category"),
                        ..Button::default()
                    })?),
                )?,
                can_submit: false,
            }
            .render();
        }
        let categories = self
            .categories
            .iter()
            .map(|c| {
                Choice::new(&c.name, c.revision.id.to_string()).detail(format!(
                    "{} · {} months",
                    input::method(c.revision.defaults.assumptions.method),
                    c.revision.defaults.assumptions.life_months
                ))
            })
            .collect();
        let mut category = d
            .field(
                "category_revision_id",
                "Category",
                InputKind::Select,
                true,
                "asset",
            )
            .options(categories)
            .placeholder("Choose a category");
        let mut source = d
            .field("source", "Bought on", InputKind::Select, true, "asset")
            .options(
                self.sources
                    .iter()
                    .map(|s| {
                        let label = match (&s.source, &s.bill_number, &s.supplier) {
                            (SourceKind::Bill, number, supplier) => format!(
                                "{} · Bill {}{}",
                                s.description,
                                number.as_deref().unwrap_or("without number"),
                                supplier
                                    .as_deref()
                                    .map(|n| format!(" from {n}"))
                                    .unwrap_or_default()
                            ),
                            (SourceKind::Entry, _, _) => {
                                format!("{} · Journal entry {}", s.description, s.entry_number)
                            }
                        };
                        Choice::new(label, key(s)).detail(format!(
                            "{} · {}",
                            s.acquisition_date,
                            s.amount.display(units)
                        ))
                    })
                    .collect(),
            )
            .placeholder("Search purchases by description, supplier or amount");
        if self.revision().is_none() {
            category = category.hint("Its cost account lists the purchases to register.");
            source = source.disabled().hint("Choose a category first.");
        } else if self.sources.is_empty() {
            let account = self
                .categories
                .iter()
                .find(|c| Some(c.revision.id) == self.revision())
                .map(|c| c.name.as_str())
                .unwrap_or("this category");
            source = source.disabled().hint(format!(
                "No unregistered purchases on {account}'s cost account. Post the bill or journal first."
            ));
        } else if let Some(chosen) = self.source() {
            source = source.hint(format!(
                "Cost {}, bought {}.",
                chosen.amount.display(units),
                chosen.acquisition_date
            ));
        }
        let fields = vec![
            category,
            source,
            d.field("description", "Description", InputKind::Text, true, "asset"),
            d.field(
                "in_service_date",
                "In service from",
                InputKind::Date,
                true,
                "asset",
            ),
            d.field(
                "residual_value",
                "Residual value",
                InputKind::Text,
                false,
                "asset",
            )
            .hint("What it will be worth at the end of its life. Leave empty for none."),
        ];
        Dialog {
            id: "asset-modal",
            title: "New fixed asset",
            description: "",
            change: "asset_change",
            submit: "register_asset",
            cancel: "close_asset",
            verb: "Register asset",
            working: "Registering…",
            alert: &self.alert,
            fields,
            extra: Html::EMPTY,
            can_submit: !self.sources.is_empty(),
        }
        .render()
    }
}
use bilibi_finance_ledger::MinorUnits;
