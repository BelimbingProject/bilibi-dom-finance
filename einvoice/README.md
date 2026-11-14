# E-invoice

Module ID: `finance/einvoice` (Bilimbi `Bilimbi.Finance.Einvoice`). The fiscal
documents a company issues through a clearance authority. This version holds
the schema, the company's setup (settings, credentials, signing certificate
with its facts, and supplier profile), the `Authority` contract with the one
authority this release clears documents with (MyInvois) and its rules: the
draft a caller asks to issue and its validation, the UBL 2.1 JSON builder with
its byte-exact writer, the JSON XAdES signer, and the authority's code lists
with each company's overrides; and clearance: issuing a document, submitting
it through the MyInvois client on Base Integration with the submit and poll
jobs on Base Queue, refreshing its status and cancelling it; and the ledger
bridge: a validated document's one owned entry and a cancelled one's
reversal, retried from the reconciliation read, source links to the entry
Receivables or Payables already posted, the monthly consolidated document,
the printable PDF with its validation link's QR code, and the auditor's
evidence datasets. The documents, consolidation, reconciliation and setup pages are available. Other modules and
Domains never read its tables.

## Reviewed operations (H4)

H4 exposes `finance.einvoice.document.issue`, `.get` and `.list` through the shared HTTP API and MCP registry. The New e-invoice dialog (see Documents pages) calls the same contracts, with `finance.einvoice.document.submit` (queues an issued, released, refused or invalid document for the authority; `finance.einvoice.documents.submit`), `finance.einvoice.source.list` (the newest issued invoices and credit notes with no e-invoice that stands), `finance.einvoice.document.prefill` (the draft one of them starts from, see Starting from an invoice) and `finance.einvoice.code.list` (the company's classification, state, country and tax type codes and the authority's registration types); the three reads need `finance.einvoice.view`. Issue requires a Receivables or Payables source entry and an explicit whole-second `issued_at`. The fiscal draft is supplied by the caller; the source proves accounting ownership, currency and total, not line-by-line equivalence. Issuing does not submit to MyInvois or post another entry. Writes use a reviewed plan and join its audited transaction.

`finance.einvoice.party.prefill` reads a selected Core company's name, tax ID, registration number, email and phone into a draft party. Read the linked customer first and pass its Core company ID. It requires e-invoice view and Companies view; a restricted fact refuses the prefill. Registration type and billing address remain for the caller to supply; Core does not yet expose the billing address on this connection. Prefill does not validate or issue a document.

The discoverable [guide](guide.md) describes the wire contract. `.get` returns the full `DocumentView`; `.list` returns `{records, next_before}`, newest first, with an exclusive `before` cursor and `page_size` of 1–100 (default 50). Issue returns `{record, path: null}` and requires issue plus ledger-view authority, not e-invoice-view authority. It reviews the actual normalized party, line, source and settings snapshots; commit revalidates them on the plan transaction before recording that same prepared value. Earlier steps cannot silently change a later reviewed issue. PostgreSQL-assigned identities are not predicted or allocated during preview.

`TODO(platform)`: pinned Settings only accepts `GenericClient`. Preview resolves non-secret profile/environment through Settings on a separate checkout with a two-second acquisition bound; document reads use the same bound for the portal URL. All source/document/code reads remain on the handed client, and execution resolves settings on its own transaction. Exhaustion fails the request instead of retaining pool slots indefinitely. No settings tables are read directly, no secrets leave Settings, and no cache is introduced. Object-safe Settings resolution would remove this fallback. The guarantee is to persist the reviewed settings-derived snapshots, not to freeze settings until commit.

For the running proof, create a fresh disposable database and run these commands from the pinned Platform root (the bootstrap migrates and installs only fixture records). Use fresh private paths, and the same origin in the app and OAuth bootstrap:

```sh
export DATABASE_URL=postgres://bilibi:bilibi@127.0.0.1:5438/h4_proof
export APP_ORIGIN=http://127.0.0.1:3100
export BILIBI_AGENT_PROOF=disposable
export BILIBI_AGENT_CREDENTIALS=/tmp/h4-credentials.json
export BILIBI_EINVOICE_FIXTURE=/tmp/h4-fixture.json
cargo run -p bilibi-finance-walkthrough-fixture --bin agent-operations
cargo run -p bilibi-finance-walkthrough-fixture --bin einvoice-operations
cargo run -p bilibi-finance-walkthrough-fixture --bin einvoice-operations -- saturation
# Start bilibi-web run with the same DATABASE_URL and APP_ORIGIN, port 3100.
python3 apps/domains/finance/fixtures/walkthrough/einvoice-operations.py
```

