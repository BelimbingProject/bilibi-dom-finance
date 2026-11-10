# Ledger

Module ID: `finance/ledger` (Bilimbi `Bilimbi.Finance.Ledger`). A company's
general ledger: its book, chart of accounts (accounts, account groups and starter charts), journal entries, posting
notices, fiscal years with period closing and the year-end close, controlled
bookkeeping and late-recording reviews, reversals, the separation of duties
with its approval exceptions and their reviews, manual journals, the trial
balance and account totals (also by owner) at a pinned entry, and the evidence
datasets of the auditor's pack. Other Finance modules and other Domains post
and read through this module's facade; none of them reads its tables.

## Book currency

The currency list, names and decimal places come from Core Currency (`core/currency`, ISO 4217; its README owns the source, seeds and updates). The ledger keeps no list of its own. Commits check a code through `Currencies::check`, which marks it used; a preview and the `book.currencies` read run Core's catalog query on the connection they were handed (`Currencies` reads cannot take the handed client), so they never wait for a second pooled connection under the company lock.

- **Proposal.** The Book page and `finance.ledger.book.currencies` propose the company's own base currency, else the tenant's, when it is not withdrawn, and say which. Nothing is hard-coded.
- **Choice.** Opening accepts any current currency; there is no enable step. A code Core does not know or has withdrawn is refused with that reason. `book.currencies` lists every current currency with whether the tenant has `used` it, the used ones first, and the Book page offers them in one searchable list ("MYR Malaysian ringgit"). The commit's `Currencies::check` marks the chosen one used in the same transaction as the book.
- **Decimal places.** They default to the currency's ISO 4217 value (`minor_units` may be omitted by an agent). A different value is allowed, since a company may keep finer or coarser books; the review and the page mark it *nonstandard* and name the standard. The review shows code, name and places (`changes.after.currency`: `currency_name`, `standard_minor_units`, `nonstandard_minor_units`, `reference`).
- **List not available.** Only when Core's list was never installed (its `currencies` table is empty), a well-formed code is accepted *unchecked* and `minor_units` is required. The page shows a plain code field that says the code is not checked; the review says `reference: unchecked`; `book.currencies` reports `reference: unavailable`. Once the list is installed this path closes.
- Platform follow-up: Core has no capability-free "list installed" answer (`Currencies::catalog` needs the view capability and a concrete client), so the ledger asks `SELECT 1 FROM currencies LIMIT 1` on the connection it was handed.

## Public API

Resolve the `Ledger` service (`finance/ledger.ledger`) from the registry and
call it with a sealed `Scope` and an explicit company id. Results are plain
records (`Book`, `Account`, `Entry`, `FiscalYear`, `Period`, `ClosingEvent`,
`ControlledBookkeeping`, `LateRecording`, `DutySeparation`,
`ApprovalException`, `Journal`, `TrialBalance`, `AccountTotals`,
`OwnerTotals`, `EvidencePage`, ...); the schema is private.

