# Fixed assets

`finance/fixed_assets` mirrors Bilimbi's `Bilimbi.Finance.FixedAssets` at `4abfaf829321f8540581d833752a086b21b45ba9`: immutable categories and acquisitions (NX1), monthly depreciation, corrections, reconciled reports and evidence (NX2). The four NX3 pages compose shared Base UI components and contribute the three reference menu items. This module owns no CSS or JavaScript.

## Facade

Resolve `FixedAssets` at `finance/fixed_assets.fixed_assets`. Every data call takes a sealed `Scope` and company ID and checks company reach and capability. Writes use `Ledger::write`: capability checks before and inside the audited `MutationBoundary`, writable-company row lock, then every affected record captured as a `Change`. Actors and impersonators come only from the scope. Reads of source records use the providers' authorized `_on` facade methods on the caller's connection, so no write waits for a second pool slot while holding the company lock.

| Call | Capability | Result |
| --- | --- | --- |
| `create_category(scope, company, name, RevisionInput)` | `finance.fixed-assets.finance-asset-configure` | Category and first revision atomically; returns the revision with its category name. |
| `revise_category(scope, company, category_id, RevisionInput)` | same | Complete new revision; never a patch or rename. |
| `list_categories(scope, company)` | `finance.fixed-assets.view` | Name-ordered categories, latest revision and full ID-ordered history. |
| `acquire_from_bill(scope, company, bill_id, line_number, AcquisitionInput)` | `finance.fixed-assets.finance-asset-acquire` | Asset registered from a posted supplier bill's net debit line. Also requires Payables and Ledger view. |
| `acquire_from_entry(scope, company, entry_id, line_number, AcquisitionInput)` | same | Asset registered from a live original debit. Also requires Ledger view. |
| `get_asset(scope, company, id)` | `finance.fixed-assets.view` | Asset, original category revision, projected schedule, posted charges with their earliest reversal/void, asset events and permanent void state, all on one repeatable-read snapshot. Also requires Ledger view to read book precision. |
| `depreciate(scope, company, month)` | `finance.fixed-assets.finance-asset-depreciate` | One immutable monthly run, items and skipped-asset exceptions; one owned ledger entry unless empty. |
| `reverse_run(scope, company, run_id, EventInput)` | `finance.fixed-assets.finance-asset-reverse` | Reverse the latest live charges without reversing previously voided assets twice. |
| `dispose(scope, company, asset_id, DisposalInput)` | `finance.fixed-assets.finance-asset-dispose` | Sale or write-off, with an owned posting and immutable event. |
| `reverse_disposal(scope, company, asset_id, EventInput)` | `finance.fixed-assets.finance-asset-reverse` | Reverse a standing disposal through its ledger owner. |
| `void_acquisition(scope, company, asset_id, EventInput)` | `finance.fixed-assets.finance-asset-void` | Permanently retire a reversed acquisition and mirror only its live depreciation. |
| `list_runs(scope, company)` / `list_events(scope, company, asset_id)` | `finance.fixed-assets.view` | Runs newest month first with items, exceptions and reversal state; asset events in event-number order. |
| `register_report(scope, company, date, ReportPins)` / `depreciation_schedule_report(...)` | `finance.fixed-assets.view` | Same dated register, projected schedule and ledger reconciliation, at four immutable boundaries. |
| `evidence_page(scope, company, dataset, limit, after)` | `finance.fixed-assets.view` | Explicit frozen fields, exact decimal text, UTC timestamps; ID cursor and limit 1–500. Datasets: `opening_stage`, `opening_decision`, `opening_batch`, `opening_item`, `asset`, `category`, `category_revision`, `event`, `run`, `run_exception`, `run_item`. |

All seven reference capability keys and six `finance-asset-*` verbs are declared and granted to `tenant_owner`. Reads that need ledger context also require Ledger view; nonempty postings and reversals independently require Ledger post authority.

## Frozen defaults and acquisition

`RevisionInput` requires cost and accumulated **asset** accounts (different IDs), an **expense** account, an **income** gain account and an **expense** loss account. Each must be active and belong to the company when the revision is recorded. Expense and loss may share an account. `reason` is trimmed, nonblank and at most 500 characters. Category `name` is trimmed, nonblank, company-unique and at most 200 characters.

