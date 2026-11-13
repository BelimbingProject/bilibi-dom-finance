//! A tax code's SST-02 item, chosen in place: on the code's row in SST setup,
//! and on a return that the missing choice keeps from finalising. Both pages
//! draw the same searchable choice and save through the same facade call, so
//! a person fixes the blocker where they meet it.
use super::{format as f, render, Pages, DENIED};
use crate::{MappingInput, Sst, SstError, TaxSource, TaxType, MANAGE_SETTINGS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{Choice, Combobox, CommitStates, CommitStatus, Html, InlineChoice, InlineField};

/// The live event both pages raise to save a choice.
pub(super) const SAVE_EVENT: &str = "save_mapping";

/// The value an item editor sends for "Not reported" (a combobox's empty
/// value means nothing chosen).
const NOT_REPORTED: &str = "none";

/// One tax code and the item it reports in now: `None` when none was chosen
/// yet, `Some(None)` when it is deliberately not reported.
pub(super) struct ItemChoice<'a> {
    pub source: TaxSource,
    pub tax_code_id: i64,
    pub code: &'a str,
    pub current: Option<Option<&'a str>>,
}

impl ItemChoice<'_> {
    /// The key a row's outcome and its save event carry: `source-id`.
    pub(super) fn key(&self) -> String {
        format!("{}-{}", self.source.as_str(), self.tax_code_id)
    }

    /// The choice in place, from the items the code's source can report in
    /// ("Not reported" too), for someone who manages the setup; read alone by
    /// anyone else.
    pub(super) fn render(&self, can_manage: bool, statuses: &CommitStates) -> Result<Html> {
        let key = self.key();
        let display = match self.current {
            None => f::badge(
                "Not chosen",
                bilibi_ui::BadgeKind::Warning,
                "whitespace-nowrap",
            )?,
            Some(None) => Html::text("Not reported"),
            Some(Some(b)) => box_html(b)?,
        };
        let value = match self.current {
            None => String::new(),
            Some(None) => NOT_REPORTED.to_owned(),
            Some(Some(b)) => b.to_owned(),
        };
        let label = format!(
            "Reports in, for {} tax code {}",
            f::source_name(self.source),
            self.code
        );
        let mut field = InlineField::new(format!("sst-map-{key}"), "box", &label, value.clone())
            .version(0)
            .status(statuses.get(&key));
        field.id_value = key.clone();
        field.save_event = SAVE_EVENT.into();
        if !can_manage {
            field = field.readonly();
        }
        let mut options = vec![Choice::new("Not reported", NOT_REPORTED)];
        options.extend(Sst::mappable_boxes(self.source).into_iter().map(|b| {
            Choice::new(box_title(b.form_box.key), b.form_box.key)
                .detail(format!("item {}", b.form_box.item))
        }));
        let editor = Combobox::new(format!("sst-map-{key}-input"), "box")
            .aria_label(&label)
            .placeholder("Search SST-02 items")
            .wrapper_class("mb-0")
            .value(value)
            .options(options);
        render(InlineChoice {
            field,
            display,
            editor: render(editor)?,
        })
    }
}

/// One tax code's item saved from its place on a page (`id` is `source-id`).
/// The outcome goes to `statuses` under that key; `true` when it was saved.
pub(super) async fn save(
    pages: &Pages,
    scope: &Scope,
    v: &serde_json::Value,
    statuses: &mut CommitStates,
) -> Result<bool> {
    let key = v["id"].as_str().ok_or(Error::Invalid)?.to_owned();
    let (source, tax_code_id) = key
        .split_once('-')
        .and_then(|(s, id)| Some((s.to_owned(), id.parse::<i64>().ok()?)))
        .ok_or(Error::Invalid)?;
    let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
    if submitted.is_empty() {
        statuses.put(
            key,
            CommitStatus::refusal(
                "Reports in",
                &submitted,
                &["choose an item or Not reported"],
            ),
        );
        return Ok(false);
    }
    if !pages.allowed(scope, MANAGE_SETTINGS).await? {
        statuses.put(key, CommitStatus::Error(DENIED.into()));
        return Ok(false);
    }
    let input = MappingInput {
        source,
        tax_code_id,
        box_key: (submitted != NOT_REPORTED).then_some(submitted.clone()),
        reason: None,
    };
    let result = match super::company(scope) {
        Some(c) => pages.sst.map_tax_code(scope, c, &input).await,
        None => Err(SstError::Unauthorized),
    };
    match result {
        Ok(_) => {
            statuses.put(key, CommitStatus::Saved);
            Ok(true)
        }
        Err(SstError::Invalid(errors)) => {
            let reasons: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
            statuses.put(
                key,
                CommitStatus::refusal("Reports in", &submitted, &reasons),
            );
            Ok(false)
        }
        Err(e) => {
            statuses.put(key, CommitStatus::Error(f::refusal(&e)));
            Ok(false)
        }
    }
}

/// An item's name without its number ("Sales tax: Taxable sales at 10%").
fn box_title(key: &str) -> String {
    TaxType::ALL
        .into_iter()
        .find_map(|tax_type| {
            Sst::boxes(tax_type)
                .iter()
                .find(|b| b.key == key)
                .map(|b| format!("{}: {}", f::tax_name(tax_type), b.label))
        })
        .unwrap_or_else(|| key.to_owned())
}

/// An item as its row shows it: the name, then its number muted, as the
/// combobox draws the choice.
fn box_html(key: &str) -> Result<Html> {
    let item = TaxType::ALL.into_iter().find_map(|tax_type| {
        Sst::boxes(tax_type)
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.item)
    });
    render(BoxName {
        name: box_title(key),
        item: item.map(|i| format!("item {i}")).unwrap_or_default(),
    })
}

#[derive(Template)]
#[template(
    source = r#"<span>{{ name }}</span>{% if !item.is_empty() %} <span class="italic text-ink-muted">{{ item }}</span>{% endif %}"#,
    ext = "html"
)]
struct BoxName {
    name: String,
    item: String,
}
