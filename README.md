# bilibi-dom-finance

Finance Domain for [Bilibi](https://github.com/BelimbingProject/bilibi): a
company's bookkeeping, ported from Bilimbi's
[b-dom-finance](https://github.com/BelimbingProject/b-dom-finance). It is one
optional, independently versioned repository that an adopter mounts into a
Bilibi checkout; it does not build on its own.

| Module | Descriptor ID | Owns |
| --- | --- | --- |
| [`ledger/`](ledger/README.md) | `finance/ledger` | each company's book and currency, its chart of accounts (accounts that take postings and account groups that total what is under them, with starter charts a person can preview and apply to an empty chart; the bundled chart awaits accountant review), its journal entries (append-only, numbered without gaps, balanced at commit, optionally owned by another module's record) with posting notices that announce only committed entries, its fiscal years of monthly periods with the closing log (close, reopen, lock, year-end close and reopen with closing entries), controlled bookkeeping and its changes, the late-recording report with its reviews, reversals (an owned entry only through its owner), the separation of duties (impersonation counted) with the approval exceptions log and its independent reviews, manual journals that post only after approval (their submitter may withdraw or correct one that is still waiting), the trial balance and account totals (also by owner) summed by PostgreSQL at a pinned entry, and the evidence datasets of the auditor's pack. |
| [`attachments/`](attachments/README.md) | `finance/attachments` | source documents kept with the records they support: records-grade files in Base Artifacts, the company's retention years and their history, replace and remove while a record is a draft, legal holds, and the retention refresh job that follows a record's final date. The `Source` contract each record owner implements is here, and so are the Source documents page, its download route and the panel a record's page embeds. |
| [`einvoice/`](einvoice/README.md) | `finance/einvoice` | Fiscal documents, MyInvois clearance and cancellation, owned ledger posting and reversals, consolidation, PDF, evidence, and documents, reconciliation and setup pages. |
| [`payables/`](payables/README.md) | `finance/payables` | settings and tax codes with change histories; immutable bills, debit notes and source-backed opening supplier credits; independent approval, void, and atomic owned ledger posting; source documents. Payment requests, credit applications and refunds, independent payment approval/rejection, owned reversals, aged balances, tax totals, control-account tie-out and evidence. The three-way match (held bills, match events and exceptions, `approve_match_exception`, `billed_quantities`) against its own copy of Supply Chain's order lines and goods receipts, kept from Supply Chain's domain events and unavailable while Supply Chain is not mounted. Pages: bills (NP4), then recording a bill or debit note, payments, aged payables and setup (NP5). |
| [`payroll/`](payroll/README.md) | `finance/payroll` | Payroll and claims posting, account mappings, corrections and evidence. It keeps a copy of People events; source posting is unavailable when the required publishers are not mounted. |
| [`receivables/`](receivables/README.md) | `finance/receivables` | setup, series, tax codes and customers; invoice/credit-note drafts and numbered issuance with frozen party and owned posting; source-backed opening customer credits, their applications and refunds; fully allocated receipts and owned reversals; dated/pinned aging, tax and received-tax totals, control-account tie-out, attachment sources and evidence datasets. Its pages (customers, invoices, receipts, aging, settings) call the same operation contracts as the agent doors; Bilimbi has none. |
| [`fixed_assets/`](fixed_assets/README.md) | `finance/fixed_assets` | Categories, acquisitions and opening registers; depreciation runs, corrections, disposals, reconciled reports, evidence and pages. |
| [`reports/`](reports/README.md) | `finance/reports` | a company's management reports read through the ledger's facade: profit and loss, the balance sheet and the cash position with up to four comparative columns, the overview against the previous period, the entries behind any figure, all at one pinned entry number; retained source-period schedules; the `finance.reports.sections` layout setting with its validator; the ledger's posting notices for live reports; immutable issued snapshots; and the opening-conversion reconciliation with accountant sign-off. Its pages (Finance setup, source-period schedules, the management reports, report sections, bookkeeping controls, approval exceptions and conversion reports) and the auditor pack are built; `finance.reports.setup.get` reads which Finance areas a company has set up. |
| [`reconciliation/`](reconciliation/README.md) | `finance/reconciliation` | Bank accounts, statement import, matching, reconciliations and the conversion bank bridge, with reviewed pages and operations, source documents and evidence. |
| [`sst/`](sst/README.md) | `finance/sst` | Sales and service tax setup, return preparation, finalisation, amendments, exports and pages. Filing remains outside the module. |

The ledger contributes agent operations through Bilibi's shared `/api/v1` and `/mcp`: reads and reviewed writes for books, accounts, account groups, starter charts, journals, reversals, fiscal closing and the trial balance. The [ledger contract](ledger/README.md#agent-operations) and [agent guide](ledger/guide.md) describe their inputs and controls. Bilimbi's ledger has no pages. This port adds them, so a person without an AI agent can do the same daily work: the book, the chart of accounts and its account groups, journals and reversals, fiscal years and periods, and the trial balance ([ledger pages](ledger/README.md#pages)). Each page calls the same operation as an agent does. Every contributed Finance operation serves every door (`channels: every`) and lists no step-up: Finance declares no Base Step-up operation, as Bilimbi has none.

## Status

Finance is in development and has no production users yet. The Malaysian starter chart and tax treatments are drafts that an accountant has not reviewed. Full conversion of existing books, strict sales-only record segregation and full statutory statements are not built. Bounded opening-balance and register imports and source-schedule retention exist; their limits are described in the module READMEs.

## Mount

Clone this repository into a Bilibi checkout at `apps/domains/finance` (a real
nested checkout, never a symlink), then work from the Bilibi root:

```sh
git clone https://github.com/BelimbingProject/bilibi-dom-finance.git apps/domains/finance
python3 scripts/discover-modules.py
cargo metadata --format-version 1 > /dev/null
python3 scripts/discover-modules.py --pin
cargo build -p bilibi-web
DATABASE_URL=… ./target/debug/bilibi-web migrate
```

For an upgrade, check out the intended Finance revision in the mounted clone, rerun discovery, resolve `Cargo.lock` with `cargo metadata --format-version 1 > /dev/null`, and pin the composition before rebuilding. Back up the database, then run `migrate` against it; applied migrations are retained, and new migrations run in order. See the module READMEs for setup and conversion limits.

Bilibi discovers the mounted container from `bilibi.container.toml`; no list
names it. Removing the directory removes Finance's code from the next build and
keeps its data: the migration ledger keeps an unmounted module's applied
versions. The composition rules are in Bilibi's `apps/domains/AGENTS.md`.

## Walkthrough fixture

`fixtures/walkthrough/` is an example-only crate (never a product dependency)
that installs the Platform's walkthrough fixture and then opens Company 001's
book, chart of accounts and first fiscal year through the ledger facade. Its binary is the
running proof of this repository's facade:

```sh
DATABASE_URL=postgres://…/disposable BILIBI_SETTINGS_KEY=$(openssl rand -hex 32) \
  cargo run -p bilibi-finance-walkthrough-fixture
```

The settings key is an ephemeral 32-byte key for the encrypted e-invoice settings proof; never reuse a production key.

Its Receivables sections exercise setup and drafts through the real facade, followed later by issue-time snapshots and numbering races, multi-control receipts, owned reversals, inactive-control settlements, closed-period and late audit rollback, dated/pinned aging and tax boundaries, unrounded received-tax shares, competing payments/reversals, control tie-out differences, both attachment sources, all evidence exports, live grants, archival, SQL guards and complete row audits. See the [Receivables contract](receivables/README.md) for inputs and parity adaptations.

It migrates a fresh disposable database, installs the fixtures, and prints
the facade's answers to accepted and refused books, accounts and account groups, the starter charts (core-only and with optional sections, repeat and conflict refusals), entries (owners,
settlement accounts, concurrent posts, paged reads), reach (grants,
affiliation, tenancy, archival), money finer than the book allows, the
database's own refusals (edits to posted entries, unbalanced or lineless
entries, cross-company keys) and the posting notices a commit, a rollback and
a refusal do and do not send. The `bilibi-finance-walkthrough-ui` binary serves the composed app on a
disposable database with a few kept files and two walkthrough records (one
draft, one final) whose page mounts the attachments panel, so the Source
documents page, downloads and panel can be tried in a browser (its header
says how). Its attachments section does the same for
source documents: the company's retention years, attaching and capturing
files, replacing and removing on a draft record, a final record that keeps its
files, retention extensions and legal holds (including a change made in Base
Artifacts out of band), the retention refresh job as its system principal
before and after a tenant grants it, evidence pages, restricted sources, an
archived company and the database's own refusals. Its periods section, on
Company 002 with dates counted back from today, opens fiscal years, closes,
locks and reopens periods in and out of order, closes and reopens a year with
its closing entry and reversal, turns controlled bookkeeping off and on,
reviews entries recorded late, races a close against posts into the same
period, and ends with the database's own closing refusals. Its controls section, on
Company 001 in 2026, reverses manual entries (refused twice, for a reversal,
before its original, for a closing entry), corrects an owned entry only through
its owner's own transaction (rolled back and committed), submits, approves and
rejects journals (refused for the submitter, for an operator who impersonated
the submitter, after a period closed and an account was deactivated), turns
the separation of duties off and on around sibling approvals and overrides,
lists and reviews the approval exceptions (refused for the approver and
preparer), races two approvals of one journal, and ends with the database's
own journal and exception refusals. Its e-invoice section shows the code lists and
their overrides, the company settings with their validators and encryption
(it needs `BILIBI_SETTINGS_KEY`, 64 hex characters, and `openssl` for a
throwaway certificate and key), the setup read with the certificate's facts,
the authority's shapes and rules, drafts accepted with their derived amounts
and refused in Bilimbi's words (invoices, notes and their originals,
self-billed and consolidated documents), the payload a recorded document
becomes (unsigned, and signed, verified and tampered with), and the
database's refusals on the e-invoice tables. The e-invoice crate's
`sdk_samples` example rebuilds the authority's four SDK samples byte for byte
and verifies their signatures (see `einvoice/README.md`).
Its totals section reads Company 001's
trial balance (balanced, refused out of reach), posts a backdated entry and
reproduces the earlier figures from the pinned entry number, compares an
account's PostgreSQL totals with its listed lines, splits totals by owner,
shows Company 002's closed year with and without its closing entries, and
pages every evidence dataset. Its reports section configures Company
001's report sections (refused shapes first), reads profit and loss, the
balance sheet, the cash position and the overview with comparative columns,
follows a figure to the entries behind it, reproduces a report from its pinned
entry number after a later post its posting subscription hears, issues and
reads back immutable snapshots (refused without the capability and for an
archived company), and ends with the facade's and the database's refusals.

