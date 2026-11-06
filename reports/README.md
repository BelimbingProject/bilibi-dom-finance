# Reports

Module ID: `finance/reports`, crate `bilibi-finance-reports`, ported from
Bilimbi's `Bilimbi.Finance.Reports`. It reads a company's management reports
from the general ledger and retains source-period conversion schedules. The
reports cover profit and loss, the balance sheet, cash position, headline
figures against the previous period, and the entries behind any account's
figure. It stores immutable issued snapshots; every report figure comes through
`finance/ledger`'s facade, and it reads none of the ledger's tables.

Reports (management reports) and account activity are available at `/finance/reports`
and `/finance/reports/accounts/{account_id}` (NT2). Bookkeeping controls and Approval
exceptions are also available (NT3), alongside auditor-pack generation and downloads (NT4).

## Pages

**Finance setup** (`/finance/setup`, menu Finance, Bookkeeping, first) shows what the current company has set up and what remains, with a link to the page for each area. It is described under [Finance setup](#finance-setup) below; the two report routes follow.

Both routes require `finance.ledger.view` and read the session's current company through the facade. Reports has four addressable views (`view=overview`, `profit-and-loss`, `balance-sheet`, `cash-position`), the shared period filter (the latest month with entries by default, see First report and layout), and previous-period comparison (`compare=none` disables it except on Overview). Custom dates stay a draft until Apply range succeeds. The display policy and browser timezone decide the preset's calendar.

`Freeze these figures` puts the displayed positive entry number in `through`; `Show latest figures` removes it. This is not an issued snapshot. Every account-amount link carries its column's dates and the displayed cutoff, including `closing=excluded` for profit and loss. Account entries show whole-window debit and credit totals and 100 dated entries at a time; `Next entries` retains the cutoff and the `(after_date, after_id)` cursor. Activity does not subscribe to postings. Account entries keep that open from/to window: a figure's drill-down is one column's dates, not one of the shared period presets.

Reports subscribe before the mounted read. Commit notices and missed-notice refreshes invalidate unpinned figures; bursts coalesce, and pinned figures ignore them. Custom-range drafts and table arrangement survive a posting refresh. The relay task stops with the page. If the relay cannot start, a visible warning explains periodic-refresh fallback. As in Bilimbi, zero is not a positive URL cutoff, so an empty ledger cannot be pinned.

All tables use shared FlexTable and built-in ListColumns on ListState. There is no new sort/filter/page-size control: Bilimbi fixes statement order and pages activity by cursor. Section headings, subtotals and footers remain ordered rows with the reference IDs; FlexTable's uniform body replaces Bilimbi's grouped statement table. A shared SectionHeading places the statement label above the card, clear of the customization notch. The shared "Pin entry cutoff" button (not the sidebar pin beside the title) raises a read-only page event because the Platform button has no patch-link variant. No domain CSS or JavaScript is added. When figures are available, Generate auditor pack produces an export for the selected period end, followed by Download auditor pack.

## Management reports, not statutory statements

Profit and loss, the balance sheet and the cash position are management reports
for the company's own use, not statutory financial statements: there is no cash
flow statement, no statement of changes in equity or other comprehensive
income, no notes, no consolidation and no currency translation, and no layout a
standard requires (MFRS, IFRS or another) is produced here. Every page and
export that shows them must say so.

## Public API

`Reports` is the registry service `finance/reports.reports`. Every call takes
the sealed `Scope` it was handed and an explicit company id.

| Function | Does |
| --- | --- |
| `profit_and_loss` | Income and expense for one to four periods, by section, and `net_income` |
| `balance_sheet` | Assets, liabilities and equity as of one to four dates, with `earnings`, `total_assets`, `total_liabilities_and_equity` |
| `cash_position` | The balances of the accounts the cash sections cover, as of one to four dates |
| `overview` | Headline figures for a period and `previous_period` of it |
| `account_activity` | One account's debits and credits over a window, and the entries behind them, paged |
| `subscribe` | The ledger's posting notices (`Ledger::subscribe_postings`) |
| `issue_report` | An immutable issued profit and loss, balance sheet, cash position or trial balance |
| `conversion_report` | A conversion preview with its opening sources, target balances, reconciliations and explicit limits on one database snapshot |
| `get_issued_report` | The stored copy, never recomputed |
| `evidence_datasets`, `evidence_page` | Issued reports, immutable conversion sign-offs and retained source-period schedules for the auditor's pack |
| `previous_period` | The comparison period: as many whole months before, or as many days |

Every amount is a list with one value per column, in the order the periods or
dates were given; the first column is the reporting period and the rest its
comparatives. Every management report takes `through_entry_number`. A conversion
report captures its own ledger-entry boundary and each subledger's opening-batch and settlement boundaries.

## Authorization and company scope

Every function authorizes `finance.ledger.view` for the scope's actor and the
target company when it runs, through Core Company's `authorize_work_target`, and the
ledger calls it makes authorize again: a missing grant or a sibling company
without a desk is `Unauthorized`, and a company the scope cannot see
is `CompanyNotFound`. Issuing asks for `finance.reports.issue` as well, and
runs in an audited mutation transaction holding the company's lock, so an
archived company is `CompanyArchived`. `finance.reports.sign-off` records an
accountant's attestation against an issued conversion report.
`finance.reports.reports-export` generates and downloads the auditor pack, with
ledger view required. All nine source views enable the full evidence pack;
otherwise it produces a clearly labelled ledger-and-reports pack. All three
Reports capabilities are granted to `tenant_owner`.

## Figures that reproduce

A report names `through_entry_number`, the last ledger entry it counts, and
every column is read at that same number. Posted entries never change and are
numbered in posting order, so passing the number back freezes the postings
counted, including later back-dated entries. It does **not** freeze the chart,
names, sections or settings: removing an account from a cash section changes
even a pinned live cash total. `account_activity` given the report's window,
number and `exclude_closing` lists exactly the entries the figure counted, and
its debits and credits equal the figure.

Each posting account reports its own postings and a group is never a row, so
nothing is counted twice.

## Grouping and signs

The five account types are the one fixed classification. A profit and loss
lists income then expenses; a balance sheet lists assets, liabilities, then
equity. Within a type, accounts are grouped by the company's sections in their
configured order, and accounts no section covers follow under no heading
(`name: None`). A statement lists every account with a posting in any of its
columns.

Amounts are on the type's natural side: assets and expenses as debits less
credits, liabilities, equity and income as credits less debits. A profit and
loss, the overview's income and expense, and their drill-downs leave out the
ledger's year-end closing entries and their reversals (`exclude_closing`), so a
closed year still shows its activity. The balance sheet counts them: a closed
year's result sits in the retained-earnings account, and `earnings` is the
income less expense not yet closed into it. That line is part of equity, so
`total_assets` always equals `total_liabilities_and_equity`.

The cash position lists every account its sections cover, whatever its type and
whether or not it has postings, as debits less credits, so an overdrawn bank
account kept as a liability counts against the total. With no cash section
configured, the accounts are the Bank reconciliation accounts (see First report
and layout).

## Configuration

Section headings and account choices are a company's own data, never code. They
are the `finance.reports.sections` setting (Base Settings, tenant or company
scope, editor group Reports, edited by holders of
`finance.ledger.accounts.manage` on the Report sections page): an ordered JSON array of sections, each with
only these keys.

```json
[
  {"report": "profit_and_loss", "name": "<heading>", "accounts": ["<code>", "<code>"]},
  {"report": "balance_sheet", "name": "<heading>", "accounts": ["<code>"]},
  {"report": "cash_position", "name": "<heading>", "accounts": ["<code>"]}
]
```

A listed account or account group brings everything under it in the chart, unless a row under it is
listed itself: the nearest listed account decides, and within one report the
first section that lists a code keeps it. Headings (at most 100 characters) and
codes (at most 50) are trimmed. The setting's validator refuses any other shape
on save; a stored value that is not valid (written before validation, or
adopted) groups nothing rather than grouping wrongly. A report names, in
`unmatched_codes`, any listed code the chart does not have.

## Comparative periods

`previous_period` gives a period of whole calendar months the same number of
whole months before it (a quarter compares with the previous quarter, a year
with the previous year) and any other period the same number of days before
it.

## Live updates

`subscribe` relays the ledger's posting notices, which PostgreSQL delivers only
when the posting transaction commits. A notice carries no figures; the reader
reads again through this facade, and on `PostingEvent::Refresh` (notices may
have been missed) reads again as well.

## Issued reports

`issue_report(scope, company, &Issue, through_entry_number)` stores the figures
(the live report's own JSON: string keys, exact decimal strings, ISO dates),
the entry cutoff, the chart they were arranged by, the configured layout
(`{"configured_sections": ...}`), each covered account's section with the
effective sections and unmatched codes (`membership`, `{}` for a trial
balance), the implementation version, the issuer and any impersonator, and a
time the database stamps. The issuing transaction holds the company's lock,
which every ledger write takes, so the figures, chart and book it reads through
the ledger's facade describe one state of the ledger; the layout is read once
and every grouping comes from that one read. `get_issued_report` returns that
stored copy. PostgreSQL refuses update, delete and truncate of
`finance_reports_issued`. An entry pin alone reproduces postings, not a
complete report: issue one for a durable copy.

## Performance

Each column is one aggregate query in the ledger: PostgreSQL sums the lines per
account and returns one row per account. No report loads lines into memory;
the drill-down pages entries 100 at a time (1 to 500). A report reads at most
four columns, and the overview four windows.

## Proof

`fixtures/walkthrough` (the container README's Walkthrough fixture) configures
Company 001's sections, reads every report with comparatives, follows a figure
to its entries, reproduces a report from its pin after a later post its
subscription hears, issues and reads back snapshots, and shows the facade's
and the database's refusals.

From the Platform root, with `DATABASE_URL` naming a disposable PostgreSQL 18 database, `cargo run -p bilibi-finance-walkthrough-fixture --bin reports-ui` exercises the mounted page contracts and prepares the browser fixture. It proves subscribe-before-read, a stale-click pin, pinned and unpinned notices, range draft/error retention, table compactness, four views, comparison off, unopened and forbidden ledgers, and account cursors at 99/100/101 entries with stable whole-window totals. Sign in as `manager@example.test` with the walkthrough password `bilibi-dev`, then open `/finance/reports`; running the same command with `-- post` commits a real later posting to demonstrate live refresh and pinned figures. These are executable fixture proofs, not unit tests or production seeds.

## Finance setup

`/finance/setup` is one page for the current company: the book, the chart of accounts and fiscal years ("Start here", the three a company needs before it can post), then the areas a company adds as it needs them (bookkeeping and approval controls, opening balances for a company converting from another system, receivables, payables, sales and service tax, e-invoicing, bank reconciliation, payroll postings, fixed assets, accountant access). It is the page of the contract `finance.reports.setup.get` (`Reports::setup_overview`), which an agent calls with a `company_id`; the page writes nothing.

Reports owns it because it is the Finance module that already depends on every other, so no module learns of another. Each area asks its own module's facade and reports only what that module can tell: a setup record exists or it does not. The states are `set_up`, `not_set_up`, `incomplete` (e-invoicing lacks a credential; the chart has no retained-earnings account, which closing a fiscal year needs; fiscal years exist but none has an open period), `no_access` (the person works in the company but may not read that module's setup, so the page shows no status and no link), `unavailable` (the module could not answer) and `not_tracked` (accountant access has no measured completion and is a link only). The count of required steps ("2 of 3") is shown only when all three could be read. Book currency and decimal places are chosen once. The Book page proposes Core Currency's company base currency, falling back to the tenant base currency, names its source and requires an explicit reviewed choice; changing a default leaves existing books unchanged.

Opening balances is optional and does not contribute to the required-step total. The ledger's `opening_progress` reads the latest retained opening submission. No submission is `not_set_up`; a waiting, rejected, withdrawn or reversed journal is `incomplete`, with its actual status in the summary; an approved, unreversed opening is `set_up`. This measures the opening journal only. Open invoices, bills, historical transactions and the accountant's conversion reconciliation remain outside that status. The same facts are available through `finance.ledger.opening_balances.get` and the setup contract.

A different company is set up by choosing it in the top bar; **Company books** (ledger) lists every company the person works in. The page's pin id is `nav-finance-reports-setup`.

## Bookkeeping controls and approval exceptions pages

NT3 provides `/finance/reports/controls` and `/finance/reports/approval-exceptions`, ported from Bilimbi's Bookkeeping and Approval exceptions pages. The Manual journals tab reads the central exception contract and links to Workflow’s journal rules, eligible decisions and independent reviews. The Other Finance approvals tab retains the ledger facade for separation of duties and exceptions while those consumers still use `check_approval`. Controlled bookkeeping remains on the ledger facade. Late-recording review lives on the journal record, reached through Journals’ recording and review filters, and calls `finance.ledger.entry.review_late`. Fiscal years opens only from the Fiscal years list, through `finance.ledger.fiscal_year.open`. Every write checks its capability when the event arrives and again inside the ledger's audited transaction.

The pages are framed routes protected by `finance.ledger.view` and menu items in the Bookkeeping group under Finance. Bookkeeping controls keeps posting controls and their history, with links to Fiscal years and Journals. Approval exceptions defaults to `area=journals`, an ID cursor over the shared journal exceptions. Both exception areas default to `period=this_month`, resolve custom `from`/`to` dates against the saved display clock, and keep their review filter. `area=other` pages by exception number. FlexTable supplies column customization and compact rows for exceptions and change histories.

Turning off controlled bookkeeping requires a reason and records an approval exception atomically through `Ledger::set_controlled_bookkeeping`. Separation changes and approval-exception reviews use their ledger capabilities; reviews retain the ledger's independent-person and impersonation checks. Invalid reasons keep their dialog open; other refusals close it and show the reference wording. As in the reference, a control the reader may not use is not rendered (no extra explanation). Open dialogs retain their intended separation state, so another session's change cannot invert a later submission.

Run the page-service proof on a fresh disposable database with `DATABASE_URL=… cargo run -p bilibi-finance-walkthrough-fixture --bin report-controls`. It migrates and installs only walkthrough records, then exercises the actual page service and ledger: links to Fiscal years and Journals, Unicode reasons, controlled overrides, exception date/review filters, independent exception reviews, stale grants, archived-company refusals and PostgreSQL’s append-only exception-review guards. `-- --empty` installs the empty walkthrough state for comparison with the R1 reference captures before opening any book.

The page's company is the session's current company (`Scope::current_company()`), as on the other Finance pages. A scope with no current company gets an explicit missing-company state. The page proof creates and binds the independent reviewer's Company 001 affiliation through the Core Affiliation and User facades; its administrator stays unaffiliated so this missing-context case remains covered.

Archived-company history remains readable. Until Base Settings accepts an archived company for display reads, these pages fall back to the user's tenant display clock, as the report facade already does for the unavailable company layout. The ledger still refuses every archived-company write; this display fallback only keeps that refusal visible on the live page.

## Auditor pack

`export_auditor_pack(scope, company, as_of)` always collects the complete ledger, trial balance and issued management reports. Conversion reports and their sign-offs require all source views and are explicitly excluded from a ledger-only pack. When the person holds every Finance source view, it also collects every dataset published by the nine Finance siblings, Reports' source-period dataset and the aging summaries. Otherwise it excludes all subledger and source-period datasets, attachment indexes and authority payloads, and labels the result as a ledger-and-reports pack. The manifest explicitly names the excluded source-period records and conversion evidence; the page's success message says Payroll and other source records are excluded while retaining payroll journal lines. It never removes ledger lines or makes an incomplete trial balance appear complete. Collection goes through the authorized facades on one PostgreSQL repeatable-read transaction (write-enabled only because Base Authz records its access decisions on the caller's connection). The initial ledger total supplies the one `through_entry_number` for the general ledger, trial balance and aged receivables/payables. The database snapshot also freezes mutable account labels, document states, settings histories and attachment status across all evidence pages; an entry pin alone cannot do this. `as_of` dates the summaries only: evidence includes all company records visible at the snapshot, including drafts and reversals. A page's positive entry cutoff does not truncate the auditor pack.

The ZIP includes `manifest.json`, one `<module>-<dataset>.csv` per sibling dataset, `reports-source-period.csv`, `issued-reports.csv`, `reports-conversion_signoff.csv` in a full pack, `general-ledger.csv`, JSON and CSV trial balance and aging summaries, `source-file-index.csv`, and exact `authority-payload-<submission-id>.json` files. The attachment index is `attachments-attachment.csv`: it carries filenames, hashes, retention and holds, with `import_mapping_ref` linking statement attachments to `reconciliation-statement.csv#<id>`. Original supporting documents and bank-statement bytes stay in Attachments. Every authority payload's SHA-256 must match before inclusion. General-ledger debit and credit sums must equal the ledger facade's SQL aggregates, or no artifact is published.

Manifest version 11 includes `coverage`, `excluded_sources` and `coverage_notice`. Full coverage also records the aging `balance_pins`: the shared entry boundary and each subledger’s opening-batch and settlement boundaries. Aging CSV rows include credits separately, their balance side and saved control, and all three pins. It records the tenant, company, currency, minor units, actor and impersonator, UTC capture time, PostgreSQL snapshot, entry cutoff, filters, exact-decimal encoding, and each file's SHA-256 and row count (byte size for payloads). CSV uses UTF-8, CRLF and quoted cells; formula-like text is prefixed with an apostrophe even after whitespace or controls. Known numeric fields retain unrounded decimal strings, including negative amounts. Empty datasets still have their frozen headers. Files are collected in a random 0700 temporary directory with 0600 files, paged 500 rows at a time, hashed incrementally and removed on success, failure or cancellation. The compressed archive is checked against `artifacts.max_bytes` before loading into memory.

Publication uses Base Artifacts' ordinary retained download policy (`artifacts.storage_root`, `artifacts.retention_days`, `artifacts.max_bytes`). Its private owner validates an immutable UUID reference of kind `auditor-pack` (all source views) or `ledger-pack` (ledger view), and rechecks the export capability, live company and that stored coverage's view grants on create and every read. An existing full pack cannot be downloaded by someone with only ledger access or after any source permission is revoked; generating a new restricted pack never changes that old artifact's protection. Maintenance requires `finance.reports.issue`. `read_auditor_pack(scope, company, artifact_id)` returns only bytes and a trusted filename; `/finance/reports/auditor-packs/{artifact_id}` resolves the session's current company and calls this facade on every click. Base Artifacts reauthenticates the actor, verifies expiry, byte size and hash, and commits read audit evidence before delivering bytes. Shared download delivery sets attachment disposition, private/no-store, nosniff and sandbox headers. Revoking ledger view or export refuses either kind of pack. Revoking another source view refuses an existing full pack; a newly generated pack then contains only ledger and report evidence.

The Generate event reauthorizes rather than trusting a mounted flag. Success and recoverable storage/refusal flashes use Bilimbi's wording. Period changes clear the previous download link. Artifact settings belong to Operator Settings; the reports page adds no setup fields.

Run `DATABASE_URL=… AUDITOR_PACK_PROOF_DIR=/absolute/private/task-directory cargo run -p bilibi-finance-walkthrough-fixture --bin auditor-pack` on a fresh disposable PostgreSQL 18 database. It proves a 500-row page boundary under a second session's backdated posting, checks archive membership and checksums, exact totals, source hashes, formula protection, issued evidence, missing/cross-company access, size refusal, the download adapter and revocation of every source grant against an already-mounted page. Its final section assigns the production External accountant role to two people, proves adjusting journal submission and independent approval, preserves every line of a mixed payroll/sales entry, inspects the restricted ZIP and proves immediate revocation against retained scopes and links. Append `-- serve` to keep the real app at `http://127.0.0.1:4324` for browser proof; use the walkthrough manager account.

Report and control-page feedback clears on the shared `clear-flash` event for its current message kind. A dismissed auditor-pack success stays dismissed across tab changes and refresh; an older success timeout cannot clear a newer refusal.

## First report and layout

Reports opens on the latest calendar month with entries. An explicit period in the address is preserved; a book with no entries opens on this month. The initial month is read through `finance.ledger.book.get` (`latest_entry_date`).

When no cash-position section is configured, cash uses exactly the ledger accounts mapped in Bank reconciliation, including an inactive bank account that still holds money. Descendant accounts are not included by that default. An explicit cash section, including one with no accounts, takes precedence. `Reconciliation::cash_account_ids` requires ledger view and returns only account identifiers, without bank details.

Use **Report sections** (`/finance/reports/sections`) to arrange report headings and choose accounts. This form replaces the generic JSON editor. Its `finance.reports.sections.get` and `finance.reports.sections.set` contracts serve pages and agents. Saving requires `finance.ledger.accounts.manage`; the shared transaction retains the layout review fence, audits the setting and refuses an archived company.

## Source-period schedules

`finance.reports.source_period.import` retains one source schedule as immutable evidence after the opening journal is approved. It creates no ledger entry, tax return, filing, payroll run, remittance or allocation. `finance.reports.source_period.list` reads a kind in ID order, 100 records per page, using `after` for the next page. Open **Bookkeeping → Source-period schedules** to retain one row per schedule in the current company, review it and attach its source report. MCP uses the same import contract and accepts up to 500 rows at once.

Submit `company_id`, `kind` (`comparative`, `movement`, `sst` or `payroll`), a unique `source_reference` within that kind, `source_system`, the exact book `currency_code`, an IANA `accounting_timezone`, `period_start`, `period_end` and 1–500 `rows`. All periods end before the approved conversion date. A movement covers one month in the conversion fiscal year; the conversion month ends at cut-off when conversion falls mid-month. Each row has a unique `source_id` and a non-negative decimal-string `amount` at book precision. Comparative and movement rows also name a chart `account_id` and `side` (`debit` or `credit`). SST and payroll rows name the `authority`, filing or payment `reference` and an `event_date` from the source period through the day before conversion; they describe filed returns and remitted amounts respectively. Their detail tables retain and show that event date and the corresponding `filed` or `remitted` status. Preview the entire schedule and obtain consent before committing. A used reference is refused, including on retry; read the list after a lost response.

The retained record pins the opening entry, period, currency, timezone, source reference, source system and recorder. The import does not assert completeness or reconcile the figures. An accountant must compare the schedules to the source reports and opening balances, resolve differences and retain that review. The accountant must also decide how to handle outstanding source-period tax and payroll obligations. No source amount is automatically included in a statement or statutory calculation; prior-year comparative columns, source-month reporting bridges and integration with the existing tax and payroll history views remain to be implemented. No correction or replacement operation is available yet.

Source files may be attached through Attachments to the resulting record, under `finance/reports/source_comparative`, `finance/reports/source_movement`, `finance/reports/source_sst` or `finance/reports/source_payroll` as appropriate. The record is final, so files can be added but not replaced or removed. Attachments applies the company's retention policy from the conversion date. Every file read requires ledger view; SST and payroll file reads additionally require that area's view. Importing any schedule requires ledger account management, and importing SST or payroll also requires that area's settings capability. The `source_period` auditor dataset requires ledger, SST and payroll view because the table contains all four kinds; it is included only in an all-Finance pack.

An accounting practitioner has not yet reviewed the comparative layout, source-period tax treatment or payroll bridge. In particular, source summaries must not be treated as evidence that an authority accepted a return or received a payment without the retained source acknowledgement. Reports, filing status and remittance history remain the accountant's responsibility. See [conversion evidence](../ledger/README.md#source-period-schedules-at-conversion) for provenance, cut-off, access, retention and the unsupported historical scope.

## Conversion reports

Use **Conversion reports** under Bookkeeping to issue a permanent reconciliation of the current company's opening records. The reporting date is one as-of date, not a date-range filter.

For example, issue the conversion report after importing the customer, supplier and asset opening registers. It keeps the source balances beside the target trial balance, the entry, register and settlement boundaries, all opening source CSVs and correction decisions, receivables and payables detail and control tie-outs, and the asset register's cost and accumulation reconciliation. Each source dataset has its last ID and SHA-256 digest. The report retains bank statement and reconciliation evidence, bank bridges and changes since their finalisation, and the recorded tax and payroll schedules. Other-owner control-account postings stay visible: recognition of opening detail never disguises an unrelated posting. To replay a snapshot, pass the complete five-field `pins` object returned by the report; omit `pins` for a current snapshot. Partial pin tuples are refused.

Enter the reviewer and workpaper references, your review conclusion, and whether bank, tax and payroll schedules apply. An applicable schedule needs a reference; “Not applicable” needs a reason. Unassessed schedules remain unresolved. Review the report's sections, then continue to confirmation and issue it. Issuance freezes the figures, labels, sources and exclusions; later backdated postings, chart changes or opening-register corrections cannot change that copy. A fresh report captures the later state. A changed source invalidates a pending issuance review.

**Issuance is not accountant sign-off.** A person granted `finance.reports.sign-off` opens the issued report, enters their reviewer reference and conclusion (including qualifications), reviews it and signs that exact snapshot. The record keeps their session identity, any impersonator and the database's signing time. It cannot be edited, deleted, truncated or signed twice. Ledger's duty-separation check covers the issuer, original opening preparer, and the preparers and actors recorded in retained source and material configuration evidence; it records any separation-policy exception through the shared control. There is no override-reason input on sign-off. The application records an attestation; it does not establish a person's professional qualification.

### Accounting judgement and limits

The default conclusion is **unassessed**, even when every numeric difference is zero. The accountant must decide the conversion cut-off, completeness, schedule applicability and whether qualifications permit sign-off. Bank bridges identify their own dates and posting, statement and match boundaries; a bridge ending before the report date, an open bridge, a missing bridge or a backdated change requires review. Tax/payroll evidence contains what their modules recorded; it does not reconstruct legacy filings or payroll year-to-date history. External schedules and their reviewer references must cover that gap. Earlier settled transactions, legacy bank matches and historical depreciation postings remain excluded. An asset's projected schedule is not posted history and is omitted from the conversion copy.

Open accounting question: the company and its accountant must agree the wording and supporting workpapers required for an unqualified conversion sign-off. The built-in attestation permits a qualified conclusion and never clears recorded differences or certifies statutory compliance.

All source-view grants are required to preview, issue or read a conversion copy, including through general issued-report reads and evidence exports. A ledger-only auditor pack explicitly excludes conversion copies and sign-offs; a full pack includes them. Each source is read through its owning facade on one database snapshot for a standalone preview, or the reviewed write's company-locked transaction for issuance. Report size is limited to 64 MiB, with complete evidence paging; exceeding the limit refuses the whole report.

### Shared page and agent contracts

| Operation | Input beyond `company_id` | Result |
| --- | --- | --- |
| `finance.reports.conversion.preview` | `as_of`; optional complete `pins` object | Current or pinned reconciliation and retained source evidence |
| `finance.reports.conversion.list` | optional `after` issued ID | Up to 100 issued reports and next cursor |
| `finance.reports.conversion.get` | `report_id` | Frozen report and sign-off events |
| `finance.reports.conversion.issue` | `as_of`, `reviewer_reference`, `note`; optional complete `pins` object, `bank_status`, `tax_status`, `payroll_status` and corresponding `_reference` fields | Reviewed immutable issued report |
| `finance.reports.conversion.sign` | `report_id`, `reviewer_reference`, `note` | Reviewed immutable accountant attestation |

Schedule status is `not_assessed` (default), `applicable` or `not_applicable`. Issue requires `finance.reports.issue`; sign requires `finance.reports.sign-off`; every operation also requires all source views. HTTP API, MCP and the page use these same contracts. The page offers the complete retained evidence in sections, 100 records at a time, including source CSV and review references.

Run the conversion walkthrough on a fresh disposable database with `cargo run -p bilibi-finance-walkthrough-fixture --bin conversion`. Its report proof surrounds the existing register-correction lifecycle and checks stale plans, retained source rows beyond one evidence page, permission revocation, backdated control postings, duty separation, immutable issuance and qualified sign-off.

For a small browser fixture, append `-- report-browser` to that command. It keeps Company 003's opening records and an unsigned report, then stops before the bulk corrections. Start the Platform launcher against that database to review, issue and sign through the page.
