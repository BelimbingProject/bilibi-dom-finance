//! The History of an invoice or credit note: its issue or discard, then each
//! time it was emailed, to whom, with which PDF and how the send ended. It is
//! folded under one line until the reader asks (`DESIGN.md`: History is a
//! quiet disclosure).
use super::{
    super::{documents::reference, format as f, render},
    Document,
};
use bilibi_database::Result;
use bilibi_datetime::DisplayContext;
use bilibi_finance_ledger::People;
use bilibi_ui::{Card, Cell, FlexRow, Html};

impl Document {
    /// What happened to the document, oldest first, folded until the reader
    /// asks (`DESIGN.md`: History is a quiet disclosure): its issue or
    /// discard, then each time it was emailed, to whom, with which PDF and
    /// how the send ended.
    pub(super) fn history_card(
        &mut self,
        d: &crate::Document,
        display: &DisplayContext,
        people: &People,
    ) -> Result<Option<Html>> {
        let mut rows = Vec::new();
        let mut last = String::new();
        let mut event = |key: String,
                         at: &str,
                         what: String,
                         status: Html,
                         by: String|
         -> Result<()> {
            let mut row = FlexRow::new(key.clone());
            row.dom_id = Some(format!("receivables-invoice-history-{key}"));
            rows.push(
                row.slot(
                    "when",
                    f::time(display, &format!("receivables-invoice-history-at-{key}"), at)?,
                )
                .cell("what", Cell::text(&what))
                .slot("status", status)
                .cell("by", Cell::text(by)),
            );
            last = what;
            Ok(())
        };
        if let (true, Some(at)) = (d.status == "issued", d.issued_at.as_deref()) {
            event(
                "issued".into(),
                at,
                format!("Issued as {}", reference(d)),
                Html::EMPTY,
                people.who(d.issued_by_user_id, d.issued_by_impersonator_id),
            )?;
        }
        if let (true, Some(at)) = (d.status == "discarded", d.discarded_at.as_deref()) {
            event(
                "discarded".into(),
                at,
                "Discarded".into(),
                Html::EMPTY,
                people.who(d.discarded_by_user_id, None),
            )?;
        }
        for e in &d.emails {
            let pdf = match e.attachment_bytes {
                Some(bytes) => format!("{} ({})", e.attachment_name, file_size(bytes)),
                None => e.attachment_name.clone(),
            };
            let (kind, status) = email_status(&e.state, e.reason.as_deref());
            event(
                format!("email-{}", e.id),
                &e.requested_at,
                format!("Emailed to {} with {pdf}", e.to_email),
                render(bilibi_ui::Badge::status(kind, &status))?,
                people.who(Some(e.requested_by_user_id), None),
            )?;
        }
        if rows.is_empty() {
            return Ok(None);
        }
        self.history_cols.load(&rows, |row| row.key.clone());
        let summary = match rows.len() {
            1 => last,
            n => format!("{n} events · latest: {last}"),
        };
        let table = f::table_in(
            &self.history_cols,
            "history_grid",
            "History",
            rows,
            "",
            true,
        )?;
        Ok(Some(render(Card {
            id: "receivables-invoice-history-card",
            body: render(bilibi_ui::Fold {
                id: "receivables-invoice-history",
                title: "History",
                summary: &summary,
                open: false,
                body: table,
            })?,
            ..Card::default()
        })?))
    }
}

/// How one send ended, in words: the badge and its text.
fn email_status(state: &str, reason: Option<&str>) -> (bilibi_ui::BadgeKind, String) {
    use bilibi_ui::BadgeKind;
    match state {
        "queued" => (BadgeKind::Neutral, "Sending".into()),
        "sent" => (BadgeKind::Success, "Sent".into()),
        _ => (
            BadgeKind::Danger,
            format!(
                "Not sent: {}",
                match reason.unwrap_or("") {
                    "turned_off" => "invoice email was turned off",
                    "sender_access_ended" => "the sender may no longer send invoices",
                    "not_issued" => "the invoice was no longer issued",
                    "pdf_failed" => "the PDF could not be made",
                    "address_refused" | "message_refused" =>
                        "the address or the message was refused",
                    _ => "the mail server did not take it",
                }
            ),
        ),
    }
}

/// A file's size as a person reads it: "48 KB", "1.2 MB".
fn file_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", (bytes + 512) / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
