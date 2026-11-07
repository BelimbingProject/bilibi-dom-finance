# bilibi-dom-finance Agent Guide

This repository is the Finance Domain for Bilibi. It is not a standalone Cargo
workspace: it builds only when mounted in a Bilibi checkout at
`apps/domains/finance` (see `README.md`). Read Bilibi's `AGENTS.md`,
`apps/domains/AGENTS.md`, `apps/base/ui/AGENTS.md` and
`apps/base/ports/README.md` in that checkout before changing anything here;
they govern this code and are not copied here.

## Shape of the Domain

- **No production users yet (until 2027-01-01).** Never build data imports, backfills, conversion of existing rows, or compatibility tables, columns or code paths; replace or drop them in a new migration. Development and walkthrough databases are disposable. After 2027-01-01 this rule no longer applies: ask the owner whether to extend it before relying on it.
- Keep each module deep behind one small facade (`ledger/src/lib.rs`'s
  `Ledger` and its record types), and keep its schema, SQL, validation and
  helpers private (`mod`, `pub(crate)`), not `pub`: a sibling that reaches a
  table or helper couples itself to this module's storage, which the facade
  exists to hide. Add a `pub` only for something the facade's contract names.
- Never add a Cargo dependency, descriptor edge, port call or SQL reference to
  another Domain's container (Supply Chain, Factory, People, ...): cross-domain
  flows use Base domain events (`apps/base/events/README.md`), and the
  subscriber keeps its own copy of what it needs. Where the publisher has no
  events yet, leave a clearly named `TODO(domain events)` seam at the place the
  flow would start, and do not reach across. Bilimbi's `OrderSource` and
  `PeopleSource` seams are not ported as ports.
- Read order lines and goods receipts only from Payables' own copy
  (`payables/src/orders.rs`), kept from Supply Chain's documented events by
  name and parsed into its own serde types; add an event to its subscriber and
  `SOURCE_EVENTS` only once the pinned Supply Chain declares it, because boot
  refuses a subscriber to an event a mounted publisher does not declare.
- Within this Domain, depend on a sibling module only through its facade,
  declared both in the descriptor's `dependencies` and the Cargo manifest,
  only where Bilimbi has the same edge (`finance/attachments` ← `ledger`,
  `receivables` ← `attachments`, ..., `reports` ← all), and never in a cycle:
  the registry refuses an undeclared service reference and a cycle, and an
  edge Bilimbi lacks is new coupling the parity port must not add.
- Mount a real copy (`git clone`), not a symlink, and commit only inside this
  repository: `apps/domains/Cargo.toml`, `apps/domains/src/lib.rs` and
  `Cargo.lock` belong to the Platform in their unmounted form.
- Use meta-terms only (book, account, account type, entry, line, journal,
  period, duty separation, approval exception, ...). A chart of accounts or a
  currency is a company's data, never a code identifier, schema constant or
  default; the five account types are the one fixed classification.
- Name a migration `finance-<module>-NNN-<slug>` with the SQL file under the
  module's `migrations/`, declared in the descriptor; the registry refuses any
  other name. An applied file never changes (the migration ledger refuses a
  changed hash): a schema change is the next version.

## Facade rules

- Authorize every facade call inside the facade with
  `bilibi_company::authorize_work_target` and the scope it was
  handed, and open every transaction it owns with `Ledger::write` (the capability, then the
  audited `MutationBoundary` transaction holding
  `bilibi_company::require_writable(.., lock = true)`), never a bare
  `client.transaction()`: the boundary is what records every affected row as a
  `Change`, and the company lock is what serializes a company's ledger writes
  and refuses an archived company. Never take an actor or actor id as an
  argument.
- Decide a refusal (a taken code, a cycle, a balance) with `tx.sql()` before
  `tx.mutate`, and report it as a `LedgerError` variant or `FieldError`s with
  Bilimbi's words, never as `Error::Forbidden`/`Error::Internal`: a mutate
  closure that returns no `Change` poisons the transaction, and the live
  transport treats those two errors as fatal.
