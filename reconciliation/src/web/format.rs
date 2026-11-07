//! Bilimbi `Finance.Reconciliation.Web.Format`: refusal copy.
use crate::ReconciliationError as E;
use bilibi_database::{Error, Result};

pub(super) fn told<T>(result: crate::Result<T>) -> Result<std::result::Result<T, String>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(E::Failed(Error::Forbidden | Error::Unauthorized)) => Ok(Err(super::DENIED.into())),
        Err(E::Failed(error)) => Err(error),
        Err(error) => Ok(Err(refusal(&error))),
    }
}
pub(super) fn refusal(error: &E) -> String {
    match error {
        E::Unauthorized => super::DENIED.into(),
        E::CompanyArchived => "The company is archived; nothing can be changed.".into(),
        E::CompanyNotFound => "The company could not be found.".into(),
        E::MappingNotSet => "Match the file's columns first: import it again and choose which column is which.".into(),
        E::BookNotOpen => "The company's ledger book is not open yet.".into(),
        E::BankAccountNotFound => "The bank account no longer exists.".into(),
        E::BankAccountInactive => "The bank account is inactive.".into(),
        E::StatementNotFound => "The statement no longer exists.".into(),
        E::InvalidFileName => "The file needs a name.".into(),
        E::NoLines => "The file holds no statement lines.".into(),
        E::FileTooLarge => "The file is larger than the import allows.".into(),
        E::NotUtf8 => "The file is not UTF-8 text.".into(),
        E::InvalidOption => "The request was malformed.".into(),
        E::Invalid(errors) => format!("Refused: {}.", errors.iter().map(|e|format!("{} {}", e.field.replace('_', " "), e.message)).collect::<Vec<_>>().join("; ")),
        E::DuplicateFile { statement_number } => format!("This file was already imported as statement {statement_number}."),
        E::DuplicateLines(lines) => {
            let shown = lines.iter().take(5).map(|l|format!("row {} (statement {}, line {})", l.row, l.statement_number, l.statement_line_number)).collect::<Vec<_>>().join(", ");
            let more = if lines.len() > 5 { format!(" and {} more", lines.len()-5) } else { String::new() };
            format!("Nothing was imported: {} lines are already on file: {shown}{more}.", lines.len())
        }
        E::InvalidLines(lines) => {
            let shown = lines.iter().take(5).map(|l|format!("row {}: {}", l.row, l.message)).collect::<Vec<_>>().join("; ");
            let more = if lines.len() > 5 {format!("; and {} more", lines.len()-5)} else {String::new()};
            format!("Nothing was imported: {shown}{more}.")
        }
        E::MissingHeaders(headers) => format!("The file has no column named {}; import it again and match its columns.", headers.iter().map(|h|format!("“{h}”")).collect::<Vec<_>>().join(", ")),
        E::TooManyLines(max) => format!("The file holds more than {max} lines; split it."),
        E::RunningBalanceMismatch(row) => format!("Row {row}'s running balance does not follow from the amounts; check the amount sign or the balance column."),
        E::UnterminatedQuote(row) => format!("Row {row} opens a quote it never closes."),
        E::StrayQuote(row) => format!("Row {row} has a quote in the middle of a value."),
        E::Attachment(bilibi_finance_attachments::AttachmentError::RetentionNotConfigured) => "Nothing was imported. Ask a server administrator to set Storage folder (artifacts.storage_root) and Document retention days (artifacts.retention_days) in Global settings at /system/settings.".into(),
        E::Attachment(missing @ (bilibi_finance_attachments::AttachmentError::StorageFolderMissing(_) | bilibi_finance_attachments::AttachmentError::StorageFolderUnsafe)) => format!("Nothing was imported. {missing}"),
        E::Attachment(bilibi_finance_attachments::AttachmentError::InvalidContentType) => "The file's type could not be recognised.".into(),
        other => other.to_string(),
    }
}