The Payroll section proves the People-absent `unavailable` result without any entry or attempt, posting account/mapping validation and no-op histories, scope isolation, atomic owner-posting rollback, correction of explicitly historical fixture entries (including a race and a failed-date attempt), all eight evidence datasets, audited rows and the database's retention/uniqueness refusals. It does not claim People-backed recognition while that publisher is absent.

The separate NX1 walkthrough runs on its own fresh disposable database with `cargo run -p bilibi-finance-walkthrough-fixture --bin fixed-assets`. It proves immutable category history and frozen assumptions, bill-line and entry-line eligibility, exact schedules and month boundaries, duplicate registration races, grants/reach/archive refusals, evidence paging, audited rows and PostgreSQL guards. See the [Fixed Assets contract](fixed_assets/README.md). It creates no production data. Separate fixed-assets run and page fixtures are documented in that contract.

The reconciliation sections run on Company 001. They add bank accounts (refused for a liability account, an
unknown or inactive account, a counter account of the wrong type, a window of
91 and twelve kinds of unreadable import mapping; changed with a reason and
read back from the change log) and imports statements in four bank layouts
(a signed amount with a running balance, written newest first; debit and credit
columns with semicolons, decimal commas and two header rows; columns by
position with no header row; month names with `CR` and `DR` markers), refusing
a file twice, an overlapping export naming the lines it repeats, unreadable
rows, missing headers, a running balance that does not follow, stray and open
quotes, a file that is not UTF-8 or is too large, a viewer, another tenant and
a deactivated account; it reads the kept file back from Attachments, races two
sessions importing one file, imports a 10,000 line statement, and ends with the
database's own statement, match and reconciliation refusals.