- Take a book's currency and its standard decimal places from Core Currency (`ledger/src/currency.rs`, `core/currency` README), never a free-text code (except when Core's currency list is not installed) or a list of your own; a page offers the `finance.ledger.book.currencies` contract's options in a `Combobox` and shows the review's `currency` facts, so a person and an agent see the same name, places and nonstandard flag. Any current currency may be chosen: Core's `check` in the book's own write (`currency::choose_on`) marks it used, and the options list used currencies first; there is no enable step and never a trip to the Currencies page first.
- Hold amounts as `Money` built with the book's `MinorUnits` (`Money::parse`,
  `Money::new`), never a bare `Decimal` from input: the columns are
  `numeric(20,4)`, and an amount finer than the book's minor units is refused
  by the type, not rounded by PostgreSQL.
- Prove behaviour with `fixtures/walkthrough` on a disposable database
  (`cargo run -p bilibi-finance-walkthrough-fixture`), extending its chart and
  scenarios, not with unit tests or mocks: the gate is
  `scripts/domain-ci.sh` mounted, and the running proof is the facade's own
  answers. A fixture binary that composes the modules itself boots through
  `bilibi_finance_walkthrough_fixture::services` (or `bilibi_web::App::new`),
  never a bare `Services::default()` with only `bootstrap.database`: the
  pinned Platform refuses to install without `bootstrap.composition`.
- The ledger owns no CSS or JavaScript. Bilimbi's ledger has no pages
  (`web: nil`); this port's are in `ledger/src/web/`. A Finance module with
  pages uses only `bilibi_ui` components and `FlexTable` for every list, per
  `apps/base/ui/AGENTS.md`.
- A ledger page reads and writes only through its operation contracts
  (`PageContracts`: `read`, `submit` for a dialog the person filled in,
  `review` then `commit` for a confirmation), never the `Ledger` facade for
  something a contract covers. Add a contract in `ledger/src/agent_operations.rs`
  when a page needs an action that has none. Declare `PageWrites` under the live
  event name the browser raises (`create`, `do_confirm`), not an internal label:
  the host permits a write by that name.
