# Bank reconciliation

Module ID: `finance/reconciliation` (Bilimbi `Bilimbi.Finance.Reconciliation`).
A company's bank accounts, each mapped one-to-one to a ledger account; the
bank statements imported for them, immutable once imported; the matches
between statement lines and the ledger's postings on that account; the
reconciliations of an account as of a statement end date; and the bank charges
and interest posted from statement lines. This version carries Bilimbi's
schema, facade and four pages: bank accounts and an account's statements
(`NC3`), and matching and one reconciliation (`NC4`). It also owns the
reviewed conversion bank bridge operations and their additions to the account
and matching pages.

Depends on `finance/ledger` (the chart a bank account maps and the book a
statement's amounts are held to) and `finance/attachments` (the bank's own
file, kept whole as the statement's source document), Core Company and Base
Tenancy. It reads the ledger only through that facade and never its tables;
other modules call this facade and never read these tables.

## Public API

Resolve the `Reconciliation` service (`finance/reconciliation.reconciliation`)
and call it with a sealed `Scope` and an explicit company id. Results are plain
records; the schema is private.

| Function | Capability | Does |
| --- | --- | --- |
| `list_bank_accounts`, `get_bank_account` | `finance.reconciliation.view` | The company's bank accounts, by name |
| `create_bank_account` | `finance.reconciliation.bank-accounts.manage` | Adds one: `name`, `ledger_account_id`, `import_mapping` (`null` leaves the columns to the first import), optional `charges_account_id`, `interest_account_id`, `match_window_days`, `active`, `reason` |
| `update_bank_account` | `finance.reconciliation.bank-accounts.manage` | Changes everything but the ledger account, with an optional `reason`; a change that changes nothing records nothing |
| `list_bank_account_changes` | `finance.reconciliation.view` | The change log: field, values before and after, actor, reason, time |
| `import_statement` | `finance.reconciliation.statements.import` | Imports a CSV file's bytes as a numbered statement with its lines, through the account's mapping (`MappingNotSet` while it has none) |
| `preview_statement` | `finance.reconciliation.statements.import` | How a file would import, storing nothing: its columns and first rows, and the first lines read through a given mapping, the account's own, or (none yet) one guessed from the file |
| `import_statement_mapped` | `finance.reconciliation.statements.import`, and `.bank-accounts.manage` to replace a different mapping | Imports through the given mapping and keeps it with the account in the same transaction, logged with the file's name as the reason |
| `list_statements`, `get_statement` | `finance.reconciliation.view` | Statements (without lines), and one with its lines in line order |
| `evidence_page` | `finance.reconciliation.view` | One ID-ordered page (`limit` 1 to 500, `after`) of a dataset for the auditor's pack, one per table: `opening_bridge`, `opening_item`, `match_opening_item`, `bank_account`, `bank_account_change`, `statement`, `statement_line`, `match`, `match_line`, `match_entry`, `match_event`, `reconciliation_record`, `reconciliation_event`, `reconciliation_item`. Dates, times and amounts are text |
| `mapping_choices` | none | The delimiters, date formats, amount signs and decimal separators a mapping is built from |
| `unmatched_items` | `finance.reconciliation.view` | What stands unmatched on each side, including current opening items; `to` bounds both sides and ordinary statement-to-entry suggestions remain separate |
| `record_match` | `finance.reconciliation.matches.manage` | Matches `statement_line_ids` with `entry_ids`, one or many on each side, with an optional `note` (Bilimbi `match`) |
| `unmatch` | `finance.reconciliation.matches.manage` | Unmatches a standing match with a needed reason |
| `get_match`, `list_matches` | `finance.reconciliation.view` | A match with its status, lines, entries, opening items and events; a bank account's matches, oldest first |
| `list_statement_lines` | `finance.reconciliation.view` | A bank account's lines by date, each with its statement number; `to` bounds the date |
| `preview_reconciliation` | `finance.reconciliation.view` | The figures as of an end date against a statement balance, optionally at pinned `through_*` numbers |
| `open_reconciliation` | `finance.reconciliation.reconciliations.manage` | Opens one: `end_date`, `statement_balance` |
| `finalise_reconciliation` | `finance.reconciliation.reconciliations.finalise` | Records the figures, numbers and items of a zero difference, by someone other than whoever opened it |
| `abandon_reconciliation` | `finance.reconciliation.reconciliations.manage` | Abandons an open one with a needed reason |
| `get_reconciliation`, `list_reconciliations` | `finance.reconciliation.view` | One with its figures (read now while open, its snapshot once finalised); a bank account's, oldest first |
| `post_statement_line` | `finance.reconciliation.statement-lines.post` | Posts an unmatched line as a `charge` or `interest` entry the line owns, matched to it |
| `reverse_statement_line` | `finance.reconciliation.statement-lines.manage` | Unmatches a posted line and reverses its entry through the owner path, with a needed reason |

Lists page by `PageOptions` (`limit` 1 to 500, 100 by default; `after`, the
last id read); anything else is `InvalidOption`.

The eight capability keys are Bilimbi's, verbatim, all granted to
`tenant_owner`: `finance.reconciliation.view`, `.bank-accounts.manage`,
`.statements.import`, `.matches.manage`, `.reconciliations.manage`,
`.reconciliations.finalise`, `.statement-lines.post` and
`.statement-lines.manage`. The module declares the verbs `import` and
`finalise`; `post` is the ledger's, `view` and `manage` are Base's. Every call
authorizes the capability for the scope's actor and the target company when it
runs; a write holds the company's row lock in an audited `MutationBoundary`
transaction, so an archived company is `CompanyArchived` and one company's
reconciliation writes run one at a time. A call that reads the chart (adding or
changing a bank account, importing, matching, reconciling) also needs
`finance.ledger.view`, because the ledger authorizes its own calls; the
import needs `finance.attachments.create` for the file, and posting or
reversing a statement line `finance.ledger.entries.post`. Inside a write the
ledger is read with its `*_on` methods on the write's own transaction (entries
through `Ledger::list_entries_on`, totals through `account_totals_on`), never on
a second pooled connection while the company lock is held.

