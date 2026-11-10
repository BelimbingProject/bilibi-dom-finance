//! The one date format Finance pages show: "6 Nov 2026", as Bilibi's
//! `DESIGN.md` asks of every page. Base DateTime owns the format
//! (`DisplayContext::render_date`); a calendar date has no zone, so no page
//! needs a display context to show one.
//!
//! A date a person reads goes through [`display`] or [`display_text`]. A
//! date a form input, an address or a contract carries stays `YYYY-MM-DD`:
//! the shared date input reads that and shows what it understood.
use bilibi_datetime::{DisplayContext, Format, Precision};
use jiff::civil::Date;
use jiff::Timestamp;

/// A calendar date as pages show it: "6 Nov 2026". It takes the date or a
/// reference to it, because an Askama template hands its fields by reference.
pub fn display(date: impl std::borrow::Borrow<Date>) -> String {
    DisplayContext::anonymous().render_date(*date.borrow())
}

/// A contract's `YYYY-MM-DD` date as pages show it. A value that also
/// carries a time ("2026-11-06 09:13:02+00", "2026-11-06T09:13:02Z") shows
/// its date. Anything else (an empty value, a dash, words) is returned as it
/// came, so a caller never has to check first.
pub fn display_text(text: &str) -> String {
    let day = text
        .get(..10)
        .filter(|_| text.len() == 10 || matches!(text.as_bytes().get(10), Some(b'T' | b' ')));
    day.and_then(|day| day.parse::<Date>().ok())
        .map_or_else(|| text.to_owned(), display)
}

/// An instant on a printed document, which has no reader to take a zone
/// from: "6 Nov 2026, 09:13 UTC". A page shows an instant through its
/// display context (`instant` in each module's pages), never this.
pub fn display_utc(at: Timestamp) -> String {
    DisplayContext::anonymous()
        .render(at, Format::DateTime, Precision::Minute)
        .utc
}