The proof exercises HTTP and MCP, compares review snapshots with persisted documents, derives 47.60 + 2.86 = 50.46 independently, and checks inert previews (including sequences), ordinary/self-billed/payment sources, source reversal, cursor boundaries, malformed/overflowing inputs, missing grants, archived companies, drift, replay and atomic rollback after a duplicate code or late audit failure. The guarded controls use the normal settings, grant and company facades; SQL only observes or injects a disposable audit fault. No production seeds or unit tests are added.

## Public API

Resolve the `Einvoice` service (`finance/einvoice.einvoice`) from the registry
and call it with a sealed `Scope` and an explicit company id.

| Function | Capability | Does |
| --- | --- | --- |
| `Einvoice::code_lists` | none | The lists the authority publishes, sorted |
| `list_codes` | `finance.einvoice.view` | The company's active codes in one list, code to description: the shipped list with the company's overrides applied; `UnknownList` for a list the authority lacks |
| `override_code` | `finance.einvoice.settings.manage` | Renames a code, names one the shipped list lacks, or retires it, for that company only; audit action `finance.einvoice.code.overridden` |
| `setup` | `finance.einvoice.settings.manage` | What the company has configured, never a secret (see Settings), with the signing certificate's facts |
| `Einvoice::authority` | none | The `Authority` every company's documents are cleared with |
| `Draft::validate` | none | A draft's shape and the authority's rules, with the derived amounts (see Drafts) |
| `issue_document` | `.documents.issue` | Records a valid draft as an `issued` document; `Invalid`, `CodeNumberTaken`; audit action `finance.einvoice.document.issued` |
| `get_document` | `.view` | A `DocumentView`: the document, its lines, events, status, identifiers, validation link, cancellation deadline, attention flags and whether a submit job waits |
| `list_documents` | `.view` | `DocumentQuery` to `DocumentPage`: literal case-insensitive number or counterparty search, status/type/environment filters, newest first, clamped pages of 25, 50, 100 or 300; count and returned statuses share one snapshot |
| `submit_document` | `.documents.submit` | Queues the submit job as the caller (answers its ID); `NotSubmittable`, `SourceTaken`, `MissingCredentials`, `SigningKeyRequired`, `AlreadyQueued`; audit action `finance.einvoice.submission.queued` |
| `refresh_status` | `.documents.submit` | Reads the status at the authority and records what changed; `NotSubmitted`, `Authority(code)` |
| `cancel_document` | `.documents.cancel` | Cancels a valid or rejected document at the authority with a reason; `NotCancellable`, `CancellationPeriodOver`, `ActiveReferencingDocuments`, `InvalidReason`, `CancellationRefused`, `RateLimited`, `AuthorityUnavailable`, `Authority(code)` (the pre-cancel status read could not answer); an authority already holding it cancelled answers the updated document |
| `test_connection` | `.settings.manage` | Logs in afresh with the company's current credentials; audit action `finance.einvoice.connection.tested` |
| `run_submission`, `run_poll` | `.documents.submit` | The submit and poll jobs' bodies (see Clearance), callable directly for one attempt |
| `retry_posting`, `retry_reversal` | `.documents.submit` and `finance.ledger.entries.post` | Posts a valid document's entry, or reverses a cancelled one's, after a failure (see Ledger bridge); `NotPostable`, `NotReversible`, `LedgerFollowUpFailed(reason)`; audit action `finance.einvoice.posting.retried` |
| `reconciliation` | `.view` | `unposted` (valid without an entry) and `unreversed` (cancelled with an entry standing) documents, newest first, each with its entries and the last failure's reason |
| `consolidation_candidates` | `.view` | What a month's consolidated document would cover at `now`: the window, the eligible documents, those the authority's limits refuse with the reason, and the totals |
| `issue_consolidated_document` | `.documents.issue` | Records the month's consolidated document at `now` (see Consolidation); audit action `finance.einvoice.document.consolidated` |
| `generate_pdf` | `.view` | Renders the document's PDF on Base Artifacts once per state and records `pdf_generated`; audit action `finance.einvoice.pdf.generated` |
| `read_pdf` | `.view` | A stored PDF's bytes and download name, for the download route |
| `qr_code_svg` | none | The validation link's QR code as an inline SVG |
| `Einvoice::evidence_datasets`, `evidence_page` | `.view` | The auditor pack's frozen exports of this module's tables (see Evidence) |

Every call authorizes its capability for the scope's actor and the target
company at the moment it runs (Core Company's `authorize_work_target`): a missing
grant, or a company where the person has no desk, is
`EinvoiceError::Unauthorized`; a company the scope cannot see is
`CompanyNotFound`. A write then holds the company's row lock inside an audited
`MutationBoundary` transaction, so an archived company is `CompanyArchived`.
Refused values are `Invalid(Vec<FieldError>)` with Bilimbi's changeset words
(`must be 1 to 16 letters, digits, dots, or dashes`, `should be at most 500
character(s)`), each at its field's path (`code`, `buyer.address.state_code`,
`lines[2].tax_rate`).