Amounts are `Money` of the book's minor units. A stored amount is read back
with `Money::new(value, MinorUnits::new(4))` (the columns are `numeric(20,4)`),
since the ledger keeps `Money::stored` private.

Errors are `ReconciliationError` variants with Bilimbi's refusal names
(`Unauthorized`, `CompanyNotFound`, `BankAccountNotFound`,
`BankAccountInactive`, `DuplicateFile`, `DuplicateLines`, `InvalidLines`,
`MissingHeaders`, `NoLines`, `TooManyLines`, `RunningBalanceMismatch`, ...);
refused values come back as `Invalid(Vec<FieldError>)`. A refusal is decided
before the audited write, and a storage or ledger refusal is passed on as
`Attachment(..)` or `Ledger(..)`.

## Bank accounts

A bank account names an active asset account of the ledger that no other bank
account maps, and keeps it for good: every match and reconciliation reads that
account's postings. It carries the expense account bank charges post to and the
income account interest posts to (each optional until needed), the window in
days a suggested match's dates may differ by (0 to 90, default 5), and its
import mapping. Its name, counter accounts, window, mapping and active flag
change in place, and every change is appended to
`finance_reconciliation_bank_account_changes` with the field, the values
before and after, the actor, the reason (trimmed, cut at 500 characters) and
the database's time. A new account's first values are logged with no old
value. An account may be added without a mapping: its first import maps the
columns (`import_statement_mapped`) and keeps them, so later files import in
one step. A mapping is logged as JSON with its keys in order (Bilimbi logs an
Elixir `inspect` of the map). A bank account is never deleted; deactivate it,
and it imports and reconciles no more.

### Import mapping

`ImportMapping` says how the bank's CSV export is read, so a bank's layout is a
company's data, never code: the column separator, how many rows precede the
data (the last being the header row, or none), which columns hold the date,
description, optional reference, either one signed amount column (with which
sign means money in) or a debit column (money out) and a credit column (money
in), the optional running balance, the date format (nine common forms), and
the decimal separator. A column is named by its header text, or by its 1-based
position when the file has no header row. `ImportMapping::parse` builds it
from the map a company enters, filling the defaults, and refuses an unknown
setting, a missing required column, both an amount and debit or credit columns,
and a value that is not one of the choices, in Bilimbi's words. Amounts may
carry thousands separators, parentheses, a leading or trailing minus, or a
`CR` or `DR` marker.