`Assumptions` contains `method` (`StraightLine`/`ReducingBalance`, stored as `straight_line`/`reducing_balance`), `life_months` (1–1200), and `annual_rate` (annual percentage, greater than 0 through 100, at most four decimal places). Straight line clears the rate. Reducing balance requires it.

`AcquisitionInput` takes a revision, nonblank description (maximum 500 characters), in-service date, optional residual `Money`, and optional method/life/rate overrides. Absent assumptions inherit the chosen revision; residual defaults to zero. The service checks residual against book minor units, zero and cost; it never rounds input. In-service date cannot precede acquisition, and the final schedule month must fit the date type. An asset keeps the selected revision and its copied/overridden assumptions forever; later revisions and account deactivation do not rewrite them. An old revision can still register a historical debit after its accounts are deactivated.

Both source paths derive cost, acquisition date, entry ID, entry number and line number from the ledger's answer. The line must debit the revision's cost account with zero credit, and its entry must be neither reversed nor a reversal. Bill acquisition requires a posted `bill`, uses its line number in the posted entry, and retains bill provenance. One **whole debit line** supports at most one asset across both paths: no quantity splitting, tax-inclusive recalculation or allocation of additional costs. Registering posts no entry and does not claim or change source ownership; later source reversal remains possible.

As in the reference implementation, the entry path does **not** restrict the entry's owner or infer bill provenance, despite the reference calling this the manual-journal path. It accepts any otherwise qualifying original debit. This preserves the source's behavior rather than introducing a new ownership rule.

## Month convention

- Exactly `life_months` schedule rows, including zero rows, labelled with the month's first day.
- Full charge in the in-service calendar month, even for a last-day start; no daily proration.
- Straight line spreads `cost − residual` equally over useful life.
- Reducing balance charges opening carrying value × annual percentage / 1200, not remaining depreciable value or an effective compound rate.
- Half-up round each charge at the book's minor units, cap at remaining depreciable cost, floor at zero. The final month clears the entire remaining depreciable cost for either method.
- `AssetDetail::scheduled_depreciation(as_of)` includes a month only on/after its last day. These are projected figures, not posted charges or a ledger tie-out.

Examples: cost 100.01 over two months gives 50.01 then 50.00. Cost 1200, residual 100, 24% reducing balance over three months gives 24, 23.52, then 1052.48. The large final true-up is intentional reference policy.

## Monthly runs and corrections

A new run requires a first-of-month date and an existing open fiscal period, even if no charge will post. It charges cumulative scheduled depreciation through month-end minus posted, unreversed charges: missed or reversed months catch up in a later month. A reversed acquisition becomes a persisted exception (`reason = acquisition_reversed` in the run view), not a failure of the other assets. Exceptions contain only run/asset identities in storage and evidence, matching Bilimbi.

The company lock and unique company/month serialize concurrent first creation. Retrying an existing month returns its original items and exceptions plus current reversal state before period/chronology checks; empty, skipped-only and reversed months remain consumed forever. Authorization and writable-company checks still run on every retry.

`EventInput` requires an effective date and trimmed, nonblank reason of at most 500 characters. New run/correction dates cannot precede any stored run's month-end or any event's effective date, including empty/reversed runs and old reversal events. Run reversal first refuses later unreversed runs with non-voided charges and **any standing disposal in the company**, then a date before the original month-end, then later historical activity. Reverse disposals first, then live runs newest to oldest.

Disposal requires a live acquisition, no prior standing disposal, a date on/after acquisition and exact posted depreciation through the last completed month (including the disposal month if dated month-end). Proceeds must be nonnegative book-precision `Money`; positive proceeds require an active company asset settlement account, while zero ignores it. The entry removes cost and accumulation, records proceeds and posts the gain or loss. A disposal reversal restores the asset for later catch-up.

Void requires a reversed acquisition and no standing disposal. It requires an open period even when no depreciation exists, mirrors only the asset's charges in unreversed runs with one ledger-line pair per original charge, and remains permanent. It leaves the asset visible with zero scheduled/accumulated depreciation and carrying value and excludes it from reconciliation. Whole-run reversals retain owner `run:<id>`; a mixed void/live run mirrors only live charges with owner `event:<id>`. Disposals and void mirrors also use their event owner; all records and entries commit in the same audited transaction. Ordinary mirrors require active posting accounts; full owner reversals may reverse inactive original accounts, as in the reference. Void dates are not additionally restricted by the source reversal's date.