## Party codes

Customers and suppliers permanently link to a Core Company or Person at creation. Finance owns no party codes: every page, agent operation, report and document uses the fixed Core code. The shared party picker and transaction helpers live in Ledger ([party controls](ledger/README.md#shared-party-controls)).

For the Company/Person code walkthrough, mount this repository in the pinned
Platform and run on an empty disposable database:

```sh
cargo run -p bilibi-finance-walkthrough-fixture --bin party-code
```

The walkthrough creates both party kinds through Core, uses the agent customer
create contract, posts a consumer's TV invoice and checks its Core code in aging
and customer/supplier evidence. To check the full auditor archive afterward,
set `PARTY_CODE_STORAGE_ROOT` to a private disposable directory and run the same
binary with `-- --evidence`. For browser proof, use the Platform launcher and
its own `BILIBI_DEV_HOST`. Its startup seed retires demo accounts; the proof's
`-- --restore-identities` restores only walkthrough identities after that seed,
and requires this walkthrough's issued invoice before it will run.

## Naming

Optional Bilibi repositories are named `bilibi-<role>-<id>`: `dom-` for a
Domain. The mount folder is the container ID in snake_case, so
`bilibi-dom-finance` mounts at `apps/domains/finance`. Module crates are
`bilibi-finance-<module>`, capability keys `finance.<module>.*`, tables
`finance_<module>_*` and migrations `finance-<module>-NNN-<slug>`, all as
Bilimbi names them.

## CI

CI is Bilibi's reference Domain workflow (`docs/domain-ci/ci.yml`): it checks
out Bilibi at the revision in [`.github/bilibi-revision`](.github/bilibi-revision),
mounts this repository, and runs `scripts/domain-ci.sh` over the composed
workspace (discovery, committed CSS, Cargo.lock pinning, fmt, clippy with
`-D warnings`, the locked release build, migrate, seed-production and a boot
that loads `/`). Bump that revision to build against a newer Platform.

## License

[MIT](LICENSE).

## File length

A pull request may not add a Rust file over 800 lines, and may not make a file that is already over 800 lines longer. Split it or keep it from growing.

- Run `python3 scripts/file-length-ratchet.py` before you push; it compares your branch with `origin/main`.
- Files under `fixtures/`, `examples/`, `migrations/` and `generated/` are skipped.
- CI runs it as `.github/workflows/file-length.yml`. The same script is copied unchanged from the Bilibi Platform into every Bilibi repository.