For an account whose columns are not mapped yet, `preview_statement` guesses
a mapping from the file (`src/guess.rs`): the separator its lines share, the
rows above the first row holding a date, the usual header words of a bank's
export ("Date", "Description", "Withdrawal", "Deposit", "Balance", ...), then
the values (the first column of dates, of numbers, the widest text), the first
date format every date reads with, and a decimal comma when amounts end in
one. It only proposes; the person sees the first lines read through it.

## Statements

`import_statement` takes the file's name and bytes (and its content type,
`text/csv` unless given), reads it with the account's mapping, keeps the file
whole as the statement's source document through `finance/attachments`
(`STATEMENT_SOURCE`, `finance/reconciliation/statement`; a records-grade
document retained for the company's retention years after the end of the
financial year the statement ends in), and records one numbered statement with
its lines and the mapping it was read through in one transaction. Every check
below runs before the file is stored (the active bank account again inside the
transaction, under the company lock), and the store runs before the
transaction, as Base Artifacts asks; a refused import stores nothing, and a
storage refusal imports nothing. A statement line's `amount` is signed from the
account holder's side, money in positive and money out negative, which is the
sign of a debit less a credit on the asset account it is matched against, and
is held as `Money` in the book's minor units: a figure finer than the book's
currency is an unreadable row, not a rounded one. A row whose amount is zero is
skipped and counted in `skipped_count`.

With a running balance column, each line's balance must be the previous
line's plus its amount; a file written newest first is read in reverse, and one
that chains in neither direction is refused
(`RunningBalanceMismatch(row)`), which catches a mapping whose sign is wrong.
The statement then records its opening and closing balances. Without one, the
lines are kept in file order, oldest first (a file whose first date is after
its last is read in reverse).

The same file is refused a second time by its SHA-256
(`DuplicateFile { statement_number }`, company-wide). The same line is refused
by its fingerprint, a hash of its date, description, reference, amount, running
balance and its occurrence among identical lines of the same file, so a
statement may carry two identical movements on one day while an overlapping
export is refused whole, naming each line already on file and the statement and
line that hold it (`DuplicateLines`). Amounts are fingerprinted without
trailing zeros, so `12.5` and `12.50` are the same line (Bilimbi's text hash
tells them apart). A row the mapping cannot read refuses the file with every
faulty row and why (`InvalidLines`); a file holds at most 10,000 lines, 5 MiB
and valid UTF-8, and the reader (an RFC 4180 reader: one delimiter, quoted
fields with doubled quotes, fields spanning lines, CRLF, LF or CR line ends, a
byte-order mark) refuses a stray or unterminated quote naming the row.
Statements and lines are never changed or deleted. Once an opening bridge is
recorded for the account, an import containing any statement line before its
conversion date is refused whole; a conversion-day line is allowed.

