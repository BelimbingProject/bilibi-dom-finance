//! The Add supplier dialog (opened by its button, or by records dropped on the
//! page or carried by "Add to…"), built on one modal and one draft held by the
//! server.
use super::{Create, Vendors, FORM};
use crate::web::render;
use crate::MANAGE_SETTINGS;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MultiPartyChooser, PartyErrors};
use bilibi_tenancy::Scope;
use bilibi_ui::*;
use serde_json::Value;
use std::collections::HashMap;

impl Vendors {
    /// Opens the Add vendor dialog, as "Add vendor" does.
    pub(super) async fn open_create(&mut self, s: &Scope) -> Result<()> {
        self.reload(s).await?;
        let existing = self
            .all
            .iter()
            .map(|vendor| {
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("search", &vendor.code)
                    .finish();
                (
                    format!("{}:{}", vendor.record_kind, vendor.record_id),
                    format!(
                        "/finance/payables/vendors?{query}#payables-vendors-table-row-{}",
                        vendor.id
                    ),
                )
            })
            .collect();
        self.create = Some(Create {
            chooser: MultiPartyChooser::new("vendor-new", FORM).existing(
                "Already a supplier",
                "View supplier",
                existing,
            ),
            values: HashMap::new(),
            errors: PartyErrors::default(),
        });
        Ok(())
    }

    /// The records an "Add to…" link carried, opened once.
    pub(in crate::web) async fn open_dropped(&mut self, s: &Scope) -> Result<()> {
        match self.dropped.take() {
            Some(text) => self.receive_drop(s, &text).await,
            None => Ok(()),
        }
    }

    /// Companies and contacts dropped on the page, or carried by "Add to…":
    /// the Add vendor dialog opens with them chosen, as if each had been found
    /// in the chooser. The package is a request only: `Records::receive`
    /// re-reads every record under this person's scope, the chooser reads it
    /// again, and `vendor.create` decides when the dialog is confirmed.
    pub(super) async fn receive_drop(&mut self, s: &Scope, text: &str) -> Result<()> {
        if !self.pages.allowed(s, MANAGE_SETTINGS).await? {
            self.notice = Some((
                StatusKind::Error,
                "You do not have permission to manage suppliers.".into(),
            ));
            return Ok(());
        }
        let dropped = match self
            .pages
            .records
            .receive(s, crate::web::VENDOR_TARGET, text)
            .await?
        {
            Ok(dropped) => dropped,
            Err(refusal) => {
                self.notice = Some((StatusKind::Error, refusal.message()));
                return Ok(());
            }
        };
        if self.create.is_none() {
            self.open_create(s).await?;
        }
        let context = self.context();
        let mut refused = 0;
        if let Some(create) = self.create.as_mut() {
            for record in &dropped {
                match create
                    .chooser
                    .add(&context, s, &record.reference.kind, &record.id)
                    .await
                {
                    Ok(()) => {}
                    Err(Error::Invalid | Error::NotFound | Error::Forbidden) => refused += 1,
                    Err(error) => return Err(error),
                }
            }
            create.errors = PartyErrors::default();
        }
        if refused > 0 {
            self.notice = Some((
                StatusKind::Error,
                format!(
                    "{refused} of the dropped records could not be added: they are already suppliers or not available to you."
                ),
            ));
        }
        Ok(())
    }
    fn modal(&self, id: &str, title: &str, body: Html, event: &str, label: &str) -> Result<Html> {
        let mut actions = Self::button(&format!("{id}-cancel"), "cancel_vendor", "Cancel", "", "")?;
        actions.push(&render(Button {
            id: &format!("{id}-submit"),
            event,
            button_type: "button",
            variant: ButtonVariant::Primary,
            body: Html::text(label),
            ..Button::default()
        })?);
        render(Modal {
            id,
            title,
            cancel_event: "cancel_vendor",
            width: ModalWidth::Wide,
            description: &format!("For {}", self.company_name),
            role: "dialog",
            flash: None,
            body: render(Form {
                id: &format!("{id}-form"),
                event: "vendor_change",
                change: true,
                class: "mt-4",
                body,
                actions: render(Actions { body: actions })?,
            })?,
        })
    }
    pub(super) fn create_dialog(&self, c: &Create) -> Result<Html> {
        let mut labels = c.chooser.labels();
        labels.push((
            "payment_terms_days".into(),
            "vendor-payment_terms_days".into(),
            "Payment terms".into(),
        ));
        let mut body = render(c.errors.summary("vendor-errors", &labels))?;
        body.push(&c.chooser.render(&c.errors)?);
        body.push(&render(
            Field::in_form(FORM, "payment_terms_days")
                .id("vendor-payment_terms_days")
                .label("Payment terms (days)")
                .kind(InputKind::Number)
                .attr("min", "0")
                .attr("max", "3650")
                .value(
                    c.values
                        .get("payment_terms_days")
                        .cloned()
                        .unwrap_or_default(),
                )
                .errors(c.errors.under("payment_terms_days", "Payment terms")),
        )?);
        self.modal(
            "vendor-new-modal",
            "Add suppliers",
            body,
            "create_vendor",
            if c.chooser.count() == 1 {
                "Add supplier"
            } else {
                "Add suppliers"
            },
        )
    }
    pub(super) async fn take(&mut self, _s: &Scope, v: &Value) -> Result<()> {
        let form = params::form_values(v, FORM);
        if let Some(c) = &mut self.create {
            c.values.extend(form.clone());
            c.chooser.read(&form);
        }
        Ok(())
    }
}
#[derive(askama::Template)]
#[template(
    source = r#"<div class="flex w-full justify-end gap-2">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Actions {
    body: Html,
}