## Reproducing and reconciling reports

Pass the same date and **all four returned pins**: `through_entry_number`, `through_asset_number`, `through_event_number` and `through_category_revision_id` (or `report.pins()`). A negative pin is refused. The register bounds acquisitions, category revisions, events and run entries independently, so a later backdated registration or entryless void cannot rewrite a reproduced figure. Lifecycle comes from bounded events, not today's ledger reversal links. Full projected schedules remain available for disposed assets.

Facade reports read on one repeatable-read connection. Operation reads use the handed connection and return the same four immutable-stream boundaries; write reviews use that report implementation while holding the company lock. The transaction is not SQL `READ ONLY` because Platform authorization records decision logs. No report writes business data. Ledger's authorized `account_totals_on` and `list_fiscal_years_on` avoid another pool checkout while the company lock is held.

Reconciliation includes accounts from every bounded category revision, including unused ones. Cost and accumulated depreciation use ordinary debit-positive ledger balances of every posting, because an acquisition debit comes from a bill or a journal. Expense, gain and loss use this module's own activity (`account_totals_by_owner`, owner `finance/fixed_assets`), excluding year-end closing entries and their reversals at the same entry pin; every other posting on such an account (sales on an income account the category also uses for disposal gains, a manual depreciation journal) is the row's `other_postings`, shown beside it and never counted as a difference. Shared accounts aggregate. Each row gives actual ledger balance, register balance and `difference = ledger − register`. Unregistered acquisitions, other postings on cost or accumulated depreciation and reversed-but-not-yet-voided acquisitions remain visible differences. This departs from the reference, which counted all activity, so an account shared with sales or cost of sales showed false differences on the register and blocked a clean conversion sign-off. Disposed assets retain depreciation expense and disposal gain/loss, but no cost/accumulated balance.

## Storage and refusals

Migration `finance-fixed-assets-001-acquisitions` creates the three NX1 tables. PostgreSQL assigns company-local asset numbers under its advisory transaction lock, stamps UTC recording time, enforces scoped composite references and unique source debits, and refuses UPDATE, DELETE and TRUNCATE permanently. No sibling SQL or cross-domain dependency is used.

Migration `finance-fixed-assets-002-runs-corrections` adds runs, run items, run exceptions and events with the same retention, scoped references and audit identity. The database numbers company events, enforces event shapes/transitions and one reversal/void, and seals children when their run's transaction commits. Deferred constraints on both run and item insertions enforce `total = sum(items.amount)`, including after an early `SET CONSTRAINTS`. This closes a reference SQL integrity gap without changing valid facade behavior. Evidence keeps frozen explicit fields and ID-ordered pagination, including nullable entry IDs for empty runs and entryless voids.

Expected refusals are `FixedAssetsError` variants (`BillNotPosted`, `BillLineNotFound`, `EntryLineNotFound`, `NotAcquisitionDebit`, `AcquisitionReversed`, reach/archive/not-found) or `Invalid(Vec<FieldError>)`. Duplicate names and sources are field refusals decided before mutation; provider failures retain their owning error. Two deliberate corrections to reference defects: blank revision reasons are refused before SQL; SQL checks require a non-null reducing-balance rate and both bill-source fields together instead of allowing nullable `CHECK` expressions to pass.

## Running proof

From the mounted Platform root, on a fresh disposable database:

```sh
DATABASE_URL=postgres://…/disposable cargo run -p bilibi-finance-walkthrough-fixture --bin fixed-assets
```

The binary migrates and installs the disposable walkthrough fixture, then proves category histories, defaults frozen after revision, book precision, source eligibility, posted-bill line provenance and ownership, straight-line ties, reducing balance with residual/final true-up, caps and zero rows, leap-month labels, historical inactive accounts, concurrent duplicates, reversal eligibility, revoked provider/owner grants, company scope, archive refusal, paged evidence, audited rows and PostgreSQL retention/uniqueness/check/composite-key refusals. It adds no production seeds and no unit tests. The repository gate remains the Platform's `scripts/domain-ci.sh`.

On a second fresh disposable database, run NX2:

```sh
DATABASE_URL=postgres://…/disposable cargo run -p bilibi-finance-walkthrough-fixture --bin fixed-assets-runs
```

This proves concurrent first-run creation/retries, posting-authority rollback, asymmetric 100/50 charges, void/partial/whole reversals and catch-up, gain/loss/break-even/write-off disposal, unrelated standing-disposal dependencies, historical reproduction, year-end closing/reopening reconciliation, skipped-only/empty consumed months, closed-period entryless void refusal, scope/archive refusals, all nine evidence datasets, audited business rows and SQL immutability/sealing/aggregate guards. It exits with `NX2: runs, corrections, reports, evidence, audit and database fences passed`.


## Pages

The view capability guards `/finance/fixed-assets/register`, `/categories`, `/runs` and `/register/{id}`. The first three share one page adapter mirroring Bilimbi `FixedAssets.Web.ListPage`, with search and URL pagination (`page_size` 25, 50 or 100). Register uses the shared period filter and shows the dated report's ledger reconciliation. “Keep this report's recorded boundary” links to the period with all four report pins; filtering, paging and period changes keep the list state and period and drop the pins, as the reference does. Categories and runs retain the reference's full facade ordering and do not acquire extra status filters or sort controls.

Every list, including the asset's projected schedule, posted charges and events, uses `FlexTable` with `ListColumns` for column arrangement and compact rows. Register links and the asset page's announcement support workspace following. Timestamps use the company/browser/UTC display policy.

The shared Record form registers a manual-entry or posted-bill debit, creates/revises category defaults, or records a monthly run. Runs show skipped acquisitions individually and keep the reference's warning after an otherwise successful run. The asset page shows its frozen assumptions, acquisition provenance, recorded actor, projected schedule, posted charges and correction history, then disposal, void and reversal forms. Void waits for the shared `ConfirmDialog`; Cancel writes nothing. The form disappears permanently after a successful void. The shared operation contracts decide all lifecycle and accounting refusals.

Each write event calls PageContracts with the live session credential and declares its contract in PageWrites with `Pending::None`. The owner checks access again inside the plan’s audited transaction. A refusal preserves the submitted draft and presents field errors; as in the reference, a form reports every bad field at once in field-name order, and an acquisition's missing source record or line comes before any other field. Missing permissions show the reference's withheld-control sentence. `record_asset` is the submit event because the Platform reserves `save` for Company inline edits.

On a third fresh disposable database, run the NX3 page proof:

```sh
DATABASE_URL=postgres://…/disposable cargo run -p bilibi-finance-walkthrough-fixture --bin fixed-assets-ui
```

It drives the actual mounted Page/LiveState adapters against facade records: create/revise category defaults, search, page and page size, acquisition and field refusals, escaping and workspace markers, run skips, disposal/reversal, void confirmation/cancel, charge correction history, grants revoked after mount, withheld controls and column/compact controls. Add `serve` with matching `APP_ORIGIN` and `BIND_ADDR` to keep that disposable app available for browser proof; the fixture manager can write and contractor can view.