`StatementSource` describes a statement to `finance/attachments` ("Bank
statement", `Statement <number>`, dated its end date, always final), so
`finance.reconciliation.view` is also needed to read its file; it reads the
statement on the connection it is handed.

## Matching

A match groups statement lines of one bank account with entries posted to its
ledger account whose two sides agree: the lines' amounts sum to the entries'
debits less credits on that account (an entry whose postings there cancel is
not an item). One line may match several entries and the other way round.
`record_match` refuses, in this order, an empty side or an id below 1
(`InvalidSelection`), a line of another bank account or company
(`StatementLineNotFound`), an unknown entry (`EntryNotFound`), an entry with
no posting on the account (`EntryNotOnAccount`), a line or entry already in a
standing match (`StatementLineMatched`, `EntryMatched`) and sides that differ
(`Unbalanced { statement_side, ledger_side }`); a note is trimmed, blank as
none, and at most 500 characters. A repeated id counts once. The match keeps
each entry's number, date, description, reference and amount as matched, so a
match and a finalised reconciliation read without the ledger.

Outstanding opening items use the separate reviewed match and unmatch
operations described under [Opening bank bridge](#opening-bank-bridge).
`record_match` remains the ordinary statement-to-entry path.

A match stands from its `matched` event until its `unmatched` event, which
`unmatch` appends with a needed reason (trimmed, at most 500 characters):
its lines and entries are free again and a new match is a new record. A match
that no longer stands is `MatchNotStanding`, and one a finalised
reconciliation counted is kept for good (`MatchFinalised`).

`unmatched_items` returns the unmatched lines (by date, statement and line,
each with its statement number), the unmatched entries (by date and id), the
current unmatched opening items, and ordinary statement-to-entry
`suggestions`: for each line in that order, the unmatched entry of the same
amount dated within the account's match window, preferring one whose reference
or description contains the line's reference (or whose reference the line's
description contains), compared in lower case on letters and digits only, then
the nearest date, then the lowest entry number; each entry is suggested once.

## Reconciliations

A reconciliation is a bank account's figures as of a statement end date
against the balance the statement shows that day. Its `figures` are read at
four pins, the last entry, statement, match event and opening bridge they count
(each the latest unless pinned): the book balance (the ledger account's debits
less credits through the end date, summed by PostgreSQL through
`account_totals_on`), and the items that explain the difference: each
statement line through the end date with no standing match (`statement_line`),
each entry through the end date with none (`entry`, described
`Entry <number>: <description>`), and each standing match whose two sides
through the end date differ (`timing`, `Match <number>: cleared after the
statement date`, signed book side less statement side). A current outstanding
opening item is an `opening_item`; the opening journal entry itself is omitted
because those items represent its clearing side. The `difference` is the
statement balance less the book balance, less the `statement_line` items, plus
the `entry`, `opening_item` and `timing` items; zero means the two balances are
explained. `preview_reconciliation` reads them without opening anything
(a missing end date, a balance that is not money of the book or a negative pin
is `InvalidOption`), and at a finalised reconciliation's pinned numbers it
reproduces its snapshot whatever has been posted, imported or matched since.

`open_reconciliation` needs an active bank account, an end date and a balance
no finer than the book's currency, one open reconciliation per account, and an
end date after the last finalised one (`Invalid` with Bilimbi's words beside
`end_date`, `statement_balance` and `bank_account_id`). `finalise_reconciliation`
asks `Ledger::check_approval` on its own write with the opener as preparer
(source `finance/reconciliation`, action `reconciliation_finalisation`,
subject `Reconciliation <n> of <bank account> to <end date>`): the opener, or
whoever impersonated them or was impersonated by them, is `SamePerson` while
the company separates duties, and is recorded as a same-person approval
exception while it does not. It then reads the figures on that write, refuses
a difference other than zero (`Difference(amount)`), and records the
`finalised` event with the book balance, the difference and the four pins,
and the items as they stood, numbered from 1. `abandon_reconciliation` records
`abandoned` with a needed reason. A finalised or abandoned reconciliation never
changes (`ReconciliationFinalised`, `ReconciliationAbandoned`), and
`get_reconciliation` shows an open one's figures read now, a finalised one's
stored snapshot and an abandoned one's none.

## Posting statement lines

`post_statement_line` books an unmatched line the books do not have: a
`charge` (money out, debiting the account's charges account) or `interest`
(money in, crediting its interest account), dated the line's date, with the
line's description unless a non-blank one is given and the line's reference.
The entry is posted through `Ledger::post_entry_in` on the write with the line
as its owner (`finance/reconciliation`, `statement_line:<id>`), and the line is
matched to it with the note `Posted from statement line as <kind>`, so both
stand or neither; the ledger's refusal (a closed period, an inactive account,
a missing `finance.ledger.entries.post`) records nothing. Refusals:
`InvalidKind`, `NoChargesAccount`, `NoInterestAccount`, `NotACharge`,
`NotInterest`, `StatementLineMatched`, `BankAccountInactive`, and
`StatementLinePosted` for a line that already posted an entry: the ledger holds
one original entry per owner, so such a line is matched to its entry, never
posted twice.

`reverse_statement_line` corrects such a posting: with a needed reason
(`InvalidReason` when blank or over 500 characters), the line must stand
matched, alone, to the one entry it owns (`StatementLineNotMatched`,
`StatementLineNotPosted`); the match is unmatched with the reason and the entry
reversed through `Ledger::reverse_owned_entry` on the same write, dated the
given date or today (UTC), described `Reversal of <description>`. A match a
finalised reconciliation counted is kept (`MatchFinalised`), and the ledger's
refusal rolls both back. Bilimbi's branch for entries posted before owners
were recorded is not ported: a fresh Bilibi ledger has none.

## Database

The migration `finance-reconciliation-001-reconciliation` creates
`finance_reconciliation_bank_accounts`, `_bank_account_changes`, `_statements`,
`_statement_lines`, `_matches`, `_match_lines`, `_match_entries`,
`_match_events`, `_reconciliations`, `_reconciliation_events` and
`_reconciliation_items`. Rows of one company reference each other through
composite `(id, company_id)` foreign keys; ledger account and entry ids are
plain columns. `finance-reconciliation-002-opening-bridge` adds immutable
opening bridges and items, opening-item match links, and the opening pins on
finalised reconciliations. PostgreSQL enforces the rules the facade checks, so
no other writer can break them:

- a bank account keeps its company and ledger account and is never deleted;
  one bank account per ledger account and per name;
- every other table is append-only: no UPDATE, DELETE or TRUNCATE;
- triggers number each company's statements, matches, match events,
  reconciliations and reconciliation events without gaps and stamp their times,
  replacing whatever the writer sent;
- the same file and the same line fingerprint are refused per company and per
  bank account by unique indexes; a statement's lines are inserted only in its
  own transaction, and a deferred trigger refuses a commit in which the line
  count or dates disagree (it does the work once for the statement and once
  for the line numbered last, not once per line, so a 10,000 line statement
  commits in about a second);
- an opening bridge must reconcile its statement balance, opening ledger
  balance and items and retain the approved opening entry; statement lines
  before its conversion date are refused, and a replacement names the current
  bridge and is refused after finalisation or while the account has a standing
  match;
- a match's lines, entries and optional opening item are inserted only in its
  own transaction; a line, entry or opening item in another standing match is
  refused; a deferred trigger refuses a commit in which the two sides differ,
  either side is empty, or the `matched` event is missing; an `unmatched` event
  needs a reason and a standing match that no finalised reconciliation counted;
- one open reconciliation per bank account, each ending after the last
  finalised one; a `finalised` event carries its figures and numbers and a zero
  difference; an `abandoned` one its reason; items are inserted only in the
  transaction that finalises.

A statement's `import_mapping` is NOT NULL here: a fresh Bilibi schema has no
statement imported before the mapping was kept (Bilimbi's column is nullable
for those). A bank account's is nullable (`003-mapping-on-first-import`): an
account added without one has its columns mapped on its first import.

## Bank account pages (NC3)

`/finance/reconciliation` contributes the **Bank accounts** menu item and shows the company's accounts in a FlexTable. **+ Bank account** opens a dialog asking only the name and the ledger account (a combobox of postable asset accounts no bank account has yet), then lands on the new account's page to import its first statement. A row's own actions are icon buttons (match its statement lines, edit), its Active is a `Toggle`, and its Non-local actions menu holds only its ledger account in the Ledger. Edit opens a dialog for the name, the bank charges and interest accounts (comboboxes of postable expense and income accounts), the match window and a reason; the ledger account never changes. Every write event rechecks `finance.reconciliation.bank-accounts.manage`; the facade checks it again in `Ledger::write`. Reads of the chart inside that write use `Ledger::list_accounts_on` on the handed transaction.

`/finance/reconciliation/accounts/{id}` leads with Back, **Match statement lines** once a statement is in, **Reconcile** (a dialog for the statement end date and balance, prefilled from the latest statement) or the open reconciliation, and **Import statement** (`?do=import` opens it on load). It shows statements newest first with who imported them, their bank files through the authorized Attachments download route, and reconciliations newest first. The import dialog takes the file; an account whose columns are mapped imports it at once, and one with none yet, or whose file no longer reads with the saved columns (missing headers, unreadable rows, a balance that does not follow), goes on to a columns step: the file's first rows, which column is the date, description, reference, amount (one signed column, or money out and money in) and running balance, the date format, decimal mark, separator and rows above the first line, all guessed by `preview_statement`, and the first lines as they would import, read again on every change. Its Import statement is disabled while the choices do not read the file, and saves the columns with the account (`import_statement_mapped`). A toggle in the first step checks the saved columns before importing. After the opening journal is approved, the same page reviews and records the conversion bank bridge through `finance.reconciliation.opening.import`; its retained bank statement is attached through the shared Source documents panel. The live upload accepts one CSV/text file up to 5 MiB and consumes its bytes once in the import event. A successful import reports the dates, line count and skipped zero rows. Refusals are said in the open dialog's own alert, keeping what was entered. As on every page here, a message stays until the browser dismisses it (`clear-flash`). An upload refused in the browser reads the Platform's `UploadError` sentence ("The file is larger than this page accepts.", "That kind of file is not accepted.") rather than Bilimbi's "The file is larger than the import allows." and "Choose a CSV file.", because `UploadSpec` takes no per-page wording.

The account page lists and opens reconciliations through the facade's `list_reconciliations` and `open_reconciliation` (see Reconciliations), which return `BankReconciliation` records with their typed `ReconciliationStatus`. Its matching link and the successful opening land on the NC4 pages below.

The bank account, statement and reconciliation lists all use Base UI's FlexTable and ListColumns for column arrangement and compact rows. Bilimbi enables no list filter, sort or pager on these pages, so the source ordering and facade bounds are retained. Statement reads traverse every 500-row facade page; attachment lookups use batches of at most 100 statement IDs.


Run the NC3 disposable browser proof from the mounted Platform with an empty database and `NC3_STORAGE_ROOT` naming a private task directory:

```sh
DATABASE_URL=postgres://postgres@localhost:5433/bilibi_bilibi_dom_nc3_ui \
APP_ORIGIN=http://localhost:4324 BIND_ADDR=127.0.0.1:4324 \
NC3_STORAGE_ROOT=/tmp/nc3-source-documents \
cargo run -p bilibi-finance-walkthrough-fixture --bin bilibi-finance-reconciliation-ui
```

It proves the facade's import/skipped rows, duplicate-file refusal, original download bytes, denied/restricted scopes, Money precision, and concurrent opening refusal before serving the app. The walkthrough's manager, staff and viewer accounts exercise full control, import-only and read-only states; use the fixture's `PASSWORD`. The operating account is left ready for browser imports and opening, and the reserve account has one open reconciliation. These are disposable fixture facts, never production seeds.
`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.

## Matching and reconciliation pages (NC4)

`/finance/reconciliation/accounts/{id}/matching` (Bilimbi `MatchingLive`) shows the account's statement lines with no standing match beside the ledger entries and opening items on its account with none (`unmatched_items`), each ordinary line's suggested entry with **Accept**, and the standing matches newest first with an Unmatch icon button. Every ledger entry is a link to its journal. An opening item is matched in its reviewed dialog through `finance.reconciliation.opening.match`; its unmatch uses `finance.reconciliation.opening.unmatch`. Ticking ordinary lines and entries updates the selected counts and totals; **Match selected** sends the ticked ids and the note to `record_match`, and Accept matches a suggestion the page last read, never an id of the browser's choosing. Unmatch opens a dialog for the match the page shows and sends the reason to the appropriate contract or facade. Every one of these events asks `finance.reconciliation.matches.manage` again, and the contract or facade asks once more. A bank account out of reach redirects to `/finance/reconciliation` with the refusal, as Bilimbi does.

The checkboxes sit in FlexTable rows and join the match form through `form="matching-form"`: a FlexTable carries forms of its own (the column add box), which cannot nest inside another form, and `live.js` reads a control's `form` owner, so the selection still raises the form's change event. They are Bilimbi's bare checkboxes, not `forms.html`'s, which posts `true` with a hidden `false` companion rather than an id.

`/finance/reconciliation/reconciliations/{id}` (Bilimbi `ReconciliationLive`) shows the reconciliation's figures (balance per statement, per books, the unexplained difference with its badge), the items explaining the difference, and its history with people's names. Its header leads with Back; while it is open, **Match statement lines**, the Abandon icon button (a reason in a dialog) and **Finalise**, enabled once the difference is zero. An item's **Post as bank charge** (money out) and **Post as interest** (money in) icon buttons post a statement line the books lack through `post_statement_line` (asking `finance.reconciliation.statement-lines.post` and `finance.ledger.entries.post`). A finalised reconciliation shows the figures and items it was finalised with; an abandoned one only its statement balance and history.

Both pages follow the ledger's posting notices while connected (`web::Follow`, over `Ledger::subscribe_postings`): every notice since the last read wakes the page once and it reads again through the facade; an open reconciliation follows the books, a finalised or abandoned one does not change. A page's message stays until the browser dismisses it or it times out (`clear-flash`), so a refresh from the page's own posting keeps its success message. Lists are FlexTables with ListColumns; Bilimbi enables no filter, sort or pager on these pages.

Run the NC4 disposable proof from the mounted Platform with an empty database and `NC4_STORAGE_ROOT` naming a private task directory. It drives both mounted pages through their own events and posting subscriptions (matching, suggestions, unmatching, refusals, withheld controls for a viewer, a notice waking another session, posting a charge and interest, finalising by someone other than the opener, abandoning), then serves the app with an unmatched account and an open reconciliation for the browser:

```sh
DATABASE_URL=postgres://postgres@localhost:5433/bilibi_bilibi_dom_nc4 \
APP_ORIGIN=http://127.0.0.1:4874 BIND_ADDR=127.0.0.1:4874 \
NC4_STORAGE_ROOT=/tmp/nc4-source-documents \
cargo run -p bilibi-finance-walkthrough-fixture --bin bilibi-finance-reconciliation-pages
```

Sign in as `manager@example.test` (every control), `staff@example.test` (finalise only, granted by the proof) or `viewer@example.test` (read only) with the fixture's `PASSWORD`.

Cash reports may read `Reconciliation::cash_account_ids` with `finance.ledger.view`. It returns only the mapped ledger identifiers, including inactive bank accounts; bank names, import mappings and statements still require `finance.reconciliation.view`.

Base Artifacts declares 2557 retention days (7 years) as the default of **Document retention days** (`artifacts.retention_days`), and `seed-production` saves only **Storage folder** (`artifacts.storage_root`), the server's private `data/artifacts` folder. The first import creates that default folder with 0700 permissions, so a fresh installation needs no storage settings edits. A folder an administrator chose is never created: if it is missing, the import is refused with a plain sentence asking the administrator to check the storage folder in System settings. An import blocked by a cleared policy says file storage is not set up and asks for the same. Neither message shows a setting key, and the Attachments pages and source-document panels word them the same way. Administrators can change both settings in System settings.

## Opening bank bridge

After the opening balances journal is approved, record the bank statement balance at the conversion date and the outstanding items already included in that journal on the bank account page. The reviewed `finance.reconciliation.opening.import` operation is the same door for the page and MCP. Its CSV has `source_id,item_date,description,amount`: payments are negative and deposits positive, source IDs are unique, dates are no later than conversion, amounts are nonzero at the book's precision, and the file is at most 500 rows and 128 KiB. The source reference, statement identification, original CSV, item facts and review are retained as evidence. The bridge requires `statement balance - opening bank ledger + outstanding items = 0` and does not post another ledger entry. An exact source-reference retry returns the existing bridge; changed evidence under that reference is refused.

Match an opening item to an equal statement line dated on or after conversion on the matching page or with `finance.reconciliation.opening.match`. The source item replaces the opening journal entry on the unmatched book side, so clearing does not post it again. Unmatching uses the ordinary reason and finalised-reconciliation safeguards. To correct the source before a reconciliation is finalised, unmatch the account and record a replacement bridge naming the current bridge and a reason; both versions remain evidence. Finalised reconciliations retain their opening-bridge, statement, match-event and ledger pins.

The default is a single full-amount match per source item; split clearings and changes after a finalised reconciliation are refused. An accountant should confirm whether partial clearings or post-finalisation corrections are needed before relying on those cases. Each current bridge mounts the shared Source documents panel for `finance/reconciliation/opening_bridge`; attach the source bank statement there, where the company's normal document retention applies. A correction keeps attachments on the bridge version they support and exposes a new panel for the replacement.

The disposable proof uses the conversion fixture and `bank-bridge-operations.py`. It checks both signs, mismatch, exact retry, correction, conversion-day and later clearing, retained source-statement bytes, duplicate refusal, no extra ledger entry and independent finalisation. Its reviewer is a fixture user, so accountant review of a real conversion remains outstanding as part of #107.

On a fresh disposable database, start the `conversion` fixture with `serve` and provision OAuth credentials as described in `fixtures/walkthrough/README.md`. Build `bank-bridge`, set `BANK_BRIDGE_PROOF_BIN` to its absolute path, and run `fixtures/walkthrough/bank-bridge-operations.py` with the same environment. The script uses Company 002 and runs only from an empty bridge so every claimed check executes.