| Function | Capability | Does |
| --- | --- | --- |
| `get_book`, `get_book_on` | `finance.ledger.view` | The company's currency code and minor units, or `BookNotOpen`; `_on` uses the caller's connection |
| `open_book` | `finance.ledger.accounts.manage` | Opens the book once with `currency_code` (three letters, trimmed and upcased; see [Book currency](#book-currency)) and `minor_units` (0 to 4); a second call is `BookAlreadyOpen`. A new company's book is controlled (see Controlled bookkeeping) |
| `list_accounts`, `list_accounts_on` | `finance.ledger.view` | The chart of accounts by code; `_on` uses the caller's connection |
| `create_account` | `finance.ledger.accounts.manage` | Adds an account that takes postings: `code`, `name`, `account_type`, optional `parent_id` (an account group of the same type), `active`, `retained_earnings` |
| `update_account` | `finance.ledger.accounts.manage` | Changes `name`, `parent_id`, `active` or `retained_earnings`; `AccountNotFound` for an id of another company; refuses an account group (use `update_account_group`) |
| `create_account_group`, `update_account_group` | `finance.ledger.accounts.manage` | An account group takes no postings and stands for the total of what is under it: `code`, `name`, `account_type`, optional `parent_id` (another group of the same type). Update changes `name` or the group it sits in; code and type never change |
| `chart_balances` | `finance.ledger.view` | Each account's own `balance` and each group's `total` as of a date, at a pinned `through_entry_number` (see Account groups) |
| `chart_templates`, `preview_chart_template`, `apply_chart_template` | `.view`, `.view`, `.accounts.manage` | Starter charts: list them, preview the exact rows (with the optional `sections` chosen) against a company's chart, and create them in one transaction in a chart with no accounts or groups (see Starter charts) |
| `post_entry` | `finance.ledger.entries.post` | Posts a balanced entry of at least two lines (see Entries); `BookNotOpen` before the book is opened |
| `post_entry_in` | `finance.ledger.entries.post` | Same posting inside the owner's `MutationTransaction`; leaves commit/rollback to the owner |
| `preview_post_entry_on`, `preview_reverse_owned_entry_on` | `finance.ledger.entries.post` | Validate an owned posting/correction on the handed `OperationClient`, without identities or writes; deterministic entry, referenced accounts and applicable posting controls for the owner's review fence |
| `write` | caller's supplied capabilities | Opens an audited writable-company transaction, reauthorizing inside it, for a Finance record owner |
| `get_entry` | `finance.ledger.view` | One entry with its lines and the entry that reversed it; `EntryNotFound` for another company's id |
| `list_entries` | `finance.ledger.view` | Entries by date then id, 100 by default and 500 at most, paged with `after`; bounded by `from`, `to`, `account_id` and `through_entry_number`; `exclude_closing` leaves out year-end closing entries and their reversals |
| `list_entries_on` | `finance.ledger.view` | `list_entries` on the caller's connection, for a sibling reading entries inside its own write while it holds the company lock |
| `reverse_entry` | `finance.ledger.entries.post` | Posts a manual entry's reversal (see Reversals) |
| `reverse_owned_entry` | `finance.ledger.entries.post` | The owning module's reversal of its own entry, inside its own transaction |
| `subscribe_postings` | `finance.ledger.view` | A `PostingSubscription` that hears each committed entry of the company (see Posting notices) |
| `open_fiscal_year` | `finance.ledger.periods.manage` | Opens the next fiscal year and its monthly periods, all open (see Fiscal years and periods) |
| `list_fiscal_years`, `get_fiscal_year` | `finance.ledger.view` | Years in date order with their status, closing entry and periods; `FiscalYearNotFound` |
| `list_closing_events` | `finance.ledger.view` | A year's closing log, oldest first |
| `close_period` | `finance.ledger.periods.manage` | Closes an open period, optional `reason` |
| `reopen_period` | `finance.ledger.periods.reopen` | Reopens a closed period with a `reason`; records an approval exception |
| `lock_period` | `finance.ledger.periods.lock` | Locks a closed period for good, optional `reason` |
| `close_fiscal_year` | `.periods.manage` and `.entries.post` | Posts the closing entry and closes the year with its open periods |
| `reopen_fiscal_year` | `.periods.reopen` and `.entries.post` | Reopens the year and its last period, reverses the closing entry; records an approval exception |
| `get_controlled_bookkeeping` | `finance.ledger.view` | The setting, its latest change and the number of fiscal years |
| `set_controlled_bookkeeping` | `finance.ledger.controlled-bookkeeping.manage` | Turns it on, or off with a `reason` (an approval exception); a call that changes nothing records nothing |
| `list_controlled_bookkeeping_changes` | `finance.ledger.view` | Every change, oldest first |
| `list_late_recordings` | `finance.ledger.view` | Entries recorded 45 or more days after their date, with their reviews (see Late recordings) |
| `record_late_recording_review` | `finance.ledger.late-recordings.review` | An independent reviewer's `reason` for one late entry |
| `get_duty_separation` | `finance.ledger.view` | Whether other Finance approvals require separation of duties, and its latest change |
| `set_duty_separation` | `finance.ledger.duty-separation.manage` | Turns it on, or off with a `reason`; a call that changes nothing records nothing |
| `list_duty_separation_changes` | `finance.ledger.view` | Every change, oldest first |
| `check_approval` | `finance.ledger.view` | A Finance module's approval, inside its own transaction: refused as `SamePerson` or recorded as an approval exception (see Separation of duties) |
| `list_approval_exceptions`, `get_approval_exception` | `finance.ledger.view` | The exceptions log with each review; `ExceptionNotFound` |
| `review_approval_exception` | `finance.ledger.approval-exceptions.review` | An independent reviewer's reading of one exception, optional `note` |
| `submit_journal` | `finance.ledger.journals.submit` | Keeps a manual journal for approval (see Manual journals) |
| `opening_progress` | `finance.ledger.view` | Latest opening submission, approval status, entry and any reversal; `None` before submission; measures opening balances only |
| `submit_opening_balances` | `finance.ledger.journals.submit` | Maps source CSV to a balanced approval journal; identical source retries return that journal (see Accounting system conversion) |
| `approve_journal` | `finance.ledger.journals.approve` | Posts a submitted journal as an entry by its approver |
| `reject_journal` | `finance.ledger.journals.approve` | Rejects a submitted journal with a `reason` |
| `withdraw_journal`, `replace_journal` | `finance.ledger.journals.submit` | The submitter takes back a journal that is still waiting, or corrects it: withdraws it and submits the corrected journal in one transaction (see Manual journals) |
| `get_journal`, `list_journals` | `finance.ledger.view` | Journals with their lines, status and decision; `JournalNotFound` |
| `trial_balance` | `finance.ledger.view` | Every account's totals and one-sided balance as of a date, optionally `through_entry_number` (see Trial balance) |
| `account_totals` | `finance.ledger.view` | Per-account debit and credit totals over entries dated `from` through `to`, optionally `through_entry_number` and `exclude_closing` (see Account totals) |
| `account_totals_by_owner` | `finance.ledger.view` | The same totals, one row per account and owner module, for a control-account tie-out |
| `evidence_page` | `finance.ledger.view` | One ID-ordered page of one of the ledger's tables for the auditor's pack (see Evidence) |
| `Ledger::evidence_datasets` | none | The twenty dataset names, sorted |
| `Ledger::account_types`, `Ledger::late_recording_thresholds`, `Ledger::journal_statuses` | none | The five account types; the report's 45 and 60 days; `submitted`, `approved`, `rejected`, `withdrawn` |

Every call authorizes the capability for the scope's actor and the target
company at the moment it runs (Core Company's `authorize_work_target`): a missing
grant, or a company where the person has no desk, is
`LedgerError::Unauthorized`; a company the scope cannot see is
`CompanyNotFound`. A write then holds the company's row lock inside an audited
`MutationBoundary` transaction, so an archived company is `CompanyArchived`
and a company's ledger writes run one at a time. Refused values come back as
`LedgerError::Invalid(Vec<FieldError>)` with Bilimbi's changeset messages
(`can't be blank`, `has already been taken`, `must be an equity account`,
`would place the account under itself`, ...).

## Agent operations

H1 makes the facade-only writes reachable through the Platform's shared `/api/v1` and `/mcp` contracts. Discover and describe `finance.ledger.*` and read the `finance.ledger` guide before writing. All inputs name `company_id`; tenant, actor and impersonator come from the sealed credential. Keys, titles and schemas use meta-terms; company-specific discovery wording belongs in `keywords` (none are hardcoded here).

| Suffix under `finance.ledger.` | Contract |
| --- | --- |
| `book.get` | Book or null, the currency's name and standard decimal places (read by code, even if no longer enabled), controlled-bookkeeping and duty-separation changes |
| `book.currencies` | Every current currency (code, name, ISO 4217 decimal places, `used`), the used ones first, matching an optional `search`; the proposed currency and its source; and `reference`: `iso4217` (currencies listed) or `unavailable` (list not installed) |
| `account.list` | Database-code-ordered chart of accounts and account groups (`is_group`); `after_code`, `next_after_code` |
| `chart_balance.get` | Every account's own balance and every group's total as of `as_of`, counted through a pinned `through_entry_number` |
| `chart_template.list`, `chart_template.get` | The starter charts, their optional sections, review state and whether each can be applied now; the exact rows one would create for the chosen `sections` and any code the chart already uses |
| `journal.get`, `journal.list` | Lines and decision; list by number with optional `status`, `after`, `next_after` |
| `journals.list` | Combined postings and unposted manual journals; filters `from`, `to`, `status`, `source`, `account_id`, `through_entry_number` and `search` (an exact entry or request number). `records` carry `kind`, `id`, `stamp`, `number` (the book's entry number, null until posted), `request` (the manual journal's request number, null for other sources), `date`, `description`, `source`, `status` and decimal-string `amount`; `more` signals another page, addressed by the last record’s `stamp`, `id` and `kind` in `after_time`, `after_id` and `after_kind`. `sources` lists the company’s posting modules. |
| `entry.get`, `entry.list` | Immutable lines, ownership/reversal identifiers and `request_number` (the manual journal request it was posted from, else null); list filters `from`, `to`, `account_id`, `through_entry_number`, `exclude_closing`; cursor pair `after_date`, `after_id` in `next_after` |
| `fiscal_year.list` | Years with periods/statuses; `after_start_date`, `next_after_start_date` |
| `trial_balance.get` | Every account with a posting on or before `as_of`, with debit and credit balances, counted through a pinned `through_entry_number` |
| `book.open` (`minor_units` optional; any current currency, marked used, see [Book currency](#book-currency)), `account.create`, `account.update` | Reviewed book/chart changes; account update preserves omitted fields and uses `clear_parent` to remove a grouping |
| `account_group.create`, `account_group.update` | Reviewed changes to account groups; update takes the group's id as `account_id` |
| `chart_template.apply` | `sections` names the optional parts to include (none by default). The review lists every group and account to be created; a chart that already has any is refused and never changed |
| `journal.submit`, `journal.approve`, `journal.reject` | Submission does not post; the final Workflow approval posts once, by independent approvers under the company's published journal rules; rejection records a reason without posting. A line may carry `balance: true` instead of `debit` and `credit` (on at most one line): the preview and the commit give it the debit or credit that balances the other lines at the book's minor units, and refuse `balance` beside a `debit` or `credit`, on a second line, or when the other lines already match |
| `journal.withdraw`, `journal.replace` | Only the journal's submitter, while it waits: withdrawal records an optional `reason` and posts nothing; replacement takes the `journal.submit` fields (including `balance`) plus `journal_id`, withdraws it and submits the correction, which needs its own approval |
| `opening_balances.get` | Latest opening submission with journal number, conversion date, status, posted entry and reversal identifiers, and `imported`: each module that recognised an opening account against the posted entry, with its account count; `opening: null` before submission |
| `opening_balances.submit` | Reviewed CSV mapping to one opening journal; source reference and exact CSV are retained; optional `replaces_journal_id` withdraws the submitter's waiting opening journal and takes its place; approval is separate |
| `entry.review_late` | Keep one permanent review with a required `reason`; the recorder and impersonation overlap are refused. The preview writes nothing, and the commit joins the shared plan transaction |
| `entry.reverse` | Mirror a manual entry once, never an owned or closing entry |
| `fiscal_year.open`, `fiscal_year.close`, `fiscal_year.reopen`, `fiscal_year.lock` | Open monthly periods, close with computed entry, reopen with closing reversal, or atomically lock all remaining closed periods |
| `period.close`, `period.reopen`, `period.lock` | Existing facade ordering, reason and permanent-lock rules |

List reads return at most `page_size` records (1–100, default 100). Writes require shared plan preview/commit; direct calls refuse writes. Preview validates normalized/defaulted values on the caller's connection, without business mutations, and shows exact proposed entry lines, current accounts/periods and consequential designation changes. Amounts are decimal strings, never binary floats. Commit reauthorizes and joins the shared audited transaction through private facade methods; all operations and audit rows roll back together on refusal. A successful record response has `path: null`: a write commits no page path.

The pinned Platform previews every step before executing any. A transaction-local FIFO of review hashes rechecks each step immediately before mutation, so earlier steps cannot silently change later reviewed facts. Dependent steps need separate plans. Same-company account edits conservatively review the full chart, so even independent account edits need separate plans. Whole-year lock composes existing period locks in date order on the same transaction; it adds no year-lock event. No operation exposes direct posting, owner/settlement bypasses or disabling controls. See [guide.md](guide.md) for consent, staleness and safe ordering.

### Live HTTP and MCP proof

Use a new disposable PostgreSQL 18 database and run these commands from the mounted Platform root. The bootstrap installs only fixture records, issues real device/API and resource-bound PKCE/MCP credentials for two independent people and another tenant, and saves them with mode 0600 using `create_new`. Do not use a production database or print the credential file. The existing facade walkthrough remains the default binary.

```sh
export DATABASE_URL=postgres://…/empty_agent_proof
export BILIBI_AGENT_PROOF=disposable
export BILIBI_AGENT_CREDENTIALS=/tmp/ledger-agent-credentials.json
export APP_ORIGIN=http://localhost:3100
cargo run -p bilibi-finance-walkthrough-fixture --bin agent-operations
cargo build -p bilibi-web
amp orb service start ledger-proof --port 3100 --command \
  'BIND_ADDR=127.0.0.1:3100 ./target/debug/bilibi-web run'
python3 apps/domains/finance/fixtures/walkthrough/agent-operations.py
```

The wire walkthrough exercises all writes over both transports, observes preview non-mutation and code non-consumption on rollback, checks exact approval/reversal/closing amounts, traverses mixed-case/non-ASCII account cursors, and refuses precision, balance, same-person approval, closed/locked periods, double reversal, cross-tenant reads and actor spoofing. Two defaulted year opens in one plan prove the intra-plan fence and rollback of the first insert. Credentials expire after one hour; rerun on another fresh database for another proof.

## Pages

The one-door walkthrough provisions two extra people on the launcher’s disposable `onedoor_finance_ledger` database with `cargo run -p bilibi-finance-walkthrough-fixture --bin one-door`. With its private credentials, run `fixtures/walkthrough/one-door-operations.py setup`, run the fixture again with `-- policies`, then run the Python walkthrough without `setup`. It exercises reviewed book/year creation, opening correction and approval, source/status/late/review filters, independent-review refusals and plan rollback through MCP.

Fiscal years is the only place to open a fiscal year. Filter by company and open, closed or locked state; a locked year has all its periods locked. Bookkeeping controls links to this list. Every year opening uses `finance.ledger.fiscal_year.open`, including its review and confirmation.

Opening balances has one page, `/finance/ledger/opening-balances` (Finance setup's Opening balances row and the Journals header link to it). Its steps checklist says where each part stands: **1. Account balances**, then **2. Unpaid invoices**, **3. Unpaid bills** and **4. Assets**, which say they wait for the balances journal's approval until it posts and then link straight to their owner's import dialog (`?opening=1` on Invoices, Bills and the asset register); a step shows Imported once that owner recognised an opening account (`opening_balances.get`'s `imported`). Step 1 is a grid of every account that takes postings, with a debit and a credit column, live totals and the difference; Submit stays greyed until they match. Type amounts in, or paste a spreadsheet range into **Paste from a spreadsheet**: the page shows the first rows, guesses which column is the account (code or name), debit and credit or one balance, and the decimal mark from the headers and values (`crate::columns`), and fills the grid only on **Fill the grid**, listing the rows it left out. Submitting sends one row per filled account (its code as the source ID) through `finance.ledger.opening_balances.submit`, the contract agents call, so its review, validation and separate approval apply unchanged. A rejected or withdrawn opening refills the grid; the submitter corrects a waiting one from **Correct** (`?correct=<journal id>`). Filter Journals' Source to **Opening** to list opening journals.

Company books is the only book list. Each company links to its book at `/finance/ledger/books/{company_id}`, including the reviewed open-book action. Viewing or opening another company’s book does not switch the current company. The list filters by company name, book state, currency and fiscal-year state.

The ledger has pages so a person without an AI agent can do the same daily work. Bilimbi's ledger has none (`web: nil`), so these are this port's own. Reports keeps the older [Bookkeeping controls and Approval exceptions pages](../reports/README.md#bookkeeping-controls-and-approval-exceptions-pages).

| Menu (Finance, under Bookkeeping) | Route | What a person does there |
| --- | --- | --- |
| Company books | `/finance/ledger/books` | Lists every company the person may work in with its book (open, not open, no access), currency, latest fiscal year and earliest open month, its name opens that company's book, its Non-local actions menu leads to the company's fiscal years and chart, and an icon button opens a book that is not open yet. The currency filter is a combobox of the books' currencies, found by name or code. See [Current company](#current-company-and-company-books). |
| Chart of accounts | `/finance/ledger/accounts` | Filters authorized companies' charts (default: current company), names each row's company and currency only when comparing every company, and shows each account's balance and each group's total. The retained earnings account carries a badge. Select one company to add an account in its named dialog. With an empty chart it offers the starter charts: preview, tick the optional sections that apply, then apply, or start empty. |
| Account groups | `/finance/ledger/account-groups` | Lists the groups with what each holds and its total, adds a group in a dialog. With no group yet it says so and offers + Group. |
| (account record) | `/finance/ledger/accounts/{id}` | Edits the name and group in place, switches the account on or off with the Active toggle (off asks first, and is not offered while the account has a balance), marks the retained earnings account, reads today's balance. |
| (group record) | `/finance/ledger/account-groups/{id}` | Edits the name and the group it sits in, reads its total and what it holds. |
| Journals | `/finance/ledger/journals` | Lists postings from every source and manual journals awaiting approval, rejected or withdrawn. Filter by status, source, account and period; the list pages 100 at a time. Submit a journal with its lines in a dialog; Fill difference balances a chosen line. `?correct={id}` prefills a correction and `?copy={id}` starts again from a rejected or withdrawn journal. A row's icon buttons reverse a posted manual entry (`entry-{id}?reverse=1` opens the reversal dialog) or copy a rejected or withdrawn journal. The request number is in the column picker. |
| Journal record | `/finance/ledger/journals/{id}` | Reads the lines. A waiting journal can be approved or rejected; its submitter can correct or withdraw it. A rejected or withdrawn one can be started again. A posted entry uses `entry-{id}` in the same route and offers reversal for an unowned entry; an owned entry links back to its invoice, bill or receipt, where it is corrected. |
| Fiscal years | `/finance/ledger/fiscal-years` | Lists the years of the current company, or of any company the person may work in through the company filter, each year and period named by its company. Opens a year for a chosen company after a review that shows the dates and the number of periods. Names every period by its month ("January 2026") and a year by its months ("January to December 2026"). Each open year has one main button for its next step: "Close January" for the earliest open month, and "Close year" only once every month before its last is closed (the last month closes with the year). Reopens and locks months with icon buttons on the month's row, and years with buttons on the year. |
| Trial balance | `/finance/ledger/trial-balance` | Reads the balances as of a date at a pinned entry number. Each balance is the link to the entries that make it up: the reports' account entries at the same date and entry number. The as-of day is one date, not a range, so it keeps its own date control rather than the period filter. |

Late recordings are columns and filters on Journals. **Recording** distinguishes on-time, approaching-late, late and unposted records. **Late review** finds pending or reviewed late recordings. Open the journal row to read or record its independent review through `finance.ledger.entry.review_late`. The review permanently keeps the reason, reviewer and time. The poster and anyone who overlaps through impersonation are refused.

### Current company and company books

Pages start from the session's **current company** (`Scope::current_company()`, chosen in the top bar, Bilibi ADR 09). Company books, book details, chart drill-through and Fiscal years may name another company explicitly without switching the current company. Either choice decides where the page looks and grants nothing: each contract proves the company's reach and the capability again. A page that needs the current company says "No current company" when none is selected.

**Company books** is the overview for a person who works in several companies. It lists each company the person may work in (the same list as the top bar's switcher, archived companies left out) and reads each through `finance.ledger.book.get` and `finance.ledger.fiscal_year.list`, the contracts an agent calls. A row says "Open", "Not open" (no book yet), "No access" (the person works in the company but the ledger refused the read) or "Could not be read", apart from one another. Currency, latest fiscal year and earliest open month (flagged "past due" when its last day is before today) are shown per company in that company's own currency; nothing is added across companies, and this page makes no consolidation claim (consolidation needs a scope, an account mapping, eliminations and currency translation, which Finance does not have). A company the person cannot work in is not listed at all, so nothing about it is disclosed. Choosing the company opens its book detail at `/finance/ledger/books/{id}`. The row's **Fiscal years** link filters that page to the company (`?company={id}`). Neither action switches the current company. An agent composes the same overview from `core.company.reachable`, `finance.ledger.book.get` and `finance.ledger.fiscal_year.list`.

The **Fiscal years** page starts on the current company. When the person works in more than one company, a Company filter offers each of them (the current one marked) and "All companies", which lists every company's years under its own heading. The filter belongs to the page: it does not change the current company. The Open fiscal year dialog has a required Company choice that starts on the listed company, else the current company; choosing another company proposes the day after that company's previous year, keeps a first day or last day the person typed, and recalculates what was still the proposal. The last day shows the calculated twelve months until the person picks another month-end; then the hint says "Chosen by you", gives the length in months (short under twelve, extended over twelve), repeats the calculated date and offers "Use calculated last day". The ledger accepts 1 to 24 months; that is a platform limit, not a statement that a company may use any length, so the hint asks the person to confirm it. The review dialog shows the company, the dates the ledger will open and the number of periods. A person who works in more than one company sees the company named in every confirmation and message of an action (open, close, reopen, lock); with one company the question is just "Close January 2026?". The contract proves the company's reach again when it runs.

Every read goes through `PageContracts::read` and every write through its `submit` (a dialog the person just filled in) or `preview` then `commit` (a confirmation), so a page and an agent get the same checks, the same refusals and the same audit trail. The pages add a combined `finance.ledger.journals.list` read for the Journals list, and three other contracts: `finance.ledger.trial_balance.get`, because the trial balance had none, and `finance.ledger.chart_balance.get` and the `chart_template` operations for the group totals and starter charts. A control the person may not use is not drawn. The ledger's own words explain a refusal.

The Journals list is ordered by posting or submission time and shows a "Later journals" link when another page exists. Approved manual journals appear once, as posted entries. The chart loads whole (up to 5,000 accounts) and the page searches and sorts it.

Journals use the shared period filter (`bilibi_datetime::PeriodFilter`, `apps/base/datetime/README.md`) beside the account filter. Entry dates are civil dates, both inclusive. A preset follows the display clock, including the browser's zone when the display mode is local, and is read again on each visit: an address kept overnight with `period=today` shows the new day and starts at its first entry. Until the browser has reported its zone in local mode, the address's own days stand, so a reload or Back keeps its place in the list. The page does not turn those days into UTC instants. Applying a period writes `period`, `from` and `to`, and starts again at the first matching entry; the account filter and the trial balance pin `through` stay. A `to` with no `from` and no `period` means on or before that day, which is how the trial balance opens this list. An address with no date keys lists every entry, not the shared default of the last 30 days, because the trial balance and the account pages open this list by account alone. The control then reads "All dates" and the address and the ledger get no date; once a period is applied, the "All dates" button beside the control returns to that state, starting at the first entry and keeping the account filter and the pin. A `to` with no `from` is drawn with the date field's own earliest day for its open end, which is never written to the address or sent to the ledger. The shared control has no "All dates" preset, so both are this page's own; a Platform follow-up would replace them.

Run the current-company proof with `--bin companies-ui` in place of `ledger-ui` below: it opens books in Companies 001 (MYR), 002 (JPY, a six-month first year) and 003 (USD, no fiscal year) of tenant 1, leaves the other 59 without a book, and signs in as `manager@example.test` (works in every company), `contractor@example.test` (Company 002 only) or `viewer@example.test` (no company).

Run the proof on a disposable PostgreSQL 18 database with `DATABASE_URL=… APP_ORIGIN=http://127.0.0.1:4361 BIND_ADDR=127.0.0.1:4361 BILIBI_SETTINGS_KEY=$(openssl rand -hex 32) cargo run -p bilibi-finance-walkthrough-fixture --bin ledger-ui`. It seeds a chart, two fiscal years with closed periods, entries, and manual journals in each state, then drives the Journals period control through its live state, printing one `Journals period proof` line, and serves the app. Sign in as `manager@other.example.test` (submits), `reviewer@other.example.test` (approves) or `viewer@other.example.test` (reads), password `bilibi-dev`. Add `-- empty` to start with no book. [`screenshots/`](screenshots) shows the pages at desktop and phone width.

To exercise sources together, run `--bin finance-ui -- journals` on a separate disposable database with `FINANCE_UI_STORAGE` set to a writable disposable directory as well as the variables above. It creates issued receivables and an explicitly historical Payroll posting, installs journal approval rules, and serves the app; submit and approve a journal as the manager and reviewer to see all three on Journals.

### Comparing charts across companies

The chart's Company filter offers only companies from the shared authorized company directory. It starts with the current company, which the URL does not record, so switching company in the top bar moves the chart and its Account groups link with it; choosing a company in the filter pins it, and the Account groups page then keeps that company. An accountant with several companies can choose All companies. An accountant with one company sees only that company. Every chart, book and balance is read with an explicit company through its operation contract. No access, a book not open, an empty chart and unreadable balances remain distinct; an unreadable balance is shown as a dash, never zero.

When the chart compares every company, each account row names its company and currency. Identical codes across companies remain separate accounts. Account and group links carry `?company=ID`; their pages identify that company, preserve it on Back and in sidebar pins, and bind every read, inline edit and confirmation to it. The top bar continues to show the session's current company: filtering and drill-through do not switch it or grant access. Journals still follow the current company, so an account belonging to another company asks the person to choose that company in the top bar before offering its entries link. Creation and starter-chart application require one selected company; changing the company filter drops any account draft or starter-chart review.

This is a side-by-side operational comparison. Balances are not summed across companies, converted or consolidated. Practitioner validation of the default view and subsequent cross-company queues is tracked in [#103](https://github.com/BelimbingProject/bilibi-dom-finance/issues/103); consolidation scope in [#102](https://github.com/BelimbingProject/bilibi-dom-finance/issues/102), and bulk book opening in [#104](https://github.com/BelimbingProject/bilibi-dom-finance/issues/104).


## Capabilities

Twelve keys, verbatim from Bilimbi, and one port addition (`.templates.manage`, which publishes
reusable charts and sets the starter-chart proposal), under the `finance` domain, all granted to
`tenant_owner`: `finance.ledger.view`, `.accounts.manage`, `.templates.manage`, `.entries.post`,
`.periods.manage`, `.periods.reopen`, `.periods.lock`,
`.duty-separation.manage`, `.controlled-bookkeeping.manage`,
`.late-recordings.review`, `.journals.submit`, `.journals.approve` and
`.approval-exceptions.review`. The module declares the verbs `post`, `reopen`
and `lock` as Bilimbi does, and also `submit` and `review`, which no Bilibi
Platform module declares (Bilibi merges verbs across modules).

## Money

`Money` is a `rust_decimal` amount held to a book's `MinorUnits`:
`Money::parse("12.50", units)` and `Money::new(decimal, units)` refuse an
amount with more decimal places than the book's minor units (`has more than N
decimal places`) and one of sixteen or more digits before the point, the
`numeric(20,4)` column's range. `shown(units)` writes it with exactly the
book's decimal places.

## Entries

`post_entry(scope, company_id, &NewEntry, &PostOptions)` posts one entry of
`entry_date`, `description` (1 to 500 characters), an optional `reference` (at
most 100) and `lines`: `NewLine::debit(account_id, money)` or
`NewLine::credit(..)`, each with an optional description. It is refused as
`LedgerError::Invalid(Vec<FieldError>)`, with every refusal at once, unless
there are at least two lines (`needs at least two lines`), debits equal
credits (`debits and credits do not balance`), each line has exactly one
positive side (`a line has either a debit or a credit`) no finer than the
book's minor units (`has more than N decimal places`), and each line names an
active account of the company (`is not an account of this company`,
`is inactive`). A `FieldError` about a line carries its 1-based `line`.

When a Finance owner must record its own posting in the same commit, open `Ledger::write(client, scope, company_id, capabilities, action)` and call `post_entry_in(&mut tx, scope, company_id, new, options)`. Both its capability and Ledger's are checked; the owner returns every changed row as a `Change` and owns commit/rollback. Never call standalone `post_entry` while holding the company lock on another connection: it would wait on itself. `reverse_owned_entry` already joins that same owner transaction.

Agent preview seams accept `OperationClient`, as do `get_book_on` and `list_accounts_on`, so a caller never obtains another connection while holding the company lock. Posting/correction preview requires posting authority, not view, and reuses the actual posting/owner/reversal validation. It tries the writable company lock without waiting, returns deterministic review facts and allocates no identity or number. That lock is the one a write holds (`FOR NO KEY UPDATE`), so a preview inside a write is unaffected and someone reading the company never refuses it; a company another write holds answers `Conflict` (the retryable `record_changed`), never a database error. Only the posting date's period and referenced accounts are frozen, not unrelated fiscal years or chart rows. If the record has no ID yet, preview without an owner and state its symbolic ownership in the caller's review; execution must reserve its ID and supply the real owner to `post_entry_in`. Preview is not a posting capability or reservation; execution still reauthorizes and validates.

`PostOptions` carries Bilimbi's two options. `settlement_account_ids` lists
accounts that may be inactive in this entry only, for a module settling a
document it posted earlier; each must still be an account of the company.
`owner` is the module (its descriptor id, `finance/receivables`) and record
(`<kind>:<id>`, `payment:17`) that recognise the entry. A record has at most
one original entry per company (`owner_record already has an entry`), and an
owned entry is corrected only by its owner. An `owner`, or an id, of another
shape is `InvalidOption`. A manual entry has no owner.

The database numbers each company's entries 1, 2, 3, ... without gaps and
stamps `posted_at`, whatever the writer sent. `Entry` is read back from what
PostgreSQL stored. Reading an entry authorizes `finance.ledger.view` afresh,
and an entry id of another company is `EntryNotFound`.

An entry's date must fall in an open period: never in a closed or locked one
(`entry_date falls in a closed period`), never before the company's first
fiscal year (`is before the first fiscal year`), and, while the company is
controlled, never where no period exists (`falls in no fiscal period: open the
fiscal year that covers it first`). The database refuses the same dates on its
own, and a posting holds a share lock on its period until it commits, so a
concurrent close waits for it.

## Reversals

A posted entry is never edited or deleted. `reverse_entry(scope, company_id,
entry_id, &Reversal)` posts its mirror image (each debit a credit and the other
way round, with the lines' descriptions) on `entry_date`, not before the
original's (`entry_date cannot be before the reversed entry's date`), with its
own `description` and optional `reference`. An entry is reversed once
(`AlreadyReversed`), a reversal is never reversed (`ReversalNotReversible`:
post a new entry), and a year-end closing entry is reversed only by
`reopen_fiscal_year` (`ClosingEntry`). Accounts deactivated since the original
still take its reversal, but its date must fall in an open period, as a new
entry's does. An owned entry is `OwnedBy(owner)`: correct it through its
owner.

`reverse_owned_entry(tx, scope, company_id, entry_id, &owner, &Reversal)` is
the owning module's correction path. The owner calls it on the
`MutationTransaction` that records its own reversal record (a payment
reversal, a cancellation); the ledger authorizes and takes the company lock on
that transaction and leaves the commit to the owner, so both stand or
neither. The reversal carries the same owner. Another module's or record's
entry is `NotTheOwner`, a manual entry `NotOwned`, and a malformed owner
`InvalidOption`. The owner is a seam, not an authorization boundary: the
ledger cannot tell which module calls, so anyone holding
`finance.ledger.entries.post` who passes an entry's owner reverses it without
the owner's record, and the owner's tie-out shows the difference.

## Separation of duties

This section covers Finance consumers still using `check_approval`; manual journals use the shared case below.

A company requires separation of duties unless it turns it off. The setting
is the latest of its append-only changes (`DutySeparationChange`); with none,
separation is required. `set_duty_separation(scope, company_id, required,
reason)` turns it off only with a reason (`reason is required to turn
separation off`).

`check_approval(tx, scope, company_id, &Approval)` is how every Finance
approval asks, on the approving module's own `MutationTransaction` (it
authorizes `finance.ledger.view` and takes the company lock there, so a
refusal rolls the approval back). `Approval` names the `source` module, an
`action` and `subject_type` code, the `subject_id`, a `subject` label for the
reviewer, everyone in `prepared_by` (`Preparer { user_id, impersonator_id }`)
and, for an approval that overrides a control, its `override_reason`. Two
people overlap when either one's user or impersonator is the other's user or
impersonator, so impersonation counts both ways. A preparer who overlaps the
approver is `SamePerson` while separation is required; while it is off, the
approval is allowed and recorded as a same-person approval exception with the
reason separation was turned off, naming that change. An override is always
recorded with its own reason. Anything else records nothing. The ledger's own
reopens and turning controlled bookkeeping off are recorded as overrides
(`period_reopen`, `fiscal_year_reopen`, `controlled_bookkeeping_off`, source
`finance/ledger`, no preparer).

`list_approval_exceptions` lists the exceptions oldest first, each with its
review; `ApprovalExceptionQuery` bounds `recorded_from` (inclusive) and
`recorded_before` (exclusive) instants, filters `reviewed`, and pages by
`after` (the last exception number) and `limit` (1 to 500, 100 by default;
`InvalidOption` outside). `review_approval_exception(.., note)` records one
independent review (`AlreadyReviewed` after), never by whoever approved or
prepared what it records or anyone overlapping them
(`ReviewerNotIndependent`), whatever the setting; a note is at most 500
characters.

## Manual journals

Manual journals use Workflow's shared approval cases. Open **Approval rules**, choose the current company and **Manual journals**, review the proposed rules and publish them with a reason. This is an explicit setup action; boot, opening a book and submission do not install a policy. The initial proposal needs one independent holder of `finance.ledger.journals.approve`. Rules can add stages, named reviewers, cover, amount conditions and reminders. Amount conditions use the book's currency and the journal's total debits.

`submit_journal(scope, company_id, &NewEntry)` validates and keeps immutable lines, then creates the Workflow round in that same transaction. The journal's ID identifies its immutable revision. Workflow keeps the preparer account and impersonator, resolves their Person identities, and checks both at decision time. An agent represents its account holder and never supplies another independent approval.

**Waiting for my approval** at `/workflow/approvals` lists eligible decisions. `approve_journal` and `reject_journal` enter the same coordinator; the existing Finance operation contracts retain their business names. A journal with several stages or eligible seats, or a required approval reason, is decided from the shared list, where the step, seat and reason are explicit. A person with no eligible seat (the preparer, an impersonator or an agent acting for them) is told another eligible person must decide, in preview and on commit alike, not sent to a list that will not show the journal. A vote can leave a multi-stage journal waiting. Only the final accepted approval calls Ledger's adapter to recheck accounts, amounts and periods, post one entry and append the final journal decision, all inside Workflow's handed audited transaction. A posting refusal leaves the vote unaccepted and the journal waiting. Duplicate shared commits replay their result and never post twice.

Strict independence is the default. A journal rule can deliberately permit preparer approval, with the published policy's reason recorded in the shared exception register. The Finance **Approval exceptions** page reads those rows through `base.workflow.approvals.exceptions`; the shared review contract checks every participant and the reviewer's live permission. The older Finance separation setting and `check_approval` remain authoritative for Payables, Payroll and the other unconverted consumers. They neither approve journals nor receive copies of journal exceptions.

Rejection needs a trimmed reason of at most 500 characters and never posts. Recall is available to the submitter while the round waits. The existing `withdraw_journal` and `replace_journal` calls coordinate recall through Workflow. Their immutable journal and replacement records remain in Ledger. Read-only previews use Workflow on the handed connection, and keep the rule, request, subject, stage, seat and routing fences in the reviewed contract.

A submitted journal is never edited. Its submitter (the same user; a person
with `finance.ledger.journals.submit`, else `Unauthorized`; anyone else is
`NotSubmitter`) may take it back before anyone decides it:
`withdraw_journal(.., reason)` appends the journal's one decision, `withdrawn`,
with an optional reason, who and when, and posts nothing; the submission stays
on record and can no longer be approved. `replace_journal(.., &NewEntry,
reason)` withdraws it and submits the corrected journal in the same
transaction, checked and numbered as a submission, and names it in
`replaces_journal_id` and `replaces_journal_number` (and the old one in
`replaced_by_journal_id` and `replaced_by_journal_number`). Either
refusal rolls both back, so a refused correction leaves the journal waiting.
The correction is a new journal that needs a fresh Workflow round under the current rules. An approval racing a withdrawal or replacement is
serialized by the company lock and by the one decision per journal: exactly
one wins, and the loser is `JournalDecided`, so a withdrawn journal's content
never posts. Posted entries are untouched; they are only reversed. The
rejection route stays: an approver rejects with a reason, and the submitter
starts a new journal from the rejected one (`?copy={id}` prefills it).

## Accounting system conversion

Opening balances establish the source closing trial balance in a book **without posted entries**. After approval, import the [opening customer register](../receivables/README.md#opening-customer-register-at-accounting-system-conversion), [opening supplier register](../payables/README.md#opening-supplier-register-at-accounting-system-conversion) and [opening asset register](../fixed_assets/README.md#opening-asset-register-at-conversion) through their own reconciled operations, and record [opening bank reconciliation items](../reconciliation/README.md#opening-bank-bridge) through Reconciliation. Retain source-period schedules separately. Operational tax and payroll history, and historical/comparative transactions remain unsupported.

### Conversion date and source evidence

The **conversion date is the first date Bilibi owns transaction processing**. Close the source system immediately before that date, in the company's accounting timezone, and post the opening journal on the conversion date. For example, a 1 July conversion uses the final source closing trial balance at 30 June. Agree the cut-off, accounting timezone, transactions in transit and first live posting with the accountant; avoid overlapping source and Bilibi postings.

Retain the approved source trial balance, chart mapping, source system/export date, frozen backup and supporting schedules. Choose a permanent source reference for this export. The ledger retains the submitted CSV, mapped journal lines, preparer, reviewer and posting through its normal audit and evidence contracts. Retain the original reports and sign-off alongside the company's records; the CSV does not replace them.

**Client-specific accounting advice:** the accountant determines the applicable reporting framework, retained earnings, mid-year income and expense treatment, comparatives, tax balances and any currency or rounding adjustments. A mid-year source trial balance must preserve the YTD income and expense position. Imported income and expense balances post on the conversion date, so that month's flows include the opening YTD amounts. Earlier months and prior-year comparatives are not reconstructed. Review the source YTD reporting bridge before relying on period reports.

### Prepare and review in the UI

1. Open the book with the source currency and appropriate minor units. Create the approved chart and open the fiscal year covering conversion. Complete these steps before preparing the opening journal.
2. Open **Finance setup → Opening balances** (or the Opening balances button in Journals). Enter the conversion date and a permanent source reference, then fill each account's balance: type it, or paste the source trial balance from a spreadsheet and check the columns the page guessed before choosing **Fill the grid**. The currency is the book's.
3. When debits and credits match, check every account and amount in the review against the source closing trial balance. An inactive account, invalid precision or unbalanced total refuses the entire submission. No suspense line or currency conversion is added.
4. Choose **Submit for approval**. This keeps one immutable journal and its source evidence; it posts nothing. The returned journal appears in Journals.
5. An independent authorized reviewer opens that journal, chooses Approve and reviews the approval decision. The ordinary period, account and Workflow independence controls still apply. Where the journal rules permit preparer approval, it records an exception in Workflow for independent review.
6. Open **Bookkeeping → Conversion reports**. Review the source and target balances, unresolved differences, excluded history and supporting schedules. Issue the immutable report, then have an independent accountant record sign-off against that exact copy. See [Conversion reports](../reports/README.md#conversion-reports).

The contract's `csv` (which the page builds from the grid, and an agent sends itself) is UTF-8 CSV with commas, optional BOM, quoted fields and CRLF/LF/CR line endings. Use this exact header, unique source row identifiers and ungrouped positive amounts on one side of each row:

```csv
source_id,account_code,debit,credit
source-bank,1000,10000.00,
source-capital,3000,,10000.00
```

These example codes and currency choices belong to fixture companies, not product defaults. Replace them with the approved chart mapping. A source ID identifies one source trial-balance row, not an invoice. Multiple source rows may map to one account; reconcile their combined balance. Omit zero-balance rows. The limit is 2–500 balance rows and 128 KiB; larger conversions are not supported by this operation and must not be split into repeated opening journals.

### Retry and correction

Retry the same source reference, conversion date, currency and **exact CSV bytes** after a lost response. The operation returns its recorded journal, including its current decision, without another journal or entry. Whitespace/line-ending changes count as different CSV bytes. Reusing a reference for different data is refused. Another source reference is also refused while a live opening journal exists (submitted or approved), protecting against importing one source twice under a different name. A rejected or withdrawn opening journal is no longer live; only the last journal of a correction chain counts, so a rejected or withdrawn correction frees the book for a fresh submission.

If a submission is wrong, there are three ways to correct it while it waits for approval, and none leaves two live openings. A reviewer may reject it with a reason, or its submitter may withdraw it (`finance.ledger.journal.withdraw`); either way submit the corrected CSV with a new source reference. Or its submitter may correct it in one step: submit opening balances again with `replaces_journal_id` naming the waiting journal (the journal page's Correct button opens `/finance/ledger/opening-balances?correct=<journal id>` with the grid filled from it) and a new source reference. The waiting journal is withdrawn and the correction takes its place, carrying its own source reference and CSV as the book's opening record; it needs its own approval, and the withdrawn journal can never post. The manual `replace_journal` refuses an opening journal, because a manual replacement would carry no opening record. The earlier sources remain evidence. Before opening subledgers have been imported, an approved entry can be reversed through the existing reviewed reversal operation and corrected with an independently approved journal. Once a customer, supplier or asset opening register depends on it, reversal is refused by the facade and database. Committed register corrections use their owning module’s staged replacement contracts. They retain the original recognition and append its transfer to the new register. `replace_opening_recognition_on` requires the current recognition owner and unchanged approved balance; it never releases the journal’s reversal guard. After posting, a decided journal cannot be withdrawn or replaced. Do not delete, overwrite or repeat the opening import. Read the balances again at a new entry-number pin after correction. Closed periods and archived companies retain their existing controls. The existing manual-journal UI accepts at most 12 lines; larger correction journals currently require the reviewed `finance.ledger.journal.submit` operation through MCP. The opening balances page takes typed or pasted amounts, with no file picker in this slice.

### Prepare and review through MCP

Describe `finance.ledger.opening_balances.submit` and read `book.get`, the complete paged `account.list`, `fiscal_year.list` and `trial_balance.get`. Use the same `conversion_date`, `currency_code`, `source_reference` and `csv` as the UI, plus `company_id`. Run one dry check, inspect all `changes.rows`, totals and exclusions, obtain consent and commit the exact inputs. Keep the source reference and read back before retrying if the response is lost.

Approval uses `finance.ledger.journal.approve` in a **separate plan and review** with an eligible reviewer. Book opening, account creation, fiscal-year opening, opening submission and journal approval cannot rely on hypothetical earlier plan results. Respect the installation's plan bounds and read generated identifiers before the next dependent step. Never send database credentials or source-system passwords in CSV, descriptions or references.

### Reconciliation report and unsupported opening state

Retain a concise report with the conversion date/timezone, source system/export and reference, source closing trial balance, target trial balance and entry pin, account-by-account differences, journal/entry and reviewer references, unresolved exceptions and excluded history. Include the following supporting schedules where applicable:

| Area | Current boundary and follow-up |
| --- | --- |
| Receivables and payables | Keep source document IDs, original/due dates, gross/tax amounts, currencies, prior allocations, credits and residual unpaid amounts. The opening registers import positive residual invoices, bills and unapplied customer or supplier credits without another posting; each item retains its historical control account and every classified control reconciles separately. **Do not enter ordinary invoices, bills or credit notes to recreate them:** doing so duplicates control balances and may duplicate income, expense and tax. Later credit applications and refunds are immutable settlement events. Larger registers and committed corrections use each module's staged batches and replacement. |
| Bank and cash | Reconcile the opening bank ledger balance to the source trial balance, bank statement and outstanding cheque/deposit bridge. Opening bank reconciliation items are not created here; record them through the [reconciliation module's opening bank bridge](../reconciliation/README.md#opening-bank-bridge). |
| Fixed assets | Retain source IDs, cost, accumulated depreciation, carrying values, methods, lives, residual values and depreciation through cutover. Reconcile register totals to the GL. Use the separate opening asset-register operation to preserve source cost and accumulation and continue depreciation prospectively over remaining life. Other prospective methods and source historical detail remain follow-up work. |
| Tax and payroll | Reconcile source liabilities/refunds, filed/paid periods and supporting tax/payroll schedules separately. The journal creates no tax document or tax-return history. |
| Historical/comparative reporting | Preserve the source backup, signed financial statements, prior closing trial balance and historical documents. This slice imports no historical transactions, comparative periods or original invoice issuance. |

The scope follows the [28 October 2026 practitioner review on issue #57](https://github.com/BelimbingProject/bilibi-dom-finance/issues/57#issuecomment-6461498216). The bounded opening-state workflow is separate from the bank, historical, tax and sign-off follow-ups listed below.

### Source-period schedules at conversion

Retain source schedules and their supporting files as evidence. Opening balances and residual imports are not an archive of historical transactions, tax calculations or filings. The same limits apply when using the pages or the MCP operations in [the conversion guide](guide.md). The [source-period schedule operation](../reports/README.md#source-period-schedules) retains bounded summaries; it does not import statutory history. Only the monthly movements are compared with the opening journal, in the [year-to-date bridge](../reports/README.md#year-to-date-bridge).

| Schedule | Retain and reconcile | Operational boundary |
| --- | --- | --- |
| SST | Keep each source-period tax calculation, taxable supplies and adjustments, return version, filing acknowledgement, payment and outstanding liability or refund. Reconcile the tax control balance to the opening trial balance and explain timing differences from the return. | A ledger balance is not a tax computation or a filing. SST returns in Finance cover its own documents and adjustments; finalising a return does not file it. A retained source schedule may record a filed status, but it creates no Finance SST return or filing event. A later receipt of an opening invoice does not reconstruct its source tax. Ask the accountant how to account for any tax still due on that receipt outside this workflow. |
| Payroll | Keep payroll summaries by period and statutory authority, employee-level supporting records where needed, liability calculations, remittance references, payment dates and unpaid amounts. Reconcile each liability to the source and opening ledger. | An opening liability is an accounting balance, not a payroll run or a remittance. A retained source schedule may record a remitted status, but it creates no Finance payroll run or remittance. Arrange outstanding submissions and payments with the responsible payroll provider. |
| YTD bridge | Keep monthly source income and expense movements, the source closing trial balance and a mapping to each target account. Reconcile the cumulative source amount to the opening journal ([the year-to-date bridge](../reports/README.md#year-to-date-bridge) shows each account's difference) and show later target movements separately. | The opening journal posts YTD balances on the conversion date. A target monthly flow for that month includes those opening amounts; earlier monthly movements are not recreated. Do not add the source YTD and opening journal twice. |
| Comparatives | Keep the approved prior-year financial statements, closing trial balance, accounting policies, account mapping and any adjustments needed for comparison. Reconcile each comparative line to its source and document differences in presentation. | Retained comparative figures do not become prior-year transactions or management-report columns. Use the retained statements and an accountant-reviewed bridge for comparatives. |
| Prior allocations | Keep the source invoice or bill, gross and tax amounts, payments, allocations, credits and unpaid balance at cut-off. Reconcile the residual to its opening control account. | Only the residual becomes an opening record. No historic receipt, supplier payment or allocation is created; later settlements are new target transactions. |

For every schedule, record the company, source system and export/version, permanent reference, preparer and reviewer, period and cut-off including the company's accounting timezone, currency and precision, source and target totals, target entry and report pins, differences, explanations and sign-off. Identify incomplete records and unresolved items. Keep original-currency amounts separately; no translation is implied by an opening import. The accountant must approve any translation and document its rate and adjustments.

The archive owner must restrict access to people entitled to see each schedule, especially employee and taxpayer details, and set retention and legal holds under the applicable policy and law. Record the archive location, custodian and retention decision in the reconciliation; do not put credentials in it. Finance's opening batch evidence retains its own submitted exports, not these external schedules. The accountant is responsible for completeness, cut-off, source-to-target reconciliation, statutory treatment, outstanding filings and payments, and the reporting bridge. Confirm the retention period and permitted access with the company's advisers.

**Open design:** accounting practitioners must decide which source-period SST and payroll items require domain-owned operational records, including treatment of payments after conversion, and approve the reconciliation and reporting model. Until then, retained reports are evidence only and are not presented as migrated historical transactions or statutory submissions.

### Disposable proof

Run `cargo run -p bilibi-finance-walkthrough-fixture --bin opening-balances` on a fresh disposable composed database. It exercises mapping/balance/precision/currency refusals, racing retries, separate approval, six account balances, rejection, withdrawal and resubmission, correction by replacement (the replacement carries the opening record; a second live opening, a manual replacement and approving the replaced journal are refused) and the reversal correction. `ledger-ui opening` serves a chart and open fiscal periods with no posted entries for browser proof; `ledger-ui opening-proof` first drives the Opening balances page through its live state (a pasted trial balance with guessed columns and rows left out by name, live totals, review, submit, correction, approval, then steps 2–4 opening) and prints one `opening page proof` line. The existing `agent-operations` credential fixture plus `fixtures/walkthrough/opening-balances-operations.py` runs the same opening balances through MCP on a separate clean company. These proofs cover opening ledger balances only; they do not claim operational item migration.

Validated on 28 October 2026 against the pinned Platform revision, first in the browser on Company 100, then through MCP on a separate clean Company 002. Both used a 1 July conversion date and the same six balances: bank 10,000 debit, receivables 3,000 debit, expense 2,000 debit, payables 1,500 credit, capital 8,000 credit and income 5,500 credit. Both reconciled to 15,000 on each side. Reversal and independent approval of the revised journal changed bank to 10,100 and capital to 8,100, reconciling to 15,100; the original entry pin still reproduced 15,000. Each final book retained one import, two journals and three entries.

The supported opening path required two explicit UI reviews, or two reviewed MCP plans: submission and independent approval. The UI book/chart/periods were prepared by the fixture; MCP setup required eight additional reviewed writes. Additional checks covered unmapped and unbalanced refusals, identical retries, a same-person refusal, a 1,766-byte/100-row UI preview cancelled without business writes, read-only UI access, and dependent-plan rollback through MCP. Desktop and phone previews were inspected. The standalone MCP fixture explicitly stores the Dev instance role for loopback HTTP; setting `BILIBI_ENV` alone does not change that stored role. That proof covers opening ledger balances only; the opening-register and [bank-bridge](../reconciliation/README.md#opening-bank-bridge) proofs are described separately. Historical transaction migration remains outside the opening import.

## Posting notices

`subscribe_postings` returns a `PostingSubscription` once its listening
connection is up, so it hears every entry committed afterwards.
`subscription.next().await` yields `PostingEvent::Posted { company_id,
entry_number }` for each committed entry of the subscribed company, from any
process, and `PostingEvent::Refresh` when notices may have been missed (the
connection dropped, or the subscriber lagged): read the ledger again. The
`finance_ledger_entries_notify` trigger calls `pg_notify` in the posting
transaction, and PostgreSQL delivers it only at commit, so a refused or
rolled-back post announces nothing. A notice carries identifiers only; read
figures through `get_entry`/`list_entries`. The process holds one dedicated
connection, opened by the first subscription from `DATABASE_URL` (the variable
the Platform builds its pool from) and re-opened every five seconds after a
failure; dropping a subscription ends it.

## Fiscal years and periods

`open_fiscal_year(scope, company_id, &NewFiscalYear)` opens the company's next
year, divided into calendar-month periods. `start_date` is the first day of a
month (`must be the first day of a month`) and `end_date` the last day of a
month (`must be the last day of a month`), one to 24 months on (`must end one
to 24 months after the start`); the end defaults to twelve months on. The
first year may start on any month; each later year starts the day after the
previous one ends (`must be <date>, the day after the previous fiscal year
ends`), which is also its default. `BookNotOpen` before the book is opened.

A status is never stored: it is the latest closing event's (open with none).
`close_period` closes in date order (`EarlierPeriodOpen`; `PeriodNotOpen` for a
closed or locked period), and a year's last period closes only with its year
(`ClosesWithFiscalYear`). `reopen_period` needs a reason (`reason is required
to reopen`) and reopens in reverse date order (`LaterPeriodClosed`); a locked
period never reopens (`PeriodLocked`), an open one is `PeriodNotClosed`, and a
closed year's periods reopen only after the year (`FiscalYearClosed`).
`lock_period` locks a closed period in date order (`EarlierPeriodUnlocked`).
Reasons are trimmed and at most 500 characters. Each call returns the period
with its new status.

`close_fiscal_year(scope, company_id, year_id, &YearClose)` posts one entry,
dated the year's last day (description `Close fiscal year <start> to <end>`
unless given, optional reference), that brings each income and expense
account's balance for the year to zero against the active retained-earnings
account (`NoRetainedEarningsAccount` without one), then records the year's
close (naming the entry and the account) and closes every open period of the
year. A year with no income or expense balance closes with no entry; the
previous year's periods must be closed first (`EarlierPeriodOpen`); closing a
closed year returns it unchanged. `reopen_fiscal_year(.., reason)` reopens
the year and its last period and posts the closing entry's reversal on the
same day (`Reopen fiscal year <start> to <end>`); the other periods stay
closed. It is `FiscalYearNotClosed`, `PeriodLocked` when the last period is
locked, and `LaterPeriodClosed` while the next year has a closed period.
Closing entries and their reversals may name inactive accounts, are never
reversed by hand, and are left out of `list_entries` with `exclude_closing`
and of the late-recording report.

A reopen overrides a close and turning controlled bookkeeping off overrides a
control, so each is recorded in `finance_ledger_approval_exceptions` with its
reason (`period_reopen`, `fiscal_year_reopen`, `controlled_bookkeeping_off`,
source `finance/ledger`); see Separation of duties.

## Controlled bookkeeping

A company that is controlled posts only into a fiscal period it has opened
and left open, so its first year is opened explicitly and an entry dated after
its last year is refused until the next one is opened; an entry is never
refused for being old. The setting is the latest of the company's append-only
changes (`BookkeepingChange`: number, setting, reason, actor, impersonator,
time); `open_book` records the change that turns it on, so a new company is
controlled from the start. `set_controlled_bookkeeping(scope, company_id,
controlled, reason)` turns it off only with a reason (`reason is required to
turn this off`) and returns the new `ControlledBookkeeping`; setting what is
already set records nothing.

## Late recordings

An entry's days are whole UTC days from its `entry_date` (its transaction
date) to `posted_at` (its recording). `list_late_recordings` lists the entries
of 45 days or more, oldest number first, `Approaching` up to 60 days and `Late`
past it, each with its review; `LateRecordingQuery` filters by `status` and
`reviewed` and pages by `after` (the last entry number) and `limit` (1 to 500,
100 by default; `InvalidOption` outside). `record_late_recording_review` is
refused for an entry that is not listed (`NotLateRecorded`; `EntryNotFound`
for another company's or a closing entry), a second time (`AlreadyReviewed`),
and for a reviewer who posted the entry or impersonated (or was impersonated
by) its poster (`ReviewerNotIndependent`); its `reason` is required.

## Account groups

The chart is a tree whose leaves are accounts and whose inner rows are account
groups. An account takes postings and has no children. A group takes none and
stands for the total of every account under it; it has no balance of its own,
cannot be deactivated and cannot be the retained-earnings account. `parent_id`
names the enclosing group: an account or group sits only in a group of its own
type, in its own company, never in itself or in something it holds. A row is an
account or a group for good, like its code and type. Posting to a group is
refused by the facade (`account_id is an account group and takes no postings`)
and by the database (trigger on entry lines). Version 008 turned any account
that already had children into a group and stops with the rows named if one
held postings; the old rule that any account could parent another is gone.

`chart_balances(scope, company_id, as_of, through_entry_number)` reads every row
by code at one pinned entry number: an account's `balance` and a group's
`total`, on the type's natural side (debits less credits for asset and expense,
credits less debits for the others), negative on the other side. PostgreSQL sums
the lines per account for the trial balance; the group rolls those sums up the
tree, so no posting is counted twice. The trial balance, account totals and
every report list posting accounts only, so a group is never a row there.

Reports: a section lists account codes, and a listed group gathers every account
under it unless a nearer listed row claims it (`reports/src/layout.rs`). A group
is never a report row, and the section's total is the sum of its accounts.

## Starter charts

A starter chart is data (`templates/*.json`: key, name, review state, notes,
optional sections and rows of code, name, type, group, retained-earnings mark,
section), never a code identifier. One ships: `my-small-business`, a Malaysian small business chart
marked **draft, awaiting accountant review**. A person previews it on the Chart
of accounts page (or an agent reads `chart_template.get`) and applies it with a
reviewed write; nothing is created at install, on boot or when a book opens. Saved company charts and setup proposals extend this flow below.
Rows that only some businesses need belong to an optional **section**, each with a
one-line description of when to tick it: `Registered for SST` (the sales and service tax
payable group and its accounts), `Has employees` (net salaries payable, payroll
deductions and employer contributions payable, the employer EPF, SOCSO and EIS
expenses), `Holds inventory`, `Has intangible assets` (at cost, accumulated
amortisation, amortisation expense), `Has tax recoverable or advances`, `Takes
customer deposits`, `Gives sales returns or allowances` and `Earns finance income`.
No section is chosen by default: the
preview shows a tickbox for each, the page and `chart_template.get` / `.apply`
take the chosen keys as `sections` (the same input for a person and an agent),
and the review lists exactly the rows that will be created. Rows of a section
that was not chosen are never created; a section's rows sit only under a group
of the same section or a core group (the file is refused otherwise). A group
cannot be deactivated once created, so omitting happens when applying. Which
tax basis applies to a business needs client-specific advice and is a separate
note on the chart. Applying creates its core groups and the chosen sections' accounts in one transaction under the company
lock, each audited, and refuses any chart that already has an account or a
group: a book that has accounts is never changed, and applying twice is refused
the second time. The created rows are the company's own with no link back, so
two companies that start from the same template customise their charts
independently and a later edit of the template never reaches a book. The
template carries no opening balance and no rate; conversion work (#57) is
separate. Corrections from an accounting practitioner are a data edit of the
JSON file.

## Trial balance

`trial_balance(scope, company_id, as_of, through_entry_number)` lists every
account with a posting dated on or before `as_of`, by code, with its `debits`
and `credits` and its balance on one side (`debit_balance` or
`credit_balance`, the other zero). Each account reports its own postings
only, and a group never has a row: its total is `chart_balances`.
`total_debit` equals `total_credit` because every entry balances. Closing
entries count, so as of a closed year's last day every income and expense
account is at zero and the year's result is in retained earnings.

The result names `through_entry_number`, the last entry it counts: the one
passed, or the company's last, read in the same statement as the sums. An
entry posted later with an earlier date changes the figures for that date, so
passing the same `as_of` and `through_entry_number` reproduces an earlier
trial balance's figures exactly: entries are numbered in posting order and
never change. Account names, parents and active flags are read as they are
now. A negative number is `InvalidOption`, and `BookNotOpen` comes before the
book is opened.

## Account totals

`account_totals(scope, company_id, &TotalsQuery)` returns, for every account
with a posting dated from `from` through `to` (both optional and inclusive;
`from` after `to` is `InvalidOption`), its `debits` and `credits`, by code.
PostgreSQL sums the lines per account, so a report over any number of
entries reads one row per account; never add up `list_entries` lines for a
figure. It names `through_entry_number` like the trial balance, and reading
several windows at one number gives a report's columns one state of the
ledger. `exclude_closing` leaves out every year-end closing entry and every
reversal of one, so income and expense over a closed year show its activity
(a profit and loss reads this way, a balance sheet counts them); the set is
the same at any entry number, because a close names its entry in the
transaction that posts it. `list_entries` with the same `account_id`,
`from`, `to` and `through_entry_number` lists the entries behind a figure.

`account_totals_by_owner` splits the same totals by `owner_module` (the
owner's descriptor id, `None` for entries with no owner, first): one row per
account and owner. A module ties its control account out with it: its own
rows should be the whole account, and any other row is a posting its records
do not know, shown as a difference with its owner, never netted away.
Bilimbi's `legacy` flag is not ported, since Bilibi has no adopted entries.

## Evidence

`evidence_page(scope, company_id, dataset, limit, after)` reads one
ID-ordered page of one dataset with its explicit `fields`, its `rows` (JSON
objects) and `next`, the last id when the page was full; `limit` is 1 to 500
(500 when absent) and `after` the previous page's `next`. An unknown dataset
or an option out of range is `InvalidOption`. The datasets are `account`,
`approval_exception`, `approval_exception_review`, `book`,
`bookkeeping_change`, `chart_template`, `closing_event`, `duty_separation_change`, `entry`, `journal_approval`,
`fiscal_year`, `journal`, `journal_event`, `journal_line`,
`journal_replacement`, `late_recording_review`, `line`, `opening_balances`, `opening_recognition`, `opening_recognition_replacement` and `period`. Existing datasets mirror Bilimbi's field lists (`src/evidence.rs`), less the entry's `legacy` and plus the account's `is_group`; `opening_balances` exports the retained source reference, CSV and journal link; `opening_recognition` exports each owner's recognition of an opening account; `opening_recognition_replacement` links a recognition to the one it replaces. Dates are `YYYY-MM-DD`,
instants ISO 8601 UTC with microseconds and amounts the column's exact text
(`100.0000`). Each page is a live read on its own connection. The field lists
are an export contract: freeze one once exported and never add a credential.

## Schema (versions 001 to 009)

`finance_ledger_books` (one per company; currency and minor units fixed for
good), `finance_ledger_accounts` (code and type kept by a trigger, a
composite `(id, company_id)` parent key so an account is never grouped under
another company's, one retained-earnings account per company, never deleted)
and `finance_ledger_bookkeeping_changes` (append-only, numbered and stamped by
the database, refusing a change that changes nothing). Books and bookkeeping
changes refuse UPDATE, DELETE and TRUNCATE.

`finance_ledger_entries` and `finance_ledger_entry_lines` (002) are
append-only the same way. Composite `(id, company_id)` keys keep a line on its
own company's entry and accounts and a reversal on its own company's entry;
the entry's `(currency_code, company_id)` names its book. Triggers number
entries per company without gaps, refuse a line added to an entry whose
transaction has ended, and, deferred to commit, refuse an entry with no lines
or whose debits and credits differ. A line has exactly one positive side. An
owned record has one original entry, and a reversal carries exactly its
original's owner. 003 adds the notice trigger.

004 adds `finance_ledger_fiscal_years`, `finance_ledger_periods` and
`finance_ledger_closing_events` (all append-only): a year starts the day after
the previous one and, deferred to commit, is divided into whole months through
its end, in the transaction that opens it; the database numbers and stamps
each event and refuses one that breaks the closing rules above, a reopen with
no reason, and a closed year with an open period. An entry dated in a closed
or locked period, before the first year, or (controlled) in no period is
refused. `finance_ledger_late_recording_reviews` takes one review per entry,
refusing an entry recorded under 45 days late and a reviewer who overlaps the
poster. `finance_ledger_duty_separation_changes`,
`finance_ledger_approval_exceptions` and
`finance_ledger_approval_exception_reviews` are the end state of Bilimbi's
separation-of-duties tables, all append-only: the database numbers and stamps
each row, refuses a change that changes nothing, a same-person exception
unless the change it names is the latest and turns separation off, and a
review by anyone overlapping the approver or preparer.

005 adds `finance_ledger_journals`, `finance_ledger_journal_lines` and
`finance_ledger_journal_events` (all append-only): the database numbers and
stamps a journal, refuses a line added after the submitting transaction and,
deferred to commit, a journal with no lines or out of balance; a journal is
decided once, and an approval names its entry and a rejection its reason.
Bilimbi's database guard against approval by the submitter is replaced by 012.

006 (a port addition, no Bilimbi counterpart) lets the event be `withdrawn`,
which the database allows only to whoever submitted the journal, and adds the
append-only `finance_ledger_journal_replacements`: a correction names the
journal it replaces, and a deferred trigger refuses the link unless the same
transaction withdrew that journal for the same submitter. The link is its own
table because the journal and event evidence field lists are frozen.

007 (a port addition) adds the append-only `finance_ledger_opening_balances`: the
source reference and exact CSV of an opening-balances submission, linked to its
ordinary approval journal, one per source reference and one per journal.

008 adds `is_group` to `finance_ledger_accounts`: a group is always active and
never retained earnings (checks), keeps its group-ness for good (the identity
trigger), a row sits only in a group of its own type (trigger on `parent_id`),
and a posting line on a group is refused (trigger on entry lines).

009 adds the append-only `finance_ledger_chart_templates`: a saved reusable chart, one
per tenant and `saved:<key>`, refused any update or delete.

012 (a port addition) adds the append-only `finance_ledger_journal_approvals`, one
durable reference from a journal to its Workflow request, and replaces the journal
event trigger: a decision now requires that reference, and no longer refuses the
submitter by itself. Workflow proves independence and records any permitted
exception once; the unique journal event stays the fence against posting twice.

## Standards addressed

A book is opened once with its currency fixed; an account keeps its code and
type because entries are posted against them, and is deactivated only once
its balance is zero (the sum of its lines); a closed period takes no entry
until it is reopened with a reason that an independent reviewer can read, and
a locked one never reopens; a late entry is reviewed by someone other than
its poster; a posted entry is corrected only by a reversal, once, and an owned
one only through its owner; the person who prepares a record does not
approve it unless the company has turned separation off, and every such
approval and every override is listed for an independent reviewer; a manual
journal posts only when its Workflow round has every required approval, with its saved independence rules and current safety floors; any past trial balance or
account total reproduces exactly from its `through_entry_number`; every
table is exported to the auditor field by field; every write is attributed
to the signed-in actor and any impersonator through the Platform's audit
capture.

`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.

Opening a book through `finance.ledger.book.open` may include `separation_required: false` and `approval_reason` for a one-person company. That choice also requires `finance.ledger.duty-separation.manage`, records the duty-separation change in the same transaction and keeps every later same-person approval by an unconverted Finance consumer in Approval exceptions. It does not relax manual journals, which follow their published Workflow rules. Omitting it leaves the approval control unchanged; independent approval is the default. The Book page asks about other Finance approvals before opening. A same-person refusal names the control and the Approval exceptions address.

`Money::display` formats a page amount with thousands separators and the book's decimal places. `Money::shown` remains plain decimal notation for editable values. `Money::parse` reads an amount as a person types or pastes it, with or without thousands commas (`1,250.00`); a comma that is not a thousands separator (`1,25`) is refused as `must be a number, like 1,250.00`, never read as a larger number. `Money::ungrouped` gives a module's own figure parser (a quantity, a unit price) the same rule.

## External accountant

The ready-made **External accountant** role reads the complete ledger and reports, exports accounting records and submits proposed adjusting journals for approval. It cannot directly post, approve, close periods, change settings or read Payroll source records. Posted payroll journal lines remain visible. See [Accountant access](accountant-access.md) for the exact boundary, installation steps, evidence and unresolved responsibility restrictions.

Two more ready-made roles use the Platform's company- and date-bound assignments: **Appointed auditor** (read-only, one company, a purpose, optional dates; **Appointed auditor: payroll evidence** adds Payroll separately) and **Sales workflow** (customers and draft invoices without ledger view; a workflow boundary, not data segregation). See [Appointed auditor and sales workflow access](appointed-auditor-and-sales-access.md) for the boundary, the open questions for the company's advisers and the running proof.

### Reuse a company's chart

Save a reusable chart from **Chart of accounts → Save reusable chart** in the current company. Give it a unique key, name and description, choose any group branches that should be optional, then review every row before publishing. For example, a practice can make its payroll group optional so an adopter without employees leaves that section out.

Publishing requires `finance.ledger.templates.manage` and ledger view access to the source company. This permission is separate from managing accounts: it shares the snapshot with ledger readers throughout the same tenant. A practice spanning separate tenants saves a template in each tenant; there is no cross-tenant chart access. The tenant owner receives the permission when roles are explicitly reconciled. Existing custom roles must be granted it deliberately.

A saved template copies active accounts and groups, with their codes, names, types, hierarchy and retained-earnings designation. It includes no balances, entries, currency, tax mappings, report sections or other module settings. Each selected optional group and its descendants forms one optional section. Nested optional roots are refused, and at least one row must remain in the core chart. All optional sections start unselected when applying. Inactive accounts are omitted; the review shows the exact snapshot. A chart must contain 1–2,000 active rows to be saved.

Saved templates are immutable and use the key `saved:<key>`. A repeated key is refused without overwriting; publish a revision under another key. Editing the source company or an adopter never changes the saved template. Companies using the same template customise their accounts independently. Applying still requires an empty chart, an exact preview and confirmation, with the existing company authorization and audited transaction. No separate accounting approval is required for chart maintenance; journals and opening balances retain their independent approval controls. Import and mapping remain conversion work under #57.

### Propose a starter chart

An operator can set **Ledger → Proposed starter chart** in Base Settings. The nullable key `finance.ledger.default_chart_template` accepts global and tenant values: use a bundled template key globally, and a bundled or `saved:<key>` template within its tenant. A tenant override takes precedence; an explicit null proposes none. An unavailable key is shown as unavailable and never causes accounts to be created.

On Chart of accounts, **Starter chart** lets a person with `finance.ledger.templates.manage` choose or clear the tenant proposal through a reviewed operation. **Opening creates no accounts.** Once the book opens, the Book page takes the person to Chart of accounts, whose empty chart offers the proposal first, alongside the other templates and the choice to start empty. Apply only after reviewing the exact rows and optional sections. Existing books are never reseeded, and changing a proposal changes no accounts.

The same contracts serve pages, REST and MCP:

| Operation | Input and result |
| --- | --- |
| `finance.ledger.chart_template.list` | `company_id`; bundled and same-tenant saved templates, plus `proposal.configured` and the full `proposal.proposed` snapshot (null if unavailable) |
| `finance.ledger.book.get` | Includes the same proposal as `starter_chart` |
| `finance.ledger.chart_template.save` | Reviewed write: `company_id`, `key` (lowercase slug, max 64), `name` (max 200), `description` (max 1,000), optional `optional_group_codes`; review `after` contains every row and section |
| `finance.ledger.chart_template.propose` | Reviewed write: `company_id`, `template` (existing template key), or `clear: true` without `template`; changes only the tenant proposal |
| `finance.ledger.chart_template.get` / `.apply` | Existing reviewed apply accepts saved keys too, with optional `sections`; existing charts are refused |

The facade equivalents are `chart_templates(scope, company_id)`, `save_chart_template`, `chart_template_proposal` and `propose_chart_template`. The former static `Ledger::chart_templates()` is now an authorized asynchronous read because saved templates belong to a tenant. Saved snapshots are captured as `finance/ledger.chart_template` changes and exported in the source company's `chart_template` evidence dataset. A new migration, `finance-ledger-009-chart-templates`, installs append-only snapshot storage; earlier migrations are unchanged.

The reusable-chart proof is part of `cargo run -p bilibi-finance-walkthrough-fixture`. For a focused disposable installation, run `cargo run -p bilibi-finance-walkthrough-fixture --bin chart-templates -- --serve` with `DATABASE_URL`, `APP_ORIGIN` and `BIND_ADDR` set. It exercises the real facades and reviewed contracts, then serves the composed app for browser validation. The bundled Malaysian chart still awaits row-by-row practitioner review (#100); saving a company's chart does not certify its accounting classifications.

### Opening operational registers

Approve and reconcile the opening journal before importing residual registers. Import each module separately: the [customer register](../receivables/README.md#opening-customer-register-at-accounting-system-conversion), [supplier register](../payables/README.md#opening-supplier-register-at-accounting-system-conversion), and [asset register](../fixed_assets/README.md#opening-asset-register-at-conversion). Each import previews its source rows and exact per-account reconciliation, then needs one approval. Imports retain original identifiers, dates and source figures and do not post another opening balance. Use the owning module's reviewed settlement operations afterward.

`Ledger::opening_context_on` returns the approved, live source opening entry and account balances summed by SQL on the caller's connection. An owner recognizes a complete account through `recognize_opening_on` in its audited transaction. An eligible asset or liability account may have a reviewed zero balance, even when the approved opening entry has no line for it; this supports offsetting invoice and credit rows. The database verifies the approved entry, account eligibility and balance. Recognition records `opening_recognition` evidence and protects the source entry against reversal. It does not change the journal's owner or erase unrelated postings from owner-based control tie-outs. An aggregate opening debit must not be registered as an ordinary individual asset acquisition.

The bounded imports still have explicit follow-ups for [broader asset estimates and history](https://github.com/BelimbingProject/bilibi-dom-finance/issues/110) and [general asset lifecycle operations for MCP](https://github.com/BelimbingProject/bilibi-dom-finance/issues/111). [Source-period schedules](#source-period-schedules-at-conversion) retain bounded evidence but do not create operational tax or payroll history. Opening bills and supplier credits resolve existing [Core-linked Finance vendors](../payables/README.md#vendors) by Core party kind and code and retain source names as evidence. These boundaries do not imply full historical transaction migration.

## Shared party controls

Use `PartyLinks` (`src/party_links.rs`) to read Core Company and Person codes and permitted facts on the caller's connection, validate Base Records references, and compose creation with a customer or supplier relationship. Finance owns terms, accounts and availability; Core owns the immutable code and identity. A Finance party is always linked at creation and that link cannot change.

Masked Core Person contacts are withheld in Finance summaries and marked restricted. A document that requires contacts refuses to freeze either a masked or withheld value; a partial display value is never an authentic document fact. The disposable `party-code` walkthrough covers masked email and phone independently, then issuing the same draft with both visible.

Use `MultiPartyChooser` and `PartyPickerContext` (`src/party_picker.rs`) for both Add customers and Add suppliers. One searchable multi-select finds Companies and People by name or Core code. It displays the fixed code, disables already linked parties, and offers creation of either kind. `PartyChooser::submit_composed` calls the owning Finance operation, which accepts `record_kind` with `record_id`, `new_company`, or `new_person`. Core creation, duplicate review for Companies, the relationship and Finance record share one transaction. Each selected row commits through that contract and retains its own refusal. No page creates a Core party first and links it in a second write.

### Shared journal approval proof

Use a fresh disposable database and the mounted walkthrough:

```sh
BILIBI_DISPOSABLE_APPROVALS=yes DATABASE_URL=… \
APP_ORIGIN=http://127.0.0.1:43171 BIND_ADDR=127.0.0.1:43171 \
cargo run -p bilibi-finance-walkthrough-fixture --bin approval-journals -- --serve
```

The proof exercises direct, impersonated and live OAuth agent self-approval refusals, an independent approval, replay with one posting, a permitted preparer exception and independent review, recall and atomic correction, a closed period, and a stale real reviewed plan. It leaves a waiting journal and an unreviewed exception for the shared waiting list and Finance page browser proof. No import is provided: Finance is under development with no users or production data. Existing development databases are disposable.