- A dialog whose fields change together (a journal's lines) is a `Form` with
  `change: true` and its buttons `type="button"` with their own `event`; the
  server keeps the draft (`ledger/src/web/journals.rs`). Show a refusal beside
  its field, and what has no field in the dialog's own alert (`Refusal::alert`).
- Stack a Detail page's (`PageVariant::Detail`) cards in one `space-y-4`
  wrapper below its header (`sst/templates/return.html`): that variant adds
  no gap of its own, so cards placed one after another touch.
- Lay a form dialog out as the shared specimen does
  (`apps/base/ui/templates/ui_feedback_specimen/modal_templates.html`): the
  `Form` gets `class: "mt-4"` under the title, and its actions are Cancel then
  the one primary verb at the end of the row, through `dialog_actions`
  (`ledger/src/web/mod.rs`) or `format::actions` (Receivables), never the bare
  `Form` row, which starts at the left. Render the dialog inside the page
  element, not after it: outside `data-shared-ui` the legacy sheet drops
  required marks and checkboxes under their labels. Name a dialog
  `<record>-modal`, because the shared dialog owns `{id}-title`,
  `{id}-description` and `{id}-body`, and a `description` field in a dialog
  named `<record>` would share its id and lose its label.
  Keep a Cancel or Close in every state of a dialog, including one whose read
  was refused and renders no form (the three `*/src/web/opening.rs`): the
  shared `Modal` draws no X. Say that refusal in the dialog's own `Alert`, not
  its `FlashGroup`, which floats past the dialog's edge on a phone, and link
  the place that clears it (Go to opening journal).
  Let that round trip also drop what a changed field no longer allows (a group
  of another type once the type changes, `Draft::reconcile` in
  `ledger/src/web/accounts.rs`) and offer only eligible choices, instead of a
  hint beside an option the ledger will refuse.
- Offer an account as a choice with `bilibi_ui::Combobox` over
  `view::account_choice` (`ledger/src/web/view.rs`; `Account::choice` in
  Receivables, `Choice::new(name, id).detail(code)` elsewhere), never a native
  select over `view::account_label` text: the box and list draw the name first
  and the code muted and italic, and typing searches both. Keep
  `view::account_label` ("Cash · 1000") for a sentence and `account_name`
  (`ledger/src/web/mod.rs`) for HTML a page controls; never `code name`. An
  inline editor passes the combobox as `InlineChoice`'s editor with an id of
  `...-input` (the wrapper owns `...-editor`). Platform gaps: the combobox draws
  no required mark (it sets `required` on its input only), so a required one
  carries a trailing `*` in its `label` with the plain name as `aria_label`
  (a stopgap until the Platform's Combobox draws the shared required marker),
  and the filter
  toolbar has no combobox, so a coded filter (an account, a currency) is
  `filter_combobox` (`ledger/src/web/mod.rs`) beside the toolbar, posting the
  same `filters` event at the toolbar's height; give the toolbar
  `class = "min-w-0 flex-1"` to share its row (`books.rs`, `journals.rs`).
- Declare every page or panel service's write events in its `PageWrites`
  (Bilibi `apps/base/agent_api/README.md`): the host refuses, as
  `Error::Internal` at run time, a live-event write its service has not
  declared, and the build does not catch it. Payables' pages write
  through contracts (`Pages::write`, `payables/src/web/mod.rs`), except approving
  match exceptions, a listed `Pending` allowance; pages still using
  facade writes keep a `Pending::Every` allowance with its reason beside
  their routes, and a new write event calls its operation contract through
  `PageContracts` where one exists.
- Put only foreign actions in a row's Non-local actions menu (`AddToMenu`):
  ones that open another module or kind of record (New invoice, a company's
  fiscal years or chart). A local action on the record itself (reverse, copy,
  close, edit, delete) is a plain `IconButton` with its tooltip on the row
  (`ledger/src/web/journals.rs`, `years.rs`), and a row with no foreign action
  has no menu (captain, 2026-11-07). A menu item is a link, so it can only
  open a dialog that needs no review on open; the host refuses a review
  outside a live event.
- Offer an action on another module's record (New invoice on a customer, company or contact) as a `bilibi_records::drop_target!` beside the page's `PageWrites`, whose page opens its own dialog from `?drop=`/`drop_records` (`receivables/src/web/documents/new_draft.rs`), never a button or link of the list's own: every list of that kind then shows it in its row actions menu, only to people holding the contract's capability. A list of records that are a party shows that menu per row through `Records::add_to` (`receivables/src/web/actions.rs`).
- Build a page on contracts, not the facade: use `Pages::read`, `read_all` and `write` in `receivables/src/web/mod.rs` (they go through `PageContracts` and unwrap the envelope), and name each write event's contract in `PAGE_WRITES` with `Pending::None`. A read with no contract gets a contract in the owning module (`aging.get`), not a facade call from the page.
- A settings page names the company it changes (header subtitle and each dialog's description, through `Pages::company_name`); edit a settings row's name, status or account in place with `InlineEdit`/`InlineChoice` keyed per row on a `save_event` listed in `PAGE_WRITES`, and open a dialog only for a new record whose fields are fixed once saved. `receivables/src/web/settings.rs` and its twin `payables/src/web/setup.rs` are the running pattern (an on/off is a `Toggle` in the row, a new record a dialog from its section heading), including a setup that needs two values in one write (the first choice waits on the page, nothing half-set is persisted) and a change history folded under `Fold` (`DESIGN.md`: History is a quiet disclosure).
- Write from a page with `Pages::write` (the contract the agent doors call, event listed in `PageWrites`), never `Payables::approve_document` and the other facade writes: the facade skips the contract's validation and review, so a person and an agent stop getting the same answer. Show `Written::Fields` (keyed by input path) on the fields, not as one flash.
- Record a bill or debit note, or correct a draft, in the Bills list's dialog (`payables/src/web/bill_form.rs`, opened by "+ Bill", `?correct=<draft>` and `?debit_note=<bill>`), as New invoice is the Invoices list's; never a page of its own (one thing, one door).
- A recorded document is immutable, so "edit a draft" is a corrected draft that replaces it, through `finance.payables.bill.replace_draft` (`Payables::replace_draft_on` voids the old draft and records the new one in one transaction, so a refusal leaves the draft as it was); never void then create in two calls, and do not add an update path. List a page of payment requests with `page_payment_requests` (newest first, the total from SQL), never by loading batches of `list_payment_requests` and cutting them, which drops the newest. A form with many fields reads its own `name[...]` keys: `params::form_values` keeps only 64.
- Leave builder detail (a fingerprint, an environment every row shares, a
  stored unit code) out of a list's default columns: mount the list without
  it and offer it through `ListColumns::with_record_fields`
  (`einvoice/src/web/index.rs`), so it waits in the column picker.
- Narrow a list by a date range with the shared period filter
  (`bilibi_datetime::PeriodFilter`, `apps/base/datetime/README.md`; the
  pattern is `ledger/src/web/journals.rs`), never bare from/to date inputs or
  a range of the page's own: it keeps presets, the custom-range draft and the
  calendar zone the same on every page, and its events carry its id
  (`period_select:<id>`). An as-of date (trial balance, aging), a tax period
  (SST return, e-invoice month) or a figure's drill-down window is not a
  range filter; keep its single control and say so in the module README.
- Lay out an aging report as the twins do (`receivables/src/web/aging.rs`, `payables/src/web/aged.rs`): the as-of date is a `ToolbarControl::date` in the shared toolbar and updates live, one table by party with a Total row, and each bucket amount links to the same page narrowed to that party's bucket (`?customer=`/`?supplier=` and `bucket=`), never a date card with a Show button or the report's pins as facts.
- Lay out a document's record page (invoice, bill, receipt, payment) as the twins do (`receivables/src/web/document/view.rs`, `payables/src/web/bill/view.rs`): `DocumentLayout` puts the document left and a Summary card (status, amount due, `EntryLink` to its journal, who did what, files) right, and `RecordActions` draws Back, the record's local actions (edit, void, reverse, print, discard) as plain icon buttons with tooltips (`RecordAction::local`/`here`), one "Non-local actions" menu only for actions that open another record or module (`RecordAction::link`: New credit note, Record debit note), and the one primary verb for the record's state (`ledger/src/record_page.rs`). A local action that opens this page's own dialog is `RecordAction::here` (`?do=<action>`), which the page opens after load in `first`/`mounted` and on `Params`. Never a local action in the Non-local actions menu, and never a row of text links. A list row draws the same way through `RecordActions::row`, with the record page's actions that the row can decide from its own data, never one the page might refuse (such as one needing the document's lines or amount due).
- Name the people a page mentions with `People` (`ledger/src/people.rs`, through Base's principal directory: `Pages::people` in each module), never "User #5" or a formatter of the page's own; declare `base/principal_directory` in the module's dependencies.
- Show a money amount with the book's minor units through `Money::display` (`ledger/src/money.rs`), never `Money`/`Decimal` `to_string`: it groups thousands and keeps the book's decimal places. Keep `Money::shown` for editable, ungrouped values; a bare `Decimal` (a unit price, a report total) goes through `Money::display_decimal`, never a formatter of a page's own.
- Agent writes join the shared plan's audited transaction through the ledger's private `*_on` methods, never the pool-owning public wrappers: nested commits would escape plan rollback. Preview on the handed `OperationClient`; retain the transaction-local review fence because the pinned engine previews every step before executing any.

- Correct a submitted manual journal with `Ledger::replace_journal` (or `withdraw_journal` to only take it back), never `reject_journal` then `submit_journal` or an edit: only its submitter can, and it withdraws the journal and submits the correction in one transaction, so a refusal leaves it waiting and an approval racing it can never post the old content. The link is in `finance_ledger_journal_replacements`, not a column, because the journal and event evidence field lists are frozen. A page prefills its dialog from the journal's contract read (`?correct={id}`, `ledger/src/web/journals.rs`).
- A chart row is an account (takes postings, no children) or an account group
  (`is_group`: no postings, the total of what is under it). Offer or accept an
  account for posting or a setting only if `Account::postable()` (active and not
  a group), never `active` alone, and read group totals only through
  `Ledger::chart_balances`, never by adding up rows yourself. A starter chart is
  bundled data in `ledger/templates/*.json` (its `review` field is a product note, never shown in the UI) or an authorized saved company snapshot
  (`ledger/README.md#starter-charts`), previewed and applied only to an empty chart.
  A configured proposal creates no accounts when a book opens; never seed at boot
  or hard-code a chart in Rust.
- Post an entry only through `Ledger::post_entry` and read it through
  `get_entry`/`list_entries`; never insert into or select from
  `finance_ledger_entries`/`_entry_lines` from a sibling. Pass `owner` for an
  entry a module's record recognises (the database allows one original entry
  per record and refuses a reversal by anyone but the owner), and
  `settlement_account_ids` only to settle a document posted earlier on an
  account deactivated since. The database numbers entries, so never send or
  compute an `entry_number`.
- Use `Ledger::post_entry_in` inside `Ledger::write` when your record must
  commit with its entry; never call standalone `post_entry` while holding
  that company's lock on another connection, which would wait on itself.
- A new book is controlled, so it posts only into a fiscal period opened with
  `Ledger::open_fiscal_year`: a fixture opens one before posting, and a sibling
  reports the ledger's `entry_date` refusal instead of picking another date. A
  period's or year's status is its latest closing event's, never a column;
  inside the ledger read it through `periods::state` and record each override
  of a control (a reopen, controlled bookkeeping off) with
  `duties::record_override` in the same write.
- For Finance consumers not yet converted to Workflow, ask `Ledger::check_approval` on your own `MutationTransaction` before every
  approval you record, with everyone whose work it approves in `prepared_by`
  and an `override_reason` for an override; never compare actor ids yourself:
  the ledger counts impersonation both ways, records the exception an
  independent reviewer reads, and a `SamePerson` refusal must roll your
  approval back. Correct an entry you own with `Ledger::reverse_owned_entry` on
  the transaction that records your own reversal, never `reverse_entry`, which
  refuses it as `OwnedBy`.
- Learn of a posting through `Ledger::subscribe_postings`, never your own
  LISTEN: the relay shares one connection, and a notice carries identifiers
  only. On `PostingEvent::Refresh` read again instead of trusting the stream.
  `TODO(platform)`: the relay opens its connection from `DATABASE_URL` because
  `base/database` exposes only the pool.
- Read a ledger figure with `Ledger::account_totals` or `trial_balance`
  (PostgreSQL sums the lines), never by adding up `list_entries` lines, and
  read every column of one report at the first read's `through_entry_number`
  so it reproduces. Tie a control account out with
  `account_totals_by_owner`: a row of another owner is a difference to show,
  never to net away.
- Show a profit and loss, balance sheet, cash position, overview or a
  figure's entries through `Reports` (`reports/src/lib.rs`), never by
  arranging `account_totals` yourself: the facade reads every column at one
  entry number, leaves closing entries out of flows (and their drill-down)
  and groups by `finance.reports.sections` with its nearest-listed-account
  rule. Freeze a report with `Reports::issue_report`, not a stored entry
  number, which pins postings but not the chart or the layout.
- Export a module's auditor evidence as the ledger does
  (`ledger/src/evidence.rs`): `evidence_datasets` and an authorized
  `evidence_page` on the facade, one dataset per own table with a frozen,
  explicit field list (never a credential), ID-ordered pages of 1 to 500.
  The auditor pack reads each module through its facade, never its tables.

- Keep a record's source documents through `Attachments` (`attach`, or
  `capture_file` then `attach_capture` inside your own transaction), never
  through `bilibi_artifacts` or the `finance_attachments*` tables: the facade
  is what links the bytes to the record, applies the company's retention years,
  numbers and audits the attachment, and records holds. Describe your record
  with a `Source` contributed to `finance/attachments.sources` under
  `<your module id>/<kind>` that reads it on the connection it is handed; do
  not hand the attachments module a facade of yours, which would make a cycle.
  Call `refresh_retention` in the transaction that finalises a record whose
  date can change while it is a draft; never extend retention yourself.
- Show a record's source documents by mounting the embed `finance.attachments-panel` (`PANEL_EMBED`) as a `PanelSpec` (path `panel`, `source`, `document`; `attachments/README.md`), not an upload form, a download route or `Attachments` calls of your own: the panel owns the live upload, asks the facade on every action and reads whether the record is final from your `Source`, so a page passing its own flag cannot offer replace on a final record. Link a file as `/finance/attachments/{id}/download`; that route reauthorizes on every click.
- The Platform header supplies the page pin; pages do not wire it themselves. Finance identity icons (`gmdi-money-r`
  default; `tabler-receipt-tax` tax, `vaadin-invoice` invoices,
  `iconoir-receive-dollars` receivables, `iconoir-send-dollars` payables,
  `carbon-financial-assets` assets) are vendored in the Platform's
  `apps/base/ui/assets/vendor/product-icons/`; a new glyph is a Platform change
  with its licence, not an SVG in this repository.
- In an Askama template never name a variable `id` (or any macro parameter name) used inside a `{% call ui::... %}` body: it resolves to the macro's own, so `{{ id }}-form` renders as the card's id.
- Name a system principal as one lowercase hyphenated segment
  (`finance-attachments-retention-refresh`): Bilibi's vocabulary refuses
  Bilimbi's dotted names. Declaring one grants nothing; a tenant administrator
  grants it (`Grants::set_system`) or its jobs cancel as `actor_refused`.
- Read a company's e-invoice configuration only through `Einvoice::setup`, and
  its code lists only through `list_codes`/`override_code`; never select
  `base_settings` rows or `finance_einvoice_*` tables from a sibling. A secret
  (client secret, certificate, private key) is never returned by the facade: a
  later slice that signs or submits reads it inside `finance/einvoice` only,
  masked everywhere else.
- Put what one clearance authority defines (document types, posting rules,
  shapes, versions, windows, URLs) behind `Authority` in `myinvois.rs`, not in
  the facade, a setting or a page; a second authority is a second
  implementation, not a branch.
- Correct a fiscal document with a new status event or a note, never an edit:
  the e-invoice tables except `finance_einvoice_code_overrides` are
  append-only in the database, and a schema change is the next migration.
- Declare a setting with the `define!` macros in `configuration.rs`, with the
  editor `capability` set; a validator must accept `null` for a nullable value,
  because Base Settings runs it on cleared values too.
- Read sibling context inside a write with `Ledger::get_book_on`,
  `list_accounts_on`, `get_entry_on` and `Payables::get_document_on`, not pooled
  methods: another connection while holding the company lock can exhaust the pool.
- Add a bank account with its name and ledger account only, and map its bank's columns on its first import (`Reconciliation::preview_statement` guesses them, `import_statement_mapped` keeps them with the account in the import's own write; `reconciliation/src/web/import.rs`), never an always-open mapping form or mapping fields in the account's dialog: a person maps columns while looking at the file's rows, and a bank that changes its export is caught by the next import.
- Register assets through `FixedAssets`, never by posting or claiming their
  acquisition entry: ordinary acquisitions freeze one whole source debit and its revision; opening registers use `import_opening_register` against approved aggregate balances, preserving source accumulation and prospective remaining life;
  projected schedules are not posted depreciation (see `fixed_assets/README.md`).
  A page offers the source debit as a choice from `finance.fixed_assets.acquisition.sources`
  (the bill or journal line, name first), never a typed record or line number.