`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.

Page flashes survive background refresh and table changes. The shared `clear-flash` event clears only the message kind it names, so a delayed success timeout cannot erase a newer refusal. The combined `finance-ui` walkthrough checks this through both the mounted category and asset pages and supplies records for Chrome checks across Finance.

Asset categories and disposal forms choose accounts from the ledger's account-list contract. Page money uses `Money::display` with the book's decimal places.

## Opening asset register at conversion

Import the source asset register after the opening trial balance is approved, before registering ordinary acquisitions. Open **Fixed assets → Fixed assets** (`/finance/fixed-assets/register`) for the current company, then choose **Opening asset register**. Create category revisions with the approved cost, accumulated depreciation, expense and disposal accounts first.

Use this UTF-8 CSV header:

```csv
source_id,description,category_revision_id,acquisition_date,in_service_date,cost,accumulated_depreciation,remaining_life_months,residual_value
```

The import preserves source cost, accumulated depreciation, carrying value, acquisition and in-service dates. It uses **prospective straight-line depreciation over the remaining useful life from cut-off**, regardless of the category's lifetime projection. This is an explicit opening estimate: past accumulation need not equal Bilibi's historical projection, and no historical catch-up is posted. Review the method and remaining life with the accountant. The category method is retained in source facts; the asset's frozen method is straight line and its `life_months` is remaining life at conversion.

For example, cost 12,000, accumulated depreciation 1,800, zero residual and 96 remaining months give a carrying value of 10,200 and monthly future depreciation of 106.25. The initial accumulated figure stays 1,800. It does not become a new depreciation-expense posting. A full first conversion month is recognized at month-end, including a mid-month conversion, following the existing full-month convention. Acquisition must precede conversion and in-service date must fall between acquisition and conversion. Remaining life is 0–1200 months; residual and source accumulation must be nonnegative and together no greater than cost. Zero remaining life is accepted only when carrying value equals residual value; it creates no future schedule or catch-up charge.

**Preview import** compares total cost and total accumulated depreciation by mapped account with the approved opening balances. Every difference refuses the import. Accounts cannot be used for both cost and accumulation within the imported register. **Approve import** retains the source and creates the register in one audited transaction; it posts no acquisition or depreciation entry. The reviewer must be independent of the opening source preparer, with exceptions decided and recorded through existing Ledger controls. The source opening journal is protected against reversal once records depend on it.

UI and MCP use `finance.fixed_assets.opening.context` and reviewed `finance.fixed_assets.opening.stage` and `opening.import`. Input is `company_id`, `source_reference`, `currency_code`, `csv`, and optional `override_reason`; staged batches and corrections add the fields in the [opening guide](opening-guide.md). The facade is `FixedAssets::import_opening_register`; agent commits join the plan's transaction. Normal acquisitions still freeze one whole source debit line. An opening asset has no acquisition line (`Asset::entry_line_number` is `None`); its aggregate opening entry is not an individual acquisition debit. Read opening source facts on `AssetDetail::opening` and through the `opening_batch`/`opening_item` evidence datasets.

The existing register report includes opening cost and accumulation from conversion onward, then subsequent depreciation, disposals and corrections at its normal four report pins. Depreciation-expense reconciliation counts subsequent charges separately from source accumulated depreciation. The import can retain source-period expense and YTD schedules as evidence, but it does not reconstruct them as ledger activity. Review comparative detail with the source reporting bridge. The asset page shows source IDs, source reference, carrying value and the prospective basis.

A direct import accepts 1–500 assets, at most 128 KiB. Identical source-reference/CSV retries return the retained batch; changed retries are refused. Committed corrections use independently approved staged replacements. Source impairment, disposal and estimate history, tax capital-allowance evidence and source-period expense detail can be retained separately, as described below. Additional prospective methods and post-conversion impairment or estimate-change workflows remain follow-up work pending practitioner decisions. The accountant approves source figures and future estimates; conversion does not replace the source archive.

## Opening register corrections

Stage a replacement export, then approve the complete reconciliation independently. Source batches, original records and approvals remain immutable. The [opening guide](opening-guide.md) describes batch limits, the conversion-date effect, dependent reversals and historical report pins. `fixtures/walkthrough/src/bin/conversion.rs` proves the lifecycle on a disposable composed database.

For browser/MCP parity, serve the disposable `conversion` fixture, provision credentials with `agent-operations conversion-credentials`, then run `fixtures/walkthrough/conversion-lifecycle-operations.py`. This mode uses the same independent reviewer for both doors; the default contractor fixture retains its Core fact masks.

## Agent lifecycle operations

MCP and `/api/v1` expose the same category preparation, acquisitions, depreciation and corrections as the pages. Discover `finance.fixed_assets.lifecycle` for the [step-by-step guide](lifecycle-guide.md). Every operation serves every channel and declares no step-up.

| Operation (`finance.fixed_assets.` prefix) | Inputs beyond `company_id` | Result |
| --- | --- | --- |
| `category.list` | None | Categories, current defaults and immutable revisions |
| `category.save` | Optional `category_id` to revise; otherwise `name`; complete account IDs, `method`, `life_months`, optional `annual_rate`, `reason` | New immutable revision |
| `register.get` | `as_of`; optional `source`, `status` and four report pins | Filtered register rows, projected schedules and whole-register account reconciliation |
| `schedule.get` | `as_of`; optional four report pins | Register, projected schedules and account reconciliation |
| `asset.get` | `asset_id` | Frozen assumptions, source facts, schedule, posted charges and events |
| `asset.acquire` | `source` (`entry` or `bill`), `source_id`, `line_number`, `category_revision_id`, `description`, `in_service_date`; optional residual and assumption overrides | Asset without a new acquisition posting |
| `run.list` | None | Runs newest month first, with items, exceptions and reversals |
| `run.post` | `period` (month’s first day) | Month-end run with exact catch-up charges and skipped reversed acquisitions |
| `run.reverse` | `run_id`, `effective_date`, `reason` | Owned correction event |
| `asset.dispose` | `asset_id`, `effective_date`, `reason`, decimal-string `proceeds`; `settlement_account_id` for positive proceeds | Disposal event and owned posting |
| `disposal.reverse`, `acquisition.void` | `asset_id`, `effective_date`, `reason` | Immutable correction event |

Reads return `records`, `record` or `report`; writes return `record`. Amounts are exact decimal strings and dates are ISO dates. Every write is reviewed, then committed through the shared plan. Preview validates through the handed `OperationClient`, including the ledger’s owned-posting and reversal previews. Execution uses private transaction-joining methods; public facade wrappers remain available for callers that own their transaction.

An ordered transaction-local review fence compares the complete normalized posting and source facts again immediately before each write. The plan engine previews every step before executing any, so a dependent step that changes the next step’s reviewed result refuses and rolls back the whole plan. Commit prerequisites separately and obtain a fresh review. Category revisions retain account eligibility; run reviews retain the dated register and exact posting; disposals and corrections retain their source and ledger controls. No preview allocates record IDs or posts entries. A review that finds the company row held by another write is refused as `record_changed` and writes nothing; review again.

For the live browser and MCP continuation proof, start the existing conversion fixture on a fresh database with `--bin conversion -- serve assets-lifecycle`, `BILIBI_AGENT_PROOF=disposable`, a private `BILIBI_AGENT_CREDENTIALS` path, and matching `APP_ORIGIN`/`BIND_ADDR`. Company 001 is available for browser category preparation, opening import, depreciation, disposal/reversal and reproduced reconciliation. Set `BILIBI_AGENT_URL` to the same origin and run `fixtures/walkthrough/fixed-assets-operations.py` against Company 002 using the saved OAuth credentials. Both start with approved source balances and open fiscal periods; the wire proof creates its own category through MCP.

Opening assets share the Fixed assets list and asset pages. **Source** and **Status** are columns and filters. Choose **Opening asset register** on the list to review and approve a complete or staged register through the existing opening contracts. The batch review is a dialog; there is no separate opening menu item or route. `finance.fixed_assets.register.get` accepts `source` (`ordinary` or `opening`) and `status` (`in_service`, `disposed` or `voided`). These filters select rows; reconciliation still covers the **whole register** at the same four pins. The reproduce link keeps the filters.

The shared browser, MCP and focused list-proof commands are in the [conversion walkthrough](../fixtures/walkthrough/README.md#accounting-system-conversion).

Readers can open the opening dialog to inspect retained source batches. The import form still requires the module's existing opening-write permission, and approval checks run in the shared contract.


### Opening reconciliation and cut-off

Source balances describe the position immediately before the conversion date. The first prospective charge falls at the end of the conversion calendar month. A mid-month cut-off receives a full month, without daily proration. Review the cut-off before approving the register; importing never records a historical catch-up charge.

The import review groups source cost, accumulated depreciation and carrying value by category and its cost/accumulation account pair. Account reconciliation then combines all classes sharing an account and compares that total with the approved opening trial balance. Class totals explain the source register; they are not separate ledger balances.

### Source history and tax evidence

Use the optional `source_evidence` array on the same opening import or staging action. Each record names a `source_id` in that CSV batch. The opening dialog accepts the same array as JSON. Review shows the evidence beside the register; the asset page and the `opening_item` auditor dataset retain it after approval. A changed evidence array requires a new source reference. Corrections retain the original evidence and use the existing independently approved replacement path.

```json
[
  {
    "source_id": "asset-3",
    "history": [
      {
        "kind": "impairment",
        "effective_date": "2025-12-31",
        "amount": "200.00",
        "reference": "Prior assessment",
        "note": "Source assessment, already reflected in the source balances."
      },
      {
        "kind": "residual_value_change",
        "effective_date": "2026-06-30",
        "residual_value": "100.00",
        "reference": "Estimate review",
        "note": "Source residual estimate revised."
      },
      {
        "kind": "estimate_change",
        "effective_date": "2026-06-30",
        "remaining_life_months": 96,
        "method": "Source straight line",
        "reference": "Estimate review",
        "note": "Prospective estimate at cut-off."
      }
    ],
    "source_period_expense": [
      {
        "from": "2026-01-01",
        "through": "2026-06-30",
        "amount": "600.00",
        "reference": "Source YTD depreciation schedule"
      }
    ],
    "tax_capital_allowances": [
      {
        "from": "2025-01-01",
        "through": "2025-12-31",
        "qualifying_expenditure": "12000.00",
        "allowances": "2400.00",
        "reference": "Source tax computation"
      }
    ]
  }
]
```

History kinds are `impairment`, `impairment_reversal`, `disposal`, `residual_value_change` and `estimate_change`. Every history row needs a source date, reference and note. Impairment, impairment-reversal and disposal rows need the source-reported `amount`; a residual change needs `residual_value`; an estimate change needs the source `method`, `annual_rate` (0–100%, up to four decimals), or `remaining_life_months`. Amounts are nonnegative and use book precision. Source method names are retained text, not a choice of future depreciation method. All source dates and period ends must precede conversion. Periods include both endpoints. Keep at most 200 history and schedule rows per asset, and 64 KiB of evidence per direct import or staged batch. Each batch carries only its own assets' evidence; final staged reconciliation leaves this field empty.

**These schedules are source evidence, not postings or a tax calculation.** A partial disposal can describe the source history of an asset still held, but does not dispose of its remaining registered cost. A historical residual change does not change the CSV's current residual estimate. Source-period expense and YTD bridges remain separate from subsequent depreciation activity: they are not added to future runs, do not create a catch-up charge, and do not clear a report reconciliation difference. The approved opening trial balance is the accounting source for those historical figures. Tax capital allowances remain separate from book depreciation and never determine carrying value.

### Practitioner decisions still required for issue 110

The defensible default remains prospective straight line using the explicit remaining life and residual value in the CSV. No additional opening method is enabled by this change: each needs practitioner review of its rate, final-month treatment and cut-off policy. In particular, the ordinary-acquisition reducing-balance implementation has a final-month true-up; it must not silently become an opening default.

Historical impairment and disposal evidence can be retained, but this does not implement a separate impairment contra-account, a post-conversion impairment workflow, or a register of assets fully disposed of before cut-off. The source cost and depreciation columns must describe the remaining asset and reconcile to their approved accounts; never put an impairment balance in depreciation merely to make an import pass. Separate impairment accounting, new prospective methods and later estimate changes remain part of issue 110, pending practitioner decisions. This slice must be described as **Part of #110**.


### Running the source-evidence proof

On a fresh disposable composed database, run `cargo run -p bilibi-finance-walkthrough-fixture --bin conversion -- assets-evidence`. This focused mode checks source-history, source-period and tax evidence without posting it; two asset classes sharing opening accounts; independent staged approval and changed-evidence refusals; zero-life assets; future depreciation, disposal and reversal; run reversal and catch-up; and reproduced report pins. The complete conversion mode still includes the separate receivable/payable correction and conversion-report scenarios.

For browser and MCP proof, prepare the launcher database with `--bin conversion -- prepare-launcher`, then start the Platform through `./bilibi start` at its own HTTPS address. In the disposable `fin_issue_110_rest_ui` database, `--bin conversion -- launcher-asset-credentials` restores the proof identities after launcher seeding, assigns the administrator's desk through Core User, and saves private OAuth credentials at `BILIBI_AGENT_CREDENTIALS`. It requires `BILIBI_AGENT_PROOF=disposable`. Use Company 001 for the opening dialog and Company 002 for `fixtures/walkthrough/fixed-assets-operations.py`; both import the same economic figures, including a zero-life asset.

Write previews check their capability and assigned company list before trying the company lock, then reauthorize through Core on that lock. A busy company returns the shared retryable `record_changed` refusal instead of waiting on a key-share lock. The MCP proof exercises that refusal and then resumes the same operations after the writer releases its lock.
