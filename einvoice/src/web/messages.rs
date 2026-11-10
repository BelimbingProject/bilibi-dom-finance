//! Bilimbi `Web.DocumentLive`'s operation-specific flash wording.
use crate::{CancelRefusal, EinvoiceError as E};

pub(super) fn refusal(action: &str, error: E) -> String {
    match (action, &error) {
        ("submit", E::AlreadyQueued) => "A submission of this document is already queued.",
        ("submit", E::NotSubmittable) => "Only a recorded, released, refused, or invalid document can be submitted; a consolidated document reports the documents it covers, and one the authority refused or found invalid is never sent again: consolidate the month again.",
        ("submit", E::SourceTaken) => "Another document that stands (submitted, valid, rejected, or consolidated) is already the tax document of the same transaction.",
        ("submit", E::MissingCredentials) => "Set the client ID and secret for this environment in the e-invoice settings first.",
        ("submit", E::SigningKeyRequired) => "The chosen format version needs the certificate and matching private key in the e-invoice settings, or choose the unsigned version.",
        ("submit", E::Unauthorized) => "You do not have permission to submit documents.",
        ("submit", E::CompanyArchived) => "This company is archived; nothing can be submitted for it.",
        ("refresh", E::NotSubmitted) => "The authority has not given this document a UUID yet; submit it first.",
        ("refresh", E::Unauthorized) => "You do not have permission to read the status.",
        ("refresh", E::Authority("rate_limited")) | (_, E::RateLimited) => "The authority is limiting requests. Try again in a minute.",
        ("cancel", E::AuthorityUnavailable) => "The authority did not confirm cancellation. Refresh the status before trying again.",
        ("refresh", E::Authority("authority_unavailable")) | (_, E::AuthorityUnavailable) => "The authority could not be reached. Try again later.",
        ("refresh", E::Authority(_)) => "The authority did not answer with a status; the answer is recorded on the document.",
        ("cancel", E::Authority(_)) => "The authority did not answer with a status, so no cancellation was sent. The answer is recorded on the document.",
        ("cancel", E::NotCancellable) => "Only a valid document or one with a rejection request can be cancelled.",
        ("cancel", E::CancellationPeriodOver) => "The cancellation window has closed. Issue the appropriate credit, debit or refund note e-Invoice linked to the original.",
        ("cancel", E::ActiveReferencingDocuments) => "A note the authority holds references this document; cancel the note first.",
        ("cancel", E::InvalidReason) => "Give a reason of 1 to 300 characters.",
        ("cancel", E::Unauthorized) => "You do not have permission to cancel documents.",
        ("cancel", E::CancellationRefused { code: CancelRefusal::PeriodOver, .. }) => "The authority refused: the cancellation window has closed. Issue the appropriate credit, debit or refund note e-Invoice linked to the original.",
        ("cancel", E::CancellationRefused { code: CancelRefusal::IncorrectState, .. }) => "The authority refused: the document is not in a state it can cancel. Refresh the status.",
        ("cancel", E::CancellationRefused { code: CancelRefusal::ActiveReferencingDocuments, .. }) => "The authority refused: an active document references this one.",
        ("cancel", E::CancellationRefused { reason, .. }) => return reason.as_ref().map_or("The authority refused the cancellation.".into(), |r| format!("The authority refused the cancellation: {r}")),
        ("generate_pdf", E::Unauthorized) => "You do not have permission to generate the PDF.",
        ("generate_pdf", E::Artifacts("policy")) => "File storage is not set up. Ask your administrator to set the storage folder in System settings.",
        ("generate_pdf", E::Artifacts("unsafe_root")) => "The storage folder cannot be used. Ask your administrator to check it in System settings.",
        ("generate_pdf", E::Artifacts("missing_root")) => "The storage folder cannot be found. Ask your administrator to check it in System settings.",
        (_, E::CompanyArchived) => "The company is archived.",
        // Never expose transport, storage or database internals to the page.
        _ => return format!("{} ({error}).", match action {
            "submit" => "The submission could not be queued", "refresh" => "The status could not be refreshed",
            "cancel" => "The document could not be cancelled", _ => "The PDF could not be generated",
        }),
    }.into()
}

// A success timeout can arrive after a newer refusal was shown.
pub(super) fn clear_flash(
    flash: &mut Option<(bilibi_ui::StatusKind, String)>,
    value: &serde_json::Value,
) {
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| value["key"].as_str() == Some(kind.name()))
    {
        *flash = None;
    }
}