- Name a person a page shows (recorded by, posted by, history "By") through the
  Platform's principal directory (`People` in `fixed_assets/src/web/mod.rs`),
  never "User #5": it resolves only what the reader may see and keeps "User 5"
  for anyone it cannot name.
- A customer or supplier permanently links at creation to a Core Company or
  Person through Base Records (`record_kind`/`record_id`). Finance owns no party
  code or copied identity fields: use `PartyLinks` (`ledger/src/party_links.rs`)
  for the immutable Core code and permitted facts on the handed connection,
  never Core SQL. Both Add dialogs use `MultiPartyChooser`
  (`ledger/src/party_picker.rs`) and the owning Finance create contract, which
  commits Core creation, the relationship and Finance record together. Never
  create a Core party and link it in a second write. Receivables freezes the
  Core facts and dated billing address at issue, refusing masked facts.
- Never offer or accept an account receipts were paid into (`finance_receivables_payments.deposit_account_id`) as the receivable control account, nor the current control account, or an allocated invoice's saved control, as a receipt's deposit: `prepare_setup` (`receivables/src/configuration.rs`) and `record_payment` (`receivables/src/posting.rs`) refuse them for a person and an agent alike, and the settings page and receipt dialog leave them out of their choices. Receivables knows no account as "bank" beyond its own receipts.
- Change receivable drafts through `Receivables`, not its tables: null/missing
  lines preserve snapshots, an array replaces them atomically, and discard
  keeps them. Issue through `issue_document` and settle through `record_payment`
  / `reverse_payment`: they retain each invoice's original control after setup
  changes and commit its record with the owned entry. Read dated figures with
  the report facade at its entry, opening-batch and settlement pins (`report.pins()`), never
  the unbounded document outstanding.