## Documents pages

`/finance/einvoice/documents` uses shared FlexTable column settings and compact rows, with URL-backed search, status, document type, environment and pagination; Consolidation and Not in the books are standard header buttons, "+ E-invoice" opens New e-invoice for a person who may issue and submit documents and view the ledger, and the Environment column waits in the column picker (every row of a company is in one environment). Search uses the supplier for a self-billed document and the buyer otherwise. The reference fixes newest-first order rather than enabling sortable headers.

### Starting from an invoice

New e-invoice (`src/web/new_document.rs`) is the one door through which a person starts the e-invoice of an issued invoice or credit note. "+ E-invoice" opens it with a choice of documents (`source.list`); the invoice's own row and page open it for that invoice through `FiscalStart::href` (`?source=<module>&record=<record>`), which is a request, never proof: the dialog reads the record again under the person's own scope. It reads the draft through `document.prefill`: the buyer and lines as the invoice froze them, the gaps filled with what this company last sent to the same tax ID (or name) and for the same line description, because the invoice holds no authority codes. The person fills only what is still missing (registration type, state and country, each line's classification and the tax type of a taxed line). Submit e-invoice then calls `document.issue` and `document.submit` under the one live event `create_einvoice`, and opens the recorded document; when it was recorded but could not be queued, the flash says so and Submit to authority on its page tries again. A refusal stays beside its field; what has no field is said in the dialog's own alert. A credit note waits until the e-invoice of the invoice it corrects exists. A record whose e-invoice stands (not refused, invalid or cancelled) opens only a notice with Close.

The module never names or depends on the module that posted the record: it reads it through the ledger's seam (`bilibi_finance_ledger::FISCAL_SOURCES`, `src/fiscal.rs`) and contributes its own reader (`FISCAL_DOCUMENTS`) so the record's list and page show where its e-invoice stands and offer "Submit e-invoice" while none does. What the authority defines for this flow (the document type of a sale and of its correction, registration types, the home country code, the tax type of an untaxed line, the consolidated document's last day) is on `Authority`.

A status reads the same on every page: Not sent (issued or released), Sent, Valid, Refused, Invalid, Cancelled, Rejection requested, Consolidated.

`/finance/einvoice/documents/{id}` reads the frozen parties, lines and totals, identifiers, ledger entry and reversal (each a link to its journal), validation link and QR, event history (an event's entry links its journal), attention flags in a person's words ("Valid, but not in the books yet." with the reason the posting recorded and, when accounts are not chosen, a Choose accounts link to the setup page; otherwise a link to its row on the not-in-the-books page) and consolidated sources. Its header is Back, the document's own actions as icon buttons (refresh status, cancel, generate and download the PDF) and Submit to authority as the one primary verb. Lines show the quantity without the stored trailing zeros, prices with at least two places, and the unit code in the column picker. Lists use independent FlexTables; record links support workspace selection. Submit, refresh, cancellation and PDF generation check the current grant in both the live handler and facade. A read-only viewer may generate/download PDFs, as in Bilimbi. Revocation while a cancellation is pending refuses the action without losing the page.

Cancellation asks for a reason in the shared Modal, then asks for confirmation in the shared ConfirmDialog. The pinned Platform cannot place a reason input or a custom “Keep document” label inside ConfirmDialog, so this requires one extra confirmation click compared with Bilimbi; no Domain script or Platform change is added.

`/finance/einvoice/documents/{id}/pdf/{artifact_id}` returns an attachment through the Platform download contract. `read_pdf` reauthorizes the artifact's actual document and company on every request; the cosmetic document ID never grants access. The response is private/no-store and nosniff through the shared FileBody transport.

Run the NE5 proof against an empty disposable database with `DATABASE_URL=... BILIBI_SETTINGS_KEY=<64 hex characters> cargo run -p bilibi-finance-walkthrough-fixture --bin einvoice-ui`. It exercises literal wildcard search, page boundaries and combined filters, tenant isolation, real queue submit/poll/refresh, PDF bytes, status after PDF events, cancellation with Unicode reason and revoked grants. Only MyInvois is a local stand-in. To inspect the same records in a browser, add `serve` and set matching `APP_ORIGIN` and `BIND_ADDR`; this is fixture-only and installs no production data.

## Drafts

`Draft` is a document a caller asks to issue, as plain attributes, before it
is recorded: the document's type and code number, its issue instant (now when
none is given, never in the future), currency and exchange rate, the
counterparty (`buyer`, or `supplier` on a self-billed document, the company's
own profile taking the other side and never given), optional payment and
billing-period details, the original document a note corrects, and one to a
thousand lines. Amounts are derived, never taken: a line's subtotal is
quantity times unit price, its amount is the subtotal less its discount plus
its charge, and its tax is that amount times the rate, each rounded half up
to two places; the document's totals are the sums. An amount finer than its
column is refused, not rounded. The one exception is a consolidated
document's line, which carries its tax, the sum of its sources' tax.

`Draft::validate(now, &authority, &context)` checks the shape every authority
shares (Bilimbi's `Draft` changeset), then the authority's own rules
(`Authority::validate`): codes against the company's lists (`list_codes`, one
list per entry of `context.codes`), the fields its rules require, a company
profile (`context.supplier`, the setup's) missing what every document carries,
a note whose original (`context.original`) is not one the authority lets it
reference, and the consolidated document's own shape (`context.consolidated`).
It answers the `ValidDraft` with every line's derived amounts, or every
`FieldError` in Bilimbi's words. The walkthrough prints both for invoices,
notes, self-billed and consolidated drafts.

## Clearance

A recorded document is cleared with the authority by the submit and poll jobs
(`finance/einvoice-submit`, 100 attempts, and `finance/einvoice-poll`, 20
attempts), Base Queue workers queued with `enqueue_for`, so each runs as the
person who asked: the queue re-proves that person and their
`.documents.submit` grant on every attempt, every write of a job opens with
`Execution::begin` (which fences the job's lease), and the arguments are the
company and document IDs only. Each outcome is a new status event with the
authority's answer beside it, and each outcome the authority decides is also
an audit action; nothing edits a document. No transaction is held across a
call to the authority.

- **Submitting.** A submission row holding the exact bytes (signed when the
  company's format version is) commits before the HTTP call, so an attempt
  that dies mid-flight still leaves what it sent. The answer is then recorded
  under the company lock, only while the document is still submittable:
  `submitted` (with the submission UID and document UUID; the poll job is
  queued; action `finance.einvoice.submitted`), `refused` (action
  `finance.einvoice.refused`), `duplicate` (action
  `finance.einvoice.submission.duplicate`; the job stops with
  `duplicate_submission` for an operator), `rate_limited` (retry
  `rate_limited`) or `unavailable` (retry `authority_unavailable`). A document
  that cannot be sent records `unsendable` and stops the job with its reason
  (`missing_credentials`, `signing_key_required`, `source_taken`, ...).
- **Never resubmitting blindly.** After a duplicate, or an outage that may
  have hidden an accepted request, and before any retry that finds an earlier
  attempt without a final answer, the authority is asked for the document by
  its code number among what was sent from a minute before the oldest such
  attempt; what it holds is recorded as `submitted`. Only a clean not-found
  lets the bytes go out again; a search that cannot answer records
  `search_deferred` and retries without sending.
- **Overdue.** The queue retries after `min(3600, 2^attempt)` seconds, so a
  hundred attempts span about four days. Once the authority has been
  unavailable for its outage allowance (72 hours) since the document's first
  failed attempt, an `overdue` event and the action
  `finance.einvoice.submission.overdue` give the operator notice once; the job
  keeps retrying.
- **Polling.** The poll job reads the submission's progress up to ten times,
  four seconds apart, and retries `in_progress` while the authority is still
  processing. Then it reads the document's status and records `valid` (with
  the long ID that completes the validation link; action
  `finance.einvoice.validated`) or `invalid` (with the failed validation
  steps; action `finance.einvoice.invalid`). A 404 right after a submission is
  retried as `not_yet_available`; another refusal records `status_unreadable`.
- **Refreshing** records what changed: a buyer's rejection request becomes
  `rejected` (action `finance.einvoice.rejected`), a cancellation on the
  portal `cancelled`, and an unchanged status `status_checked`.
- **Cancelling** is guarded before the authority is asked: a valid or
  rejected document, inside the 72 hours after validation, with no note the
  authority holds (submitted, valid or rejected) referencing it, and a reason
  of 1 to 300 characters. Before each request, the authority's status is read
  using the cancellation permission; if that read cannot confirm a cancellable
  status, no cancellation request is sent and the read's own refusal is answered
  (`RateLimited`, `AuthorityUnavailable` or `Authority(code)`). This also checks
  an earlier request whose response was lost: when the authority already holds
  the document as cancelled, the status is recorded, the ledger reversal runs
  and `cancel_document` answers the updated document as a success with
  `already_cancelled` set (no other read sets it; before, it refused with
  `NotCancellable`); the page says "The tax authority had already cancelled
  this document. Its status is now updated." The authority's refusal is recorded
  as `cancellation_refused`. Each outcome is recorded only while the document,
  read again under the company lock, is still cancellable.

Once a `valid` (or `rejected`) status commits, the bridge posts the
document's entry; once `cancelled` commits, it reverses it (see Ledger
bridge). A document's view flags `Overdue`, `RejectionRequested`, `Unposted`,
`PostingFailed`, `Unreversed` and `ReversalFailed` for an operator.

## Ledger bridge

`posting.rs` (Bilimbi `Posting`). A document the authority validated posts
one balanced entry through the ledger's `post_entry_in`, as the person the
work runs for (the poll job's, the refresh's or the cancellation's), dated
its issue date in UTC (a consolidated document's, the last day of the month
it reports), described `E-invoice <code> (<type>)`, with its code number and
authority UUID as the `reference`. The accounts are the company's
`finance.einvoice.posting.*` codes in its own chart; the authority's
`posting_rule` says which three a type uses. The counter-account
(receivable, payable or settlement) takes the rule's side for the payable
amount; the net (revenue or expense) and tax accounts take the other side,
and a tax of zero posts no tax line. A sale debits receivable; a credit note
mirrors it; a refund note credits settlement; a self-billed purchase credits
payable and debits expense and purchase tax.

The entry's owner is `finance/einvoice` `document:<id>`, so the ledger
refuses a second entry for the document and a reversal by anyone else
(`OwnedBy`). Its ID is recorded on a `posted` event, and the reversal's on a
`posting_reversed` event, each in the transaction that posts it, under the
company lock; the database refuses a second of either. A refusal (book not
open, the book's currency differs, `Choose the ... account in the e-invoice settings.`,
`accounts not in the chart: ...`, `These are account groups, not accounts that take postings: ...`, a closed period, a missing ledger grant) is
recorded afterwards, in its own transaction, as `posting_failed` or
`reversal_failed` with the reason, and the document waits on
`reconciliation` until `retry_posting` or `retry_reversal` succeeds. Only a
database failure that may pass at once (a lost connection, lock contention, a
deadlock, a serialization failure) is tried a second time, and recorded as
one failure that says so. A document posts and reverses once: asking again
is unchanged. A clearance never fails because its follow-up did.

A document issued with a `source` never posts or reverses (see Source
links): its accounting and its correction are the source module's.

## Source links

`Draft.source` names the transaction the document is the tax document of,
when another Finance module already recognised it in the ledger: the
module's descriptor ID (`finance/receivables`, `finance/payables`), its
record (`document:17`) and the entry that record posted. `issue_document`
holds it to the ledger through `get_entry_on` (so the caller needs
`finance.ledger.view`): the entry exists, that module and record own it, it
is neither a reversal nor reversed, it is in the document's currency, and its
debits equal the document's total; and no other document that stands
(submitted, valid, rejected or consolidated, or with an attempt the authority
may hold) links the same source, which the database refuses too. Each
problem is a `source` field error in Bilimbi's words (`ledger entry 12 is
not owned by finance/receivables document:3`). A note correcting a linked
original needs its own `source`: the record that module corrected it with.
The view lists `source_links` and says `recognised_by_source`.

## Consolidation

`consolidation.rs` (Bilimbi `Consolidation`). The sources of a month's
consolidated document are the company's documents issued in the month (the
authority's calendar: MyInvois' UTC+8 month) in the current environment,
never submitted on their own, not held by another consolidated document, not
waiting in a submit job, and within the authority's limits (an invoice, in
ringgit, at most RM10,000). It is issued from the month's end for seven days
(`PeriodNotEnded`, `ConsolidationWindowClosed`) at the `now` the caller
passes (the database still stamps `recorded_at`), with a line per source and
tax whose amount and tax are that source's exact sums, to the general public
buyer with classification `004` and the month as its billing period. It
records which source each line carries and a `consolidated` event on each
source, which then cannot be submitted alone; a refused, invalid or
cancelled consolidated document releases them (`released`) and is never sent
again. Linked sources pass their links on, and linked and unlinked sources do
not mix (`MixedSourceLinks`). The view of a consolidated document lists its
`sources` with their line numbers; a source names its `consolidated_by`.

## Printable document

`pdf.rs` (Bilimbi `ArtifactOwner` and `QrCode`). `generate_pdf` renders the
document with Base Artifacts' deterministic serializer (parties, lines,
totals, status, identifiers and the validation link; characters outside
Latin-1 print as `?`) and stores it under owner `finance/einvoice`, kind
`document-pdf`, subject `document-<id>`. The adapter re-asks
`finance.einvoice.view` on every create, read and delete and never retains.
A `pdf_generated` event keeps the artifact and the last event it was
rendered from, so asking again before the document changes answers the same
artifact. Base Artifacts needs its storage root and retention period first.
`qr_code_svg` draws the validation link's QR code (black on white, no
script) for the document page.

## Evidence

`evidence.rs`: one dataset per table (`code_override`,
`consolidated_source`, `document`, `line`, `source_link`, `status_event`,
`submission`) with a frozen field list, ID-ordered pages of 1 to 500. The
submission's payload is exported as the text it is; credentials live in Base
Settings and are never exported.

### The MyInvois client

`myinvois/client.rs` calls the API only through Base Integration's `send`, as
the person the call acts for, so each exchange is recorded under them without
bodies or headers (system `myinvois`, owner `finance/einvoice` and the
company). Logging in (`POST /connect/token`, client credentials, scope
`InvoicingAPI`) yields a token that lives an hour; it is cached in process
memory per company, environment and client ID until two minutes before it
lapses, and a 401 drops it, so the authority's 12 logins a minute are never
spent per call. A token never reaches the database, a log or an exchange
record. The request envelopes are written by the module's ordered JSON
writer, like the documents. `myinvois/api.rs` turns each answer into the
`Authority` callbacks' meta-term outcomes (`SubmitOutcome`, `Progress`,
`Search`, `StatusRead`, `CancelOutcome`, `LoginOutcome`), keeping the
authority's answer intact. Nothing in the client retries.

## Capabilities

Five keys, verbatim from Bilimbi, under the `finance` domain, all granted to
`tenant_owner`: `finance.einvoice.view`, `.documents.issue`,
`.documents.submit`, `.documents.cancel` and `.settings.manage`. The module
declares the verbs `issue`, `submit` and `cancel` (Bilibi merges verbs across
modules). It contributes queue workers, pages, menu items and reviewed agent operations. It depends on `finance/ledger` (posting, reversal, source entries) and `base/artifacts` (the PDF), where Bilimbi has the same edges.

## Settings

Company-scoped Base Settings, edited on the Settings screen under the group
`E-invoice` and guarded by `finance.einvoice.settings.manage`. No value has a
default that names a country, a tax number or a credential.

- `finance.einvoice.environment` (`sandbox` by default, or `production`),
  `.document_version` (`1.1`, signed, or `1.0`, unsigned, for sandbox testing
  before the certificate arrives) and `.expiry_alert_days` (30, 1 to 365).
- Per environment: `finance.einvoice.<environment>.client_id`,
  `.client_secret` (encrypted) and `.secret_expires_on` (`YYYY-MM-DD`).
- `finance.einvoice.certificate_pem` and `.private_key_pem` (both encrypted),
  validated as one readable X.509 certificate with an RSA public key and one
  unencrypted RSA private key (`PRIVATE KEY` or `RSA PRIVATE KEY`; PKCS#12,
  encrypted and EC keys are refused).
- `finance.einvoice.supplier.*`: name (blank uses the company's), TIN,
  registration type and number, sales and tourism tax registrations, industry
  code, business activity, phone, email and the address with the authority's
  state code and the ISO country code. The authority fixes the shape of the
  TIN, phone, industry code, registration type, state code and country code.
- `finance.einvoice.posting.*`: the account codes of the company's own chart a
  validated document posts to (`receivable_account`, `revenue_account`,
  `tax_account`, `settlement_account`, and for self-billed purchases
  `payable_account`, `expense_account`, `purchase_tax_account`).
- Platform settings (operators, `base.settings.global.manage`):
  `finance.einvoice.<environment>.api_url` and `.portal_url`, an https URL
  without a path, defaulting to the authority's published hosts. The sandbox
  API URL may also be an http URL on this machine's loopback (`127.0.0.1`,
  `[::1]` or `localhost`, with a port), where a stand-in answers sandbox calls;
  production is https only.

Encrypted values use Base Settings' envelope (`BILIBI_SETTINGS_KEY`), are masked
on screen and revealed only through its password-checked reveal. `setup`
reports whether a client secret, certificate and private key are set, and the
certificate's facts (`CertificateSetup::Facts`: issuer and subject names as
the authority writes them, serial number, the tax ID and registration number
its subject carries, validity, days until expiry, and whether the stored
private key belongs to it), what expires within the alert window
(`expiring`), and whether the supplier profile's TIN is the certificate's. It
reads the PEMs inside the facade and returns no byte of them; `Unreadable`
means a stored certificate does not parse or the settings key is unavailable.

## Authority

`Authority` is everything one clearance authority defines that needs no
network: its document types, posting rules, format shapes, versions, the
consolidation window, the validation link, its rules over a draft
(`validate`) and the payload a recorded `Document` becomes (`payload`: the
format, version and exact bytes to submit, signed at the given time when the
version is signed, `SigningKeyRequired` without a key, `UnsupportedVersion`
for a version it lacks). MyInvois is the only implementation (Guideline v4.8
and the SDK as read on 12 October 2026). Its API callbacks (`login`, `submit`,
`submission_progress`, `find_submission`, `document_status`, `cancel`) take a
`Connection` (the outgoing edge, the person, the company, its credentials and
the environment's base URL) and answer in meta-terms. The consolidated
document's shape and refusal join the trait with consolidation.

Everything MyInvois defines sits in `src/myinvois.rs` and its submodules:

- `myinvois.rs` validates a draft (invoices and notes in ringgit; a note
  references the validated original it corrects, an invoice references
  nothing; a self-billed supplier carries its industry and activity and the
  foreign-supplier TIN only outside Malaysia; a consolidated invoice goes to
  the general public buyer with classification `004`; types `01` to `05` need
  a rate above zero, `06` and `E` zero, `E` an exemption reason) and maps a
  recorded document to the builder's input, with per-tax-type subtotals and a
  note's original as a `BillingReference`. It alone defines the authority's
  field shapes, which the settings validators and the buyer's checks both
  ask, so supplier and buyer follow one rule each.
- `myinvois::ubl` writes UBL 2.1 JSON in the element order of the SDK's
  samples from a plain `ubl::Document`. It maps and never computes, which is
  why `examples/sdk_samples.rs` rebuilds all four SDK version 1.1 samples
  (`examples/sdk/`) byte for byte, figures that do not add up included:

  ```sh
  cargo run -p bilibi-finance-einvoice --example sdk_samples
  ```

- `myinvois::json` is the order-preserving, minified writer and reader
  (`Json`): an object's key order and a number's written form survive a read
  and a write (`1500.0` stays `1500.0`). The authority hashes and signs exact
  bytes, so UBL JSON is never written through `serde_json`, whose maps sort
  keys.
- `myinvois::signer` follows the SDK's "Signing a JSON document" steps: the
  document without `UBLExtensions` and `Signature`, minified, is hashed and
  signed with RSA PKCS#1 v1.5 and SHA-256; the certificate's DER is hashed;
  the signed properties carry the UTC signing time and the issuer name and
  serial number; their minified `{"Target", "SignedProperties"}` object is
  hashed. `verify` checks a signed document against the certificate it
  embeds (both digests, the certificate digest, issuer and serial, and the
  RSA signature). The example verifies the SDK's four signed samples, rebuilds
  their signed properties from the SDK certificate, and signs and verifies
  with a throwaway certificate.
- `Certificate::parse` and `PrivateKey::parse` read the PEMs a company stores:
  the DER bytes, the issuer name as .NET renders it (attributes in reverse DER
  order, comma-joined, quoted when needed), which is what both SDK
  certificates show, the decimal serial number, the validity window, and the
  subject attributes (`organizationIdentifier`, `serialNumber`) that carry the
  company's tax and registration numbers.

Foreign-currency documents are refused for now: the SDK's sample writes tax
amounts in ringgit beside document-currency totals without saying how they
convert, so the rule is confirmed against the sandbox first. The builder
already writes `TaxExchangeRate`.

## Code lists

`code_lists/myinvois.json` holds the authority's nine lists (classification,
country, currency, document type, industry, payment means, state, tax type and
unit) as read from the SDK on 12 October 2026, with each list's source URL. They
are compiled into the crate as the reference data every company starts from, so
a refreshed list is a new file in a release, reviewed like code. A company's
override (`override_code`, stored in `finance_einvoice_code_overrides`) renames a
code, retires it or adds one the file lacks, for that company only.

## Database

Migrations `finance-einvoice-001-documents`, `-002-consolidation` and
`-003-source-links` build the end state of Bilimbi's schema (b-dom-finance
`4abfaf8`), 26 triggers and 11 functions on seven tables:

- `finance_einvoice_documents`, `_document_lines`, `_submissions`,
  `_status_events`, `_consolidated_sources` and `_source_links` are
  append-only: PostgreSQL refuses UPDATE, DELETE and TRUNCATE, so a
  cancellation or a rejection is a new status event.
- The database stamps `recorded_at`, numbers a document's status events 1, 2,
  3, ..., refuses a line added to a document recorded in an earlier
  transaction, checks a submission's SHA-256 against its bytes, and refuses a
  commit in which a document has no lines or its totals differ from its lines'
  (or, for a consolidated document, its sources').
- A document's `posted` and `posting_reversed` events each occur once.
- A source is covered by one consolidated document at a time and is never
  submitted on its own while covered; a source transaction has one standing
  fiscal document.
- Rows of one company reference each other through composite
  `(id, company_id)` foreign keys.
- `finance_einvoice_code_overrides` is the one editable table.

Prove it with `fixtures/walkthrough` on a disposable database (see the
repository README); its ledger bridge section (inside the clearance section)
opens Company 002's book, retries the postings its earlier validations could
not make, posts and reverses on validation and cancellation, consolidates
September 2026 (refused, released, consolidated again), generates and reads a
PDF, links Company 001's documents to Receivables' and Payables' entries, and
pages the evidence. Its e-invoice section prints the facade's answers, the
drafts accepted and refused, the payload a recorded document becomes (unsigned
and signed, verified, and tampered) and each database refusal. Its clearance
section serves a stand-in for the MyInvois API on the loopback for the
run, which checks each submission's hash and verifies its signature and
answers by the code number, then clears Company 002's documents through the
facade and the real submit and poll jobs: valid, refused, invalid, rate
limited, a duplicate found and one not found, a lost answer found again, the
72-hour overdue notice and recovery, an expired token, a rejection request, a
portal cancellation, and cancellations accepted and refused; then the stored
submissions, the database's refusals, the exchanges Base Integration recorded
(without bodies or the secret) and the audit actions.

## Consolidation, reconciliation and setup pages (NE6)

`/finance/einvoice/consolidation` selects an authority-calendar month, shows the days it covers and its deadline as calendar days in the authority's own calendar ("Covers 1–31 October 2026", "The deadline passed on 7 Nov 2026."), eligible documents, individual-invoice exceptions and totals, and issues only the candidate IDs the page displayed. Month changes are addressable and Back restores the month without discarding the number draft. An issuance refusal refreshes candidates and retains the number. The issue control requires `finance.einvoice.documents.issue`, an open window and nonempty candidates; the facade rechecks all three under its audited company write.

The pinned Platform keeps a focused input's dirty value even on history navigation. A keyed wrapper replaces only the month control on Back/Forward so its displayed month agrees with the restored totals; ordinary change events retain the native control and its focus. The form and input keep Bilimbi's IDs.

`/finance/einvoice/reconciliation` (page name: E-invoices not in the books) lists validated documents missing their ledger entry and cancelled documents whose entry stands. Retry controls (an icon button per row, named with the document) require both `finance.einvoice.documents.submit` and `finance.ledger.entries.post`, checked again on each event and in the facade. An entry links its journal; above the unposted documents, a note links the e-invoice settings where a refused posting's accounts are chosen. Every outcome reloads both tables. All four tables use shared FlexTable/ListColumns, with compact rows and column arrangement; Bilimbi has no sort, filters or paging on these pages, so none are added.

`/finance/einvoice/setup` requires `finance.einvoice.settings.manage`. It displays only the facade's secret-free setup: credential presence, certificate facts and days left, key/TIN matches, renewal alerts and supplier gaps. Editing remains in System settings: Edit e-invoice settings (and the missing certificate's or supplier profile's own button) opens `/system/settings` narrowed to this company and `finance/einvoice`. That page needs the platform operator's `base.settings.global.manage`, so a tenant administrator without it cannot edit these values yet (a Platform gap). Test connection reauthorizes and reports the authority's outcome; a login `Retry-After` is preserved in `LoginOutcome::RateLimited` without exposing HTTP bodies. Successful tests use an info flash, not a write-success flash.

The page adapters mirror Bilimbi `Einvoice.Web.{ConsolidationLive,ReconciliationLive,SetupLive}` at `4abfaf829321f8540581d833752a086b21b45ba9`. They own no CSS, JavaScript or SQL. The document links and successful issue redirect are NE5 destinations; NE6 does not register them.

### Running proof

Build from the pinned Platform with Finance mounted, then run `einvoice-pages` on an empty disposable PostgreSQL 18 database. It uses real PageService/LiveState, facade writes, Settings, ledger periods and Base Integration; only the authority is a local HTTP stand-in. No production seed or unit test is involved.

```sh
cargo build -p bilibi-finance-walkthrough-fixture --bin einvoice-pages
# DATABASE_URL must name a fresh disposable database; keep a generated
# BILIBI_SETTINGS_KEY in the process environment, never in source control.
target/debug/einvoice-pages
```

For the open-window proof, run the built binary with an external process clock at 3 October 2026 (Linux `libfaketime`; monotonic time remains real):

```sh
LD_PRELOAD=/usr/lib/x86_64-linux-gnu/faketime/libfaketime.so.1 \
FAKETIME='@2026-10-03 10:00:00' FAKETIME_DONT_FAKE_MONOTONIC=1 \
target/debug/einvoice-pages
```

The proof asserts later arrivals are excluded, competing consolidation is refused, drafts survive parameter changes, login feedback includes 429's 37-second delay, secrets never render, revoked grants cannot retry, and closed-period reversals fail then succeed after reopening. At the ordinary clock outside the window it instead checks the issue form is withheld. `--serve` continues into the composed app for browser inspection; `--serve-existing` reopens the prepared database without repeating proof writes, with the same settings encryption key and process clock. Set `APP_ORIGIN` to the exact browsed origin and `BIND_ADDR` to the listening address. Fixture accounts are `manager@example.test` and `viewer@example.test`, password `bilibi-dev`. Use September 2026 for the illustrated consolidation fixture; a deliberately missing revenue account leaves a real failed posting to retry.

`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.

Page feedback uses the shared `clear-flash` event: a dismissal clears only its named message kind. Detail-page successes can time out without an invalid event, and a delayed success dismissal preserves a newer refusal. The combined Finance browser walkthrough exercises this through the mounted detail page against the local authority fixture.
