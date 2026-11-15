# bilibi-dom-finance

Finance is the bookkeeping Domain for Bilibi: invoices, bills, bank reconciliation, fixed assets, SST and Malaysian e-invoices, on one ledger.

It is an optional repository mounted in a [Bilibi](https://github.com/BelimbingProject/bilibi) checkout, ported from [Bilimbi Finance](https://github.com/BelimbingProject/b-dom-finance). It does not build on its own.

## Follow the money

Issue an invoice in Receivables, then record its receipt when the customer pays. Both actions post to the ledger; open the trial balance to see the resulting account balances. You do not need to post those entries by hand.

## Modules

Each module’s README explains its work, limits and detailed contract.

| Module | What it does |
| --- | --- |
| [Ledger](ledger/README.md) | Books, accounts, journals, periods and trial balance. |
| [Attachments](attachments/README.md) | Source documents, retention and legal holds. |
| [Receivables](receivables/README.md) | Customers, invoices, credit notes, receipts and aging. |
| [Payables](payables/README.md) | Suppliers, bills, payments and aged payables. |
| [Bank reconciliation](reconciliation/README.md) | Bank accounts, statement imports, matching and reconciliation. |
| [Fixed assets](fixed_assets/README.md) | Asset register, depreciation and disposals. |
| [Payroll posting](payroll/README.md) | Posting payroll runs and staff claims. |
| [SST](sst/README.md) | Malaysian sales and service tax returns. |
| [E-invoice](einvoice/README.md) | MyInvois documents, clearance and cancellation. |
| [Reports](reports/README.md) | Management reports, snapshots and auditor evidence. |

The ledger contributes agent operations through Bilibi's shared `/api/v1` and `/mcp`: reads and reviewed writes for books, accounts, account groups, starter charts, journals, reversals, fiscal closing and the trial balance. The [ledger contract](ledger/README.md#agent-operations) and [agent guide](ledger/guide.md) describe their inputs and controls. Bilimbi's ledger has no pages. This port adds them, so a person without an AI agent can do the same daily work: the book, the chart of accounts and its account groups, journals and reversals, fiscal years and periods, and the trial balance ([ledger pages](ledger/README.md#pages)). Each page calls the same operation as an agent does. Every contributed Finance operation serves every door (`channels: every`) and lists no step-up: Finance declares no Base Step-up operation, as Bilimbi has none.

## Status

Finance is in development and has no production users yet. The Malaysian starter chart and tax treatments are drafts that an accountant has not reviewed. Full conversion of existing books, strict sales-only record segregation and full statutory statements are not built. Bounded opening-balance and register imports and source-schedule retention exist; their limits are described in the module READMEs.

## Mount

Clone this repository into a Bilibi checkout at `apps/domains/finance` (a real nested checkout, never a symlink), then work from the Bilibi root:

```sh
git clone https://github.com/BelimbingProject/bilibi-dom-finance.git apps/domains/finance
python3 scripts/discover-modules.py
cargo metadata --format-version 1 > /dev/null
python3 scripts/discover-modules.py --pin
cargo build -p bilibi-web
DATABASE_URL=… ./target/debug/bilibi-web migrate
```

For an upgrade, check out the intended Finance revision in the mounted clone, rerun discovery, resolve `Cargo.lock` with `cargo metadata --format-version 1 > /dev/null`, and pin the composition before rebuilding. Back up the database, then run `migrate` against it; applied migrations are retained, and new migrations run in order. See the module READMEs for setup and conversion limits.

Bilibi discovers the mounted container from `bilibi.container.toml`; no list names it. Removing the directory removes Finance's code from the next build and keeps its data: the migration ledger keeps an unmounted module's applied versions. The composition rules are in Bilibi's `apps/domains/AGENTS.md`.

## Walkthrough fixture

`fixtures/walkthrough/` is an example-only crate (never a product dependency) that installs the Platform's walkthrough fixture and then opens Company 001's book, chart of accounts and first fiscal year through the ledger facade. Its binary is the running proof of this repository's facade:

```sh
DATABASE_URL=postgres://…/disposable BILIBI_SETTINGS_KEY=$(openssl rand -hex 32) \
  cargo run -p bilibi-finance-walkthrough-fixture
```

The settings key is an ephemeral 32-byte key for the encrypted e-invoice settings proof; never reuse a production key.

Its Receivables sections exercise setup and drafts through the real facade, followed later by issue-time snapshots and numbering races, multi-control receipts, owned reversals, inactive-control settlements, closed-period and late audit rollback, dated/pinned aging and tax boundaries, unrounded received-tax shares, competing payments/reversals, control tie-out differences, both attachment sources, all evidence exports, live grants, archival, SQL guards and complete row audits. See the [Receivables contract](receivables/README.md) for inputs and parity adaptations.

It migrates a fresh disposable database, installs the fixtures, and prints the facade's answers to accepted and refused books, accounts and account groups, the starter charts (core-only and with optional sections, repeat and conflict refusals), entries (owners, settlement accounts, concurrent posts, paged reads), reach (grants, affiliation, tenancy, archival), money finer than the book allows, the database's own refusals (edits to posted entries, unbalanced or lineless entries, cross-company keys) and the posting notices a commit, a rollback and a refusal do and do not send. The `bilibi-finance-walkthrough-ui` binary serves the composed app on a disposable database with a few kept files and two walkthrough records (one draft, one final) whose page mounts the attachments panel, so the Source documents page, downloads and panel can be tried in a browser (its header says how). Its attachments section does the same for source documents: the company's retention years, attaching and capturing files, replacing and removing on a draft record, a final record that keeps its files, retention extensions and legal holds (including a change made in Base Artifacts out of band), the retention refresh job as its system principal before and after a tenant grants it, evidence pages, restricted sources, an archived company and the database's own refusals. Its periods section, on Company 002 with dates counted back from today, opens fiscal years, closes, locks and reopens periods in and out of order, closes and reopens a year with its closing entry and reversal, turns controlled bookkeeping off and on, reviews entries recorded late, races a close against posts into the same period, and ends with the database's own closing refusals. Its controls section, on Company 001 in 2026, reverses manual entries (refused twice, for a reversal, before its original, for a closing entry), corrects an owned entry only through its owner's own transaction (rolled back and committed), submits, approves and rejects journals (refused for the submitter, for an operator who impersonated the submitter, after a period closed and an account was deactivated), turns the separation of duties off and on around sibling approvals and overrides, lists and reviews the approval exceptions (refused for the approver and preparer), races two approvals of one journal, and ends with the database's own journal and exception refusals. Its e-invoice section shows the code lists and their overrides, the company settings with their validators and encryption (it needs `BILIBI_SETTINGS_KEY`, 64 hex characters, and `openssl` for a throwaway certificate and key), the setup read with the certificate's facts, the authority's shapes and rules, drafts accepted with their derived amounts and refused in Bilimbi's words (invoices, notes and their originals, self-billed and consolidated documents), the payload a recorded document becomes (unsigned, and signed, verified and tampered with), and the database's refusals on the e-invoice tables. The e-invoice crate's `sdk_samples` example rebuilds the authority's four SDK samples byte for byte and verifies their signatures (see `einvoice/README.md`). Its totals section reads Company 001's trial balance (balanced, refused out of reach), posts a backdated entry and reproduces the earlier figures from the pinned entry number, compares an account's PostgreSQL totals with its listed lines, splits totals by owner, shows Company 002's closed year with and without its closing entries, and pages every evidence dataset. Its reports section configures Company 001's report sections (refused shapes first), reads profit and loss, the balance sheet, the cash position and the overview with comparative columns, follows a figure to the entries behind it, reproduces a report from its pinned entry number after a later post its posting subscription hears, issues and reads back immutable snapshots (refused without the capability and for an archived company), and ends with the facade's and the database's refusals.

The Payroll section proves the People-absent `unavailable` result without any entry or attempt, posting account/mapping validation and no-op histories, scope isolation, atomic owner-posting rollback, correction of explicitly historical fixture entries (including a race and a failed-date attempt), all eight evidence datasets, audited rows and the database's retention/uniqueness refusals. It does not claim People-backed recognition while that publisher is absent.

The separate NX1 walkthrough runs on its own fresh disposable database with `cargo run -p bilibi-finance-walkthrough-fixture --bin fixed-assets`. It proves immutable category history and frozen assumptions, bill-line and entry-line eligibility, exact schedules and month boundaries, duplicate registration races, grants/reach/archive refusals, evidence paging, audited rows and PostgreSQL guards. See the [Fixed Assets contract](fixed_assets/README.md). It creates no production data. Separate fixed-assets run and page fixtures are documented in that contract.

The reconciliation sections run on Company 001. They add bank accounts (refused for a liability account, an unknown or inactive account, a counter account of the wrong type, a window of 91 and twelve kinds of unreadable import mapping; changed with a reason and read back from the change log) and imports statements in four bank layouts (a signed amount with a running balance, written newest first; debit and credit columns with semicolons, decimal commas and two header rows; columns by position with no header row; month names with `CR` and `DR` markers), refusing a file twice, an overlapping export naming the lines it repeats, unreadable rows, missing headers, a running balance that does not follow, stray and open quotes, a file that is not UTF-8 or is too large, a viewer, another tenant and a deactivated account; it reads the kept file back from Attachments, races two sessions importing one file, imports a 10,000 line statement, and ends with the database's own statement, match and reconciliation refusals.

## Party codes

Customers and suppliers permanently link to a Core Company or Person at creation. Finance owns no party codes: every page, agent operation, report and document uses the fixed Core code. The shared party picker and transaction helpers live in Ledger ([party controls](ledger/README.md#shared-party-controls)).

For the Company/Person code walkthrough, mount this repository in the pinned Platform and run on an empty disposable database:

```sh
cargo run -p bilibi-finance-walkthrough-fixture --bin party-code
```

The walkthrough creates both party kinds through Core, uses the agent customer create contract, posts a consumer's TV invoice and checks its Core code in aging and customer/supplier evidence. To check the full auditor archive afterward, set `PARTY_CODE_STORAGE_ROOT` to a private disposable directory and run the same binary with `-- --evidence`. For browser proof, use the Platform launcher and its own `BILIBI_DEV_HOST`. Its startup seed retires demo accounts; the proof's `-- --restore-identities` restores only walkthrough identities after that seed, and requires this walkthrough's issued invoice before it will run.

## Naming

Optional Bilibi repositories are named `bilibi-<role>-<id>`: `dom-` for a Domain. The mount folder is the container ID in snake_case, so `bilibi-dom-finance` mounts at `apps/domains/finance`. Module crates are `bilibi-finance-<module>`, capability keys `finance.<module>.*`, tables `finance_<module>_*` and migrations `finance-<module>-NNN-<slug>`, all as Bilimbi names them.

## CI

CI is Bilibi's reference Domain workflow (`docs/domain-ci/ci.yml`): it checks out Bilibi at the revision in [`.github/bilibi-revision`](.github/bilibi-revision), mounts this repository, and runs `scripts/domain-ci.sh` over the composed workspace (discovery, committed CSS, Cargo.lock pinning, fmt, clippy with `-D warnings`, the locked release build, migrate, seed-production and a boot that loads `/`). Bump that revision to build against a newer Platform.

## License

[MIT](LICENSE).

## File length

CI warns about a Rust file over 800 lines that is new or got longer in your pull request. It fails only when the pull request body gives no reason for a listed file.

- Under `## File length` in the pull request body, add one line per listed file: `<path>: keep: <reason>` or `<path>: split: <what was split along which responsibility>`.
- Split by responsibility (one screen, one job), never by line count and never into `part1`/`part2`.
- Run `python3 scripts/file-length-ratchet.py` to see the list locally; it compares your branch with `origin/main`.
- Files under `fixtures/`, `examples/`, `migrations/` and `generated/` are skipped.
- CI runs it as `.github/workflows/file-length.yml`; editing the body re-runs it. The same script is copied unchanged from the Bilibi Platform into every Bilibi repository.