- Validate a document to issue with `Draft::validate` (the shape every
  authority shares, then `Authority::validate`), never with checks of your own
  on the facade: it derives every amount and answers `FieldError`s at the
  field's path in Bilimbi's words. Write the authority's JSON only through
  `myinvois::json::Json` and build and sign it only through `myinvois::ubl`
  and `myinvois::signer` (or `Authority::payload`), never `serde_json`: the
  authority hashes and signs exact bytes, and `serde_json`'s maps sort keys
  and reformat numbers. Read a certificate's facts with `Certificate::parse`
  and report them, never the PEM.

- Give a person audit or sales-only access with the roles in `ledger/appointed-auditor-and-sales-access.md` (`appointed_auditor`, `appointed_auditor_payroll`, `sales_workflow`) on a Platform company/date-bound assignment, never a Finance table, a role-name check or an expiry job. A capability that such a role holds opts in with `CapabilityDefinition::company` only once every path to it decides with `authorize_work_target` for the explicit company; a sibling that must work without `finance.ledger.view` uses `Ledger::book_figures_on` / `list_accounts_for_on` with its own capability, never a ledger view grant. Prove the role with `fixtures/walkthrough/src/bin/engagement-access.rs`.
- A page acts on the session's current company: read it with `scope.current_company()` (each module's one `company()` function, `Pages::company` in the ledger), never `scope.actor().organisation()`, which is portal business identity, not a staff workplace (Bilibi ADR 19). Call it "current company" in visible words. It grants nothing: every read and write still names its `company_id` and the contract or facade proves the reach.
- A page that lists or filters several companies takes them from `Pages::companies` (`ledger/src/web/mod.rs`, the top bar's list) and reads each with an explicit company through `Pages::read_in`, `review_in`, `submit_in` or `book_in`, never a query across companies and never by switching the current company. Name the company in every row, action and confirmation; say "No access" (the contract refused) apart from "Not open" or "None"; leave a company the person cannot work in out of the list. Do not add figures across companies (`ledger/src/web/books.rs`, `years.rs`).
- Put a view that spans Finance modules (the setup overview) in `reports`, the one module that depends on every sibling (`reports/src/setup.rs`), and read each area through its own facade; a module's own page names its own area and links, never reads a sibling. Show a status only where the owner can determine it (`not_tracked` otherwise), and a count only when every counted area was read.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
