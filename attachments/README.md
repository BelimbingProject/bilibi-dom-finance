# Attachments

Module ID: `finance/attachments` (Bilimbi `Bilimbi.Finance.Attachments`).
Source documents kept with the Finance records they support: a supplier's
invoice scan behind a bill, a remittance behind a payment, a contract behind an
invoice, a voucher behind a manual journal, the bank's own file behind an
imported statement. The module is the services (below) and their pages: the
Source documents page, the download route and the panel a record's page
embeds. The manual-journal source follows the ledger's journals.

Depends on `finance/ledger` (the financial year a document's retention counts
from is the ledger's), Core Company, Base Artifacts, Base Queue and Base
Tenancy. Record owners (Receivables, Payables, Reconciliation, ...) depend on
this module, never the other way round.

## Public API

Resolve the `Attachments` service (`finance/attachments.attachments`) and call
it with a sealed `Scope` and an explicit company id. Results are plain records
(`Attachment`, `Retention`, `Capture`); the schema is private.

| Function | Capability | Does |
| --- | --- | --- |
| `get_retention` | `view` | The company's retention years (7 until it lengthens them) and the latest change |
| `set_retention` | `retention.manage` | Lengthens the years (1 to 100) with a reason; fewer or equal is `RetentionNotLengthened`, asked before the reason and range errors (listed reason first), as in Bilimbi |
| `list_retention_changes` | `view` | Every change, oldest first |
| `retain_until` | `view` + `finance.ledger.view` | What a document dated `date` would be kept until |
| `attach` | `create` | Stores the file as a records-grade artifact and links it to a record |
| `capture_file` / `attach_capture` | `create` | The same in two steps, for a record inserted in the caller's own transaction (a bank statement): capture first, link inside the transaction |
| `replace` | `create` | Captures a new file and ends the old attachment with `replaced`; only while the record is not final |
| `remove` | `delete` | Ends an attachment with `removed` and a reason; only while the record is not final |
| `extend_retention` | `retention.manage` + `view` | Moves one attachment's retain-until later, with a reason |
| `place_hold` / `release_hold` | `retention.manage` + `view` | A legal hold on the bytes, with a reason |
| `refresh_retention` | none (the source's own read) | Queues the retention refresh for a record that has just become final |
| `list_attachments`, `list_company_attachments`, `list_sources`, `get_attachment`, `read_attachment` | `view` + all of the source's view capabilities | The reads; a source the caller cannot read is left out of every list |
| `evidence_page` | `view` | One ID-ordered page of the `attachment`, `attachment_event` and `retention_change` datasets for the auditor's pack |

Capabilities are `finance.attachments.view`, `.create`, `.delete` and
`.retention.manage`, verbatim from Bilimbi, all granted to `tenant_owner`. The
module declares no verb. Every call authorizes the capability for the scope's
actor and the target company when it runs; a write holds the company's row
lock in an audited `MutationBoundary` transaction. Errors are
`AttachmentError` variants with Bilimbi's refusal names (`Unauthorized`,
`CompanyNotFound`, `CompanyArchived`, `DocumentFinal`, `AttachmentNotStanding`,
`RetentionNotExtended`, `AlreadyOnHold`, ...); refused values come back as
`Invalid(Vec<FieldError>)`.

## Source: the record an attachment supports

A Finance module that keeps source documents contributes one `SourceDefinition`
per kind of record to the `finance/attachments.sources` catalog, under the id
`<its module id>/<record kind>` (`finance/payables/bill`). Its `Source` says
how a record is named (`label`), every capability that reads it
(`view_capabilities`) and, for a record id, its `label`, `date` and `is_final`
on the caller's connection and with the caller's scope. Attachments authorizes
every declared capability before asking the source to authorize and read the
record. The source answers `SourceRefusal::NotFound` or `Unauthorized`. The
attachments facade stores the source id and the contributing module with each
attachment and never reads the owner's tables. A source with a record page
also answers `href(document_id)` (`/finance/payables/bills/{id}`), which
`Attachments::record_href` hands the Source documents list so each record is a
link; the page itself authorizes its reader again.

A record whose date can change while it is a draft calls
`refresh_retention` inside the transaction that finalises it.

## Retention

A file is stored once in Base Artifacts as a records-grade document, retained
until the company's retention years after the end of the financial year the
record's date falls in, at the end of that day in UTC. Base refuses to delete
the bytes before then, and a legal hold refuses it after. An attachment row is
never changed; its standing is its latest event (`replaced`, `removed`) and its
retention events (`retention_extended`, `hold_placed`, `hold_released`) say what
was done to the bytes. A replaced or removed attachment stays readable.

A change of one attachment's retention is made in Base Artifacts first, after a
short transaction under the company lock has checked it, and its event follows;
Base is the fact. `get_attachment` and `read_attachment` append any retention
event Base holds that the module does not, and a change whose event could not
be recorded is `RecordedInStorage`.

`TODO(finance/ledger periods)`: the financial year is the calendar year (31
December) until the ledger's fiscal years land; the facade then reads them, as
Bilimbi does, and a company with no fiscal year keeps 31 December.

### Retention refresh job

`refresh_retention` checks, with the caller's own scope, that the record is
readable and final, computes the retain-until its final date gives and queues
the Base Queue job `finance/attachments-retention-refresh` in the caller's
transaction. The job runs as the system principal
`finance-attachments-retention-refresh` (Bilimbi
`finance.attachments.retention_refresh`; Bilibi principal names are one
hyphenated segment), which holds `finance.attachments.retention.manage` only
where a tenant administrator granted it (`Grants::set_system`); without the
grant the job cancels as `actor_refused`. The person who finalised the record
needs no attachments grant. An archived company, an unmounted source or a
removed account cancels the job; any other failure retries.
`TODO(platform)`: Base Artifacts accepts only a user's scope, so the job reaches
it through the finaliser's live scope (proved again on every attempt) and Base's
history names that person; the module's own events name the principal.

## Pages

| Page | Route | Access | Mirrors |
| --- | --- | --- | --- |
| Source documents | `/finance/attachments` (menu "Source documents" under the Bookkeeping group, order 2) | `finance.attachments.view` | Bilimbi `Web.IndexLive` |
| Download | `/finance/attachments/{id}/download` | `finance.attachments.view`, asked again by `read_attachment` on every request | Bilimbi `Web.DownloadController` |
| Panel | embed `finance.attachments-panel` | signed in; every read and write asks the facade | Bilimbi `Web.Panel` |

**Source documents** shows the company's retention (the years in force and the
latest change; holders of `finance.attachments.retention.manage` get
Lengthen retention, a dialog whose save re-asks the capability, and the change
history folded under History; a recorded change reads the page again, and a
blank or non-numeric years is refused inside the dialog with the reason's
error first, as Bilimbi's changeset words it) and every attachment by number
in a FlexTable (column settings, compact rows) with the record it supports
(linked to its page when its source has one), file and size, retention date
and legal hold, capture time and standing; the SHA-256 waits in the column
picker. The kind-of-record and standing filters sit in the shared filter
toolbar (`filters[source]`, `filters[status]`; `source`, `status` in the URL;
standing by default) and keep to the address; an empty list says "No source
documents yet" or, with a filter set, "No source documents match". "Show
more" reads the next 200 after the last attachment number. The page's message stays until the
browser dismisses it (`clear-flash`), across form changes and filter patches. Attaching a
file happens on the record's own page.

**Panel.** A record's page mounts it as a panel (`LiveState::panels`):

```rust
PanelSpec {
    id: "payables-bill-attachments".into(),
    embed: bilibi_finance_attachments::PANEL_EMBED.into(), // "finance.attachments-panel"
    path: HashMap::from([
        ("panel".into(), "payables-bill-attachments".into()), // the spec's own id
        ("source".into(), "finance/payables/bill".into()),    // the source id
        ("document".into(), bill_id.to_string()),
    ]),
    query: HashMap::new(),
}
```

It lists the record's standing attachments with a download link each, a live
file input (one file, up to `MAX_FILE_BYTES`) to attach the next, and, while
the record is not final, Replace and Remove (with a reason) per attachment.
Unlike Bilimbi, which takes `final?` from the page, the panel asks the
record's `Source` whether it is final on every render, so a page cannot offer
replace or remove for a record that has just become final. Controls show by
the live capability, an absent control is not explained away by a silent
`if` (a reader who cannot attach and finds nothing sees "No source
documents"), and every action asks the facade again. The company is the
session's current company (Bilibi ADR 09), which grants nothing; the facade proves the reach.

## Tables

`finance_attachments` (one row per captured file), `finance_attachment_events`
and `finance_attachments_retention_changes`. All three are append-only
(PostgreSQL refuses UPDATE, DELETE and TRUNCATE), the database numbers a
company's attachments without gaps and stamps the capture time, refuses a
shorter retention, a second `replaced`/`removed` per attachment, a replacement
by another record's attachment and a hold without a reason.

`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.
