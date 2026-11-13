# SST returns

`finance/sst` mirrors Bilimbi's `Bilimbi.Finance.Sst` at `4abfaf829321f8540581d833752a086b21b45ba9`: Malaysia's Sales and Service Tax return, form SST-02, for a company registered for sales tax, service tax, or both. NS1 owns the whole schema, the registration and its numbered versions, which SST-02 box each receivables and payables tax code reports in, the form itself, the taxable periods of the registration's cycle, and a return's figures computed from posted documents and reconciled against the tax accounts' ledger postings. NS2 owns the returns: opening, adjusting, discarding, finalising into a stored snapshot, amending, reproducing and exporting them, and the evidence datasets. NS3 owns the pages: the Returns, Return and Setup pages, the CSV download and the print page, and the two menu items (see [Pages](#pages)). The module contributes no agent operations, CSS or JavaScript.

Filing the return electronically to MySST, paying the tax, and the penalty of item 15 are out of scope, as in Bilimbi: a return is prepared, reviewed, finalised and exported here, and filed by the company.

## Sources

The form's layout and rules come from the Royal Malaysian Customs Department (RMCD) and the MySST portal, as Bilimbi read them on 13 October 2026:

- **Form**: Borang SST-02 as amended on 2 October 2026, <https://mysst.customs.gov.my/wp-content/uploads/2026/10/BORANG-SST-02-AMENDMENT-02102026.pdf>.
- **Completion guide**: *Panduan Mengisi Penyata SST-02 (Secara Manual/Pindaan)*, 31 May 2026, <https://mysst.customs.gov.my/wp-content/uploads/2026/07/Panduan-Mengisi-Penyata-SST-02-Manual-Pindaan-BM-2026-2.pdf>.
- **Guide on Return and Payment**, version 3 (13 October 2020), RMCD's specific guide for sales tax and service tax.
- **Sales Tax Act 2018** (Act 806) and **Service Tax Act 2018** (Act 807), with their 2018 regulations.
- **Rates**: the Sales Tax (Rate of Sales Tax) Order 2025 and the Service Tax (Rate of Tax) Order 2018 as amended, linked from <https://mysst.customs.gov.my/sst-orders/>.

Rates are a company's data, never code: each tax code carries its rate, and the boxes of items 11(a) to 11(d) check that the codes mapped to them carry 5%, 10%, 6% and 8%, the rates the form prints. A new rate the form gains is a new box in `src/form.rs`, reviewed like code.

## Facade

Resolve `Sst` at `finance/sst.sst`. Every data call takes a sealed `Scope` and an explicit company ID and checks the capability and the company's reach when it runs. Writes open `Ledger::write`: the capability before and inside the audited `MutationBoundary`, the company's writable row lock (an archived company is `CompanyArchived`, and one company's SST writes run one at a time), then every inserted row recorded as a `Change`. The actor and any impersonator come only from the scope. Documents and ledger figures are read only through the `Receivables`, `Payables` and `Ledger` facades with the same scope, so a call that lists tax codes or computes figures also needs `finance.receivables.view`, `finance.payables.view` and `finance.ledger.view`; those reads run before any lock is held.

| Call | Capability | Result |
| --- | --- | --- |
| `Sst::boxes(tax_type)`, `Sst::mappable_boxes(source)` | none | The form's boxes in order; the boxes a receivables or payables tax code may be mapped to, across both forms. |
| `get_registration(scope, company)` | `finance.sst.view` | The current registration (the latest version), or `None`. |
| `list_registrations(scope, company)` | `finance.sst.view` | Every version, oldest first. |
| `register(scope, company, RegistrationInput)` | `finance.sst.settings.manage` | The next version, or the current one unchanged when nothing changes. |
| `taxable_periods(scope, company, date, count)` | `finance.sst.view` | Up to `count` (1 to 24, default 6) periods of the current cycle, newest first, ending with the one containing `date` (default today, UTC), each with its due date. Empty before any registration. |
| `list_tax_codes(scope, company)` | `finance.sst.view` | Every receivables and then payables tax code with its current mapping or `None`. Also needs both modules' view. |
| `map_tax_code(scope, company, MappingInput)` | `finance.sst.settings.manage` | The code's next mapping, or its current one when the box is unchanged. Also needs the source module's view. |
| `list_mappings(scope, company, limit, after)` | `finance.sst.view` | Every mapping recorded, oldest first; `limit` 1 to 500 (default 100), `after` the last id read. |
| `preview_return(scope, company, tax_type, period_start, Pins)` | `finance.sst.view` | The `Figures` a return of that tax for the period starting `period_start` would show now, without opening one or counting any adjustment, optionally pinned. Also needs the three view capabilities. |
| `list_returns(scope, company, tax_type, limit, after)` | `finance.sst.view` | The company's returns oldest first (`TaxReturn`: fields, `status`, `version`, tax payable, `superseded_by_return_id`, events), without figures; optionally of one tax; `limit` 1 to 500 (default 100), `after` the last id read. |
| `get_return(scope, company, id)` | `finance.sst.view` | A `ReturnReport`: the return, its registration version and number, its adjustments (each `standing` or with its `withdrawal`), `period_ended`, and its figures: an open return's read now (needs the three view capabilities), a finalised one's stored snapshot (needs only this grant) with `posted_since`, a discarded one's none; an amendment's `amends`. |
| `open_return(scope, company, tax_type, period_start)` | `finance.sst.returns.sst-prepare` | The new open return, under the registration version governing `period_start`. |
| `add_adjustment(scope, company, return_id, AdjustmentInput)` | `finance.sst.returns.sst-prepare` | The `added` adjustment row. |
| `withdraw_adjustment(scope, company, adjustment_id, reason)` | `finance.sst.returns.sst-prepare` | The `withdrawn` row. |
| `discard_return(scope, company, id, reason)` | `finance.sst.returns.sst-prepare` | The discarded return. |
| `finalise_return(scope, company, id)` | `finance.sst.returns.sst-finalise` | The finalised return with its version. Also needs the three view capabilities. |
| `amend_return(scope, company, id, reason)` | `finance.sst.returns.sst-amend` | The new open return that amends `id`. |
| `reproduce_return(scope, company, id)` | `finance.sst.view` | A `Reproduction`: the figures read again at the numbers the finalised return recorded, and `reproduced`. Also needs the three view capabilities. |
| `export_return(scope, company, id, ExportFormat)` | `finance.sst.view` | An `Export` (`filename`, `content_type`, `content`): CSV, or a printable HTML document. |
| `Sst::evidence_datasets()`, `evidence_page(scope, company, dataset, limit, after)` | `finance.sst.view` | The eight dataset names; one ID-ordered `EvidencePage`. |

All five reference capability keys are declared verbatim with the module's own verbs `sst-prepare`, `sst-finalise` and `sst-amend` (`view` and `manage` are the platform's), and granted to `tenant_owner`. Preparing, finalising and amending are separate capabilities so one person can prepare a return and another finalise it.

Refusals are `SstError` variants in the words Bilimbi's pages show: `Unauthorized`, `CompanyNotFound`, `CompanyArchived`, `BookNotOpen`, `NotRegistered`, `NotRegisteredForTax`, `NotATaxablePeriod`, `InvalidTaxType` (from `TaxType::from_str`), `TaxCodeNotFound`, `InvalidSource`, `InvalidOption`, `ReturnOpen`, `ReturnFinalised` (amend it instead), `ReturnDiscarded`, `ReturnNotFound`, `ReturnNotFinalised`, `ReturnSuperseded`, `PeriodOverlaps`, `PeriodNotEnded`, `AdjustmentNotFound`, `AdjustmentWithdrawn`, `UnmappedTaxCodes(rows)`, `RateMismatch(rows)`, or `Invalid(Vec<FieldError>)` with Bilimbi's changeset messages. A sibling facade's own refusal stays its own variant (`Ledger`, `Receivables`, `Payables`), except reach and grant refusals, which read as this module's.

## Registration

A registration holds the sales tax and service tax registration numbers (at least one, each trimmed, upcased and shaped `A00-0000-00000000`: a station letter and two digits, the year and month of registration, and an eight-digit serial), the taxable period cycle (`period_start` and `period_months`, 1 to 12, 2 by default) and `service_tax_basis`. It is never changed: `register` records the next version, numbered by the database, with the actor, an optional `reason` (at most 500 characters) and the database's time, carrying over every field `RegistrationInput` leaves `None`; a blank number removes it. A call that changes none of the numbers, the cycle or the basis records nothing, even with a new reason. Each return keeps the version it was prepared under.

**Taxable periods** (`src/periods.rs`). A taxable period is two months, the first running from the date a person is registered to the end of the month after it (section 25(1) of both Acts); the Director General may approve another length (section 25(2) and (3)). The first period runs from `period_start` to the end of the month `period_months` months on, counting its own month; each later one is `period_months` whole calendar months. A return is due on the last day of the month after its period ends (section 26(1)), or 30 days after it on a varied cycle (section 26(2)); a due date on a weekend or holiday moves to the next working day in practice, which is not computed here.

**Service tax basis.** Service tax is due when payment is received (section 11(1) of the Service Tax Act), or, unpaid twelve months after the invoice date, on the day after (section 11(2)); a registrant may apply to account for it when invoiced instead (section 11(1A)). `service_tax_basis` is `payment` (the default) or `invoice`. Sales tax is always due when goods are sold, by the document date.

The cycle and the basis change only from a `period_start` after the last finalised return's period, so a reported period stays the period it was and nothing is counted under two bases (`Invalid` on `period_start`, and the database refuses the same); the numbers may change at any time. A period is read under the latest version whose cycle starts on or before it.

## Tax code mappings

Each receivables and payables tax code is mapped to the SST-02 item it reports in, or to none (deliberately not reported, such as a code for a tax other than SST). A mapping is never changed: `map_tax_code` records the next one, numbered by the database, with the actor, an optional reason and the time, and the latest for a code applies. Figures read the mappings through a mapping number, so a reading pinned at an earlier number routes rows as they were routed then. A code with tax in a period and no mapping is listed in `unmapped` and stops a return finalising; it is never silently left out.

| Source | Boxes a code may map to |
| --- | --- |
| Receivables | 11(a), 11(b), 17, 18(a), 18(b)(1) to 18(b)(3)(iii), 18(d) for sales tax; 11(c), 11(d), 18(c)(1) to 18(c)(4) for service tax |
| Payables | 19, 20, 21 (Schedule C purchases) for sales tax; 11(c), 11(d) (taxable services acquired or imported) for service tax |

## The form

`src/form.rs` holds each tax's boxes in the form's order; SST-02 is filed separately for sales tax and service tax, so each return is of one `TaxType`. A box is `Rated` (a value and its tax at the printed rate), `Specific` (item 17, any rate), `Value` (no tax), `Tax` (a tax amount only) or `Total` (items 12 and 14).

| Item | Sales tax | Service tax |
| --- | --- | --- |
| 11 | (a) goods at 5%, (b) goods at 10%: value and tax | (c) services at 6%, (d) services at 8%: value and tax; (e) Group H item 1 cards: tax |
| 12 | 11(a) + 11(b) + 17 | 11(c) + 11(d) + 11(e) |
| 13 | (a) credit notes, (b) sales tax deduction, (d) bad debt relief; 13A excess deduction claimed earlier | (a) credit notes, (c) service tax deduction, (d) bad debt relief |
| 14 | 12 − 13(a) − 13(b) − 13(d) − 13A | 12 − 13(a) − 13(c) − 13(d) |
| 17 | Second Schedule goods at specific rates: value and tax | — |
| 18 | (a) exports and Special or Designated Areas; (b)(1) to (b)(3)(iii) Schedule A, B and C exemptions; (d) section 35(3) or 61A: values | (c)(1) to (c)(4) exempted and non-taxable services: values |
| 19 to 21 | Schedule C purchases: values | — |

Item 14 follows the completion guide's formula for each tax; the form's printed formula leaves out 13(d), which the guide says is deducted once the Director General approves the relief. Items 15 and 16 (penalty), Part B1's lines per item and Part F's declaration are completed when the return is filed. Item 13A's key is `13aa`, since keys are lowercase.

## Figures

`preview_return` reads, at one ledger entry number:

- `Ledger::account_totals` over the period, which names the `through_entry_number` (or reproduces the pinned one);
- `Receivables::tax_totals` and `Payables::tax_totals` at that entry number: the tax on documents issued or posted in the period, summed by PostgreSQL per document kind and tax code;
- for service tax on the payment basis, `Receivables::received_tax_totals` at the same number instead of receivables' invoices (the codes mapped to a service tax box): the tax share of each payment received in the period before its invoice fell due under the twelve-month rule, less reversals, the share an invoice still owed when its twelve months ran out, and credit notes issued after that on tax already due.

Each row goes to the box its code maps to: an invoice or bill adds its value and tax; a credit note on an invoice of the same period, or a payables debit note, subtracts them; a credit note on an earlier period's invoice (or one on tax already due) deducts its tax in item 13(a), where the guide puts credit notes for tax already paid, except into a value-only box, which nets it in place. A row whose code maps to the other tax's form is left to that return; one mapped to none is listed with no box and counted nowhere. Each row's contribution keeps the columns' four places, each box is the exact sum of its rows rounded half up to the book's minor units, and items 12 and 14 are computed from the rounded boxes.

`Figures` carries `boxes` (each with `taxable_amount` and `tax_amount`, `None` where the box takes no such column), `tax_payable` (item 14), `sources` (every counted row with its box, mapping number and signed contribution), `unmapped`, `rate_mismatches` (rows mapped to a rated box of another rate, or to a value box with tax), the currency and minor units, and the `through_entry_number` and `through_mapping_number` it was read at. The same `Pins` read the same figures: posted documents never change, and a later posting carries a later entry number.

An open return's figures also count its standing adjustments, each added to its box.

**Ledger reconciliation.** For each tax account a code of the form (or an unmapped code) posted to in the period, `accounts` compares the account's ledger debits and credits over the period with what every document posted to it: receivables invoices credit their tax and credit notes debit it; payables bills debit theirs and debit notes credit it. The `difference`, ledger credits less debits less the documents' credits less debits, is always shown, never hidden: an entry no document explains, such as the payment of an earlier return, a manual journal, or another module's posting. Under the payment basis the return's service tax differs from the documents' by design; the reconciliation still compares documents with postings.

## Returns

`open_return` opens the return of a tax for a taxable period of the registration version that governs its first day and holds a number for that tax (`NotRegistered`, `NotRegisteredForTax`, `NotATaxablePeriod` otherwise), due on the period's statutory date. Per tax, one return of a period is open at a time (`ReturnOpen`), no period overlaps another's that was not discarded (`PeriodOverlaps`), and a finalised period takes only an amendment (`ReturnFinalised`). Returns are numbered per company without gaps.

While open, a return's figures are read now. **Adjustments** cover what documents cannot show: approved bad debt relief (item 13(d), claimed on JKDM No. 2 and entered once approved: section 36 of the Sales Tax Act, section 35 of the Service Tax Act), a sales or service tax deduction (13(b), 13(c)), goods taken for own use, the repayment of relief on a bad debt later recovered (section 37 of the Sales Tax Act, section 36 of the Service Tax Act; no item is dedicated to it, so the preparer adds it to the box the original tax was in), or cards under Group H item 1 (11(e)). An adjustment names a box the form lets a preparer adjust (`Invalid` on `box` otherwise), a value and a tax that are not negative, below 1E+16 and no finer than the book's minor units (a column the box does not take is left out or zero, and one of them must be above zero), a description (at most 500 characters) and an optional reference (at most 100); it adds to its box, and a deduction box deducts from item 14. It is withdrawn by an appended row whose description is the reason, never deleted (`AdjustmentNotFound`, `AdjustmentWithdrawn`).

`finalise_return` refuses a return whose period has not ended (`PeriodNotEnded`, against today in UTC), one with unmapped codes (`UnmappedTaxCodes`), and one with rate mismatches (`RateMismatch`). Otherwise it records, in one audited transaction, the `finalised` event with the ledger entry, mapping and adjustment numbers the figures were read at, the currency, the tax payable, the actor and the database's time; every box; every source row; and every account of the reconciliation. From then on `get_return` reads that snapshot, nothing about the return changes, and `reproduce_return` reads the figures again from the live records at the recorded numbers and says whether every box, source row and account figure equals it: posted documents never change and later ones carry later entry numbers, so they do.

A finalised return also shows `posted_since`: the boxes that documents dated in its period and posted after it would change, under the mappings and adjustments it was finalised with (`None` for an actor without the receivables, payables and ledger view grants, who still reads the snapshot). A document a period missed is never silently left out; it is reported by an amendment.

`discard_return` closes an open return with a reason instead (`Invalid` on `reason` without one); it stays on record, numbered, with its adjustments, and the period can be prepared again.

**Amendments.** A finalised return is never edited. `amend_return` opens a new return for the same tax and period that names it, with a reason (`Invalid` on `reason` without one), carrying a copy of each adjustment that stood on it (each copy naming the adjustment it copies). It is prepared and finalised as any return; its version, assigned by the database when it is finalised, follows the one it amends, the amended return shows `superseded_by_return_id`, and only the latest finalised version is amended while no return of the period is open (`ReturnNotFinalised`, `ReturnSuperseded`, `ReturnOpen`). The amendment's `amends` shows the amended version, its tax payable and every box that changed. RMCD lets a return be amended in MySST before it is paid and the due date passes, and takes tax left out afterwards as a supplement to the same period (Guide on Return and Payment, paragraphs 30 and 31); which applies is the filer's choice, and filing either is out of scope.

Once a return is finalised, `register` refuses a cycle or basis change whose `period_start` falls on or before its period's end (`Invalid` on `period_start`).

## Export

`export_return` writes the return as CSV (`ExportFormat::Csv`, `text/csv`), one row per item in the form's order with its part, item number, description, value and tax, preceded by Part A's particulars and the status (`Draft, not finalised` until finalised, then `Finalised, version N`), and followed by the entry and mapping numbers it was read at; amounts carry exactly the book's minor units, cells are quoted as RFC 4180 asks, and text a spreadsheet would read as a formula gets a leading quote. `ExportFormat::Html` (`text/html`) writes a printable document with Part A, the items, the standing adjustments and the ledger reconciliation, a draft marked as such, every text value escaped. The file is named `sst-02-<tax>-<start>-<end>-return-<number>.<csv|html>`. A discarded return is not exported (`ReturnDiscarded`). The HTML is a standalone document a company keeps or prints, with its own print styles; the download routes below serve both.

## Pages

`src/web/` mirrors Bilimbi's `Sst.Web` (`ReturnsLive`, `ReturnLive`, `SetupLive`, `ExportController`, `Format`). The pages compose Base UI's components only, draw every list with the shared `FlexTable` and built-in `ListColumns` (column settings and compact rows; Bilimbi's `<.table>` lists have no sort, filter or pager, so these have none), and ask the facade for every read and write; each write-shaped event asks its capability again before the facade asks once more, and a refusal is the facade's sentence (`SstError`'s words) as an error flash.

| Route | Access | What it shows |
| --- | --- | --- |
| `/finance/sst` (menu "SST returns" under the Tax group, `shield-check`, order 1) | `finance.sst.view` | SST setup in the header; the six latest taxable periods, each named by its months ("September to October 2026") with its due date and each registered tax's standing return (open, or the latest finalised version) or a Prepare button. A period still running says "Can be finalised from 1 Jan 2027" under its name: its return can be prepared and read, and finalised only after it ends. Until every registered tax's return of a period is finalised, its due date says how far off it is ("30 Nov 2026 · in 20 days"), as a warning from 14 days before ("Due in 5 days") and as "Overdue by 3 days" after it, counted from today in the company's zone; an open return's row in the list below says the same. "+ Return" opening a dialog that prepares the return of any period (a refusal shows inside it); every return on record, newest first (up to 500), or "No returns yet" with "+ Return". Before the registration, "Not registered yet" with Record registration, which opens SST setup's registration dialog (`?do=register`). Preparing redirects to the new return with "Return N opened.". Without `sst-prepare` the controls are withheld with "prepare SST returns". |
| `/finance/sst/returns/{id}` | `finance.sst.view` | Named by its tax and period ("Service tax return · September to October 2026"), with its number beside the status. Back, Export CSV and Print in the header; Return details (Part A's particulars, the due date counting down while the return is open) with Finalise (disabled while blocked) and Discard on an open return and Amend on the latest finalised version; what blocks finalising: a period still running says the day the return can be finalised from, and a tax code with no item is chosen there, in place, by someone with `finance.sst.settings.manage` (the same searchable choice and facade call as its row in SST setup; anyone else is told whom to ask); what documents posted since a finalised return would change; the SST-02 items (with the change from the amended version on an amendment); adjustments with "+ Adjustment" (a dialog whose item is a searchable choice) and a Withdraw icon button per row; the source rows; the ledger reconciliation; the history, each by the person's name. Discard, Amend and Withdraw ask for a reason in Bilimbi's dialog, which holds the adjustment it withdraws. The page subscribes to `Ledger::subscribe_postings` and reads the return again on a posting notice unless it is discarded. |
| `/finance/sst/setup` (menu "SST setup" under the Tax group, `cog-6-tooth`, order 2) | `finance.sst.view` | SST returns in the header; the current registration, with Record registration or Change registration opening the dialog that records the next version (`finance.sst.settings.manage`), and its versions folded under History; every receivables and payables tax code with the item it reports in, chosen in place on its row from the items its source can report in (a searchable choice, "Not reported" too), and the choices folded under History. Saving the registration answers "SST registration saved." and, while a tax code has no item, points to that next step. |
| `/finance/sst/returns/{id}/export.csv` | `finance.sst.view` (download) | `export_return` as CSV, saved as an attachment. |
| `/finance/sst/returns/{id}/print` | `finance.sst.view` (download) | `export_return` as HTML, shown inline in a new tab with its own print styles. |

The downloads read through the facade on every request: a refused grant or reach is 403, any other refusal (a discarded or missing return) 404, as Bilimbi's controller answers. Delivery headers (`no-store`, `nosniff`, no scripts) come from Base UI's `FileBody`.

**Parity notes.** Finalise is a plain button, as in Bilimbi (no confirmation). A period's Prepare button sends `tax|start` in one value because Base UI's `Button` carries one name and value. On the Setup page, a value the form cannot decode (a non-numeric month count, a malformed date) is reported as `is invalid` on its field before the facade is asked. The withdraw dialog accepts only an adjustment the page shows. People are named through the Platform's principal directory (`People`), not `user N`.

## Evidence

`evidence_page` is the auditor pack's contract (`src/evidence.rs`, as `ledger/src/evidence.rs`): one ID-ordered page (`limit` 1 to 500, default 500; `after` the last id read) of one of eight datasets, `adjustment`, `mapping`, `registration`, `return`, `return_account`, `return_box`, `return_event` and `return_source`, each one of this module's tables with Bilimbi's frozen field list. Dates are `YYYY-MM-DD`, instants ISO 8601 UTC, and amounts and rates the column's exact text. Pages are live reads: read them in one repeatable-read transaction for a consistent export.

## Database

Migration `finance-sst-001-sst` is Bilimbi's `20261015090000_create_finance_sst` end state: `finance_sst_registrations`, `_mappings`, `_returns`, `_return_events`, `_return_adjustments`, `_return_boxes`, `_return_sources` and `_return_accounts`, with nine functions and ten trigger declarations (thirty triggers). Rows of one company reference each other through composite `(id, company_id)` foreign keys; tax code and ledger account ids are plain columns, validated by their owners' facades. PostgreSQL enforces what the facade checks, so no other writer can break it:

- every table is append-only: no UPDATE, DELETE or TRUNCATE;
- triggers number each company's registration versions, mappings, returns, return events and adjustments without gaps and stamp their times, replacing whatever the writer sent, and give a `finalised` event its version, the count of the period's finalised returns of that tax;
- a registration's cycle or basis changes only from after the last finalised period;
- per tax, one open return of a period, no overlapping periods, a period's first return amends nothing and every later one amends its latest finalised return with a reason;
- a return is recorded with its `opened` event (a deferred check refuses one without); `finalised` and `discarded` need an open return, a `finalised` event carries its numbers, currency and tax payable, a deferred check refuses one without its box figures, and a `discarded` one needs a reason;
- a finalised return's boxes, sources and accounts are inserted only in the transaction that finalises it; an account row's difference must follow from its debits and credits;
- an adjustment is recorded only on an open return, and a withdrawal names an adjustment of the same return, at most once.

The numbering triggers rely on the facade's company write lock to serialize a company's writes; a concurrent writer that takes the same number fails on the unique index.

## Parity notes

- Inputs are typed (`RegistrationInput`, `MappingInput`, `TaxType`, `Pins`) instead of Bilimbi's attribute maps; `None` means "carry over" or "latest", as an absent key does in Bilimbi.
- Bilimbi reads the source module's tax codes inside the write's transaction. Here they are read through the owner's pooled facade before the write takes the company lock, as `AGENTS.md` asks for reads with no `_on` variant: tax codes are never deleted, and a code deactivated in between still reports what it carried.
- Return writes take typed arguments (`TaxType`, `Date`, `AdjustmentInput`, a `reason`) instead of attribute maps, and results are typed (`TaxReturn`, `ReturnReport`, `Adjustment`) instead of plain maps.
- Bilimbi finalises by reading the figures inside the locked transaction. The Receivables and Payables tax totals have only pooled reads, so here `finalise_return` reads the figures before taking the company lock, then checks under the lock that the return is still open and that no adjustment of it and no mapping was recorded in between; if one was, it reads again (three times at most, then `Failed(Conflict)`). A posting in between is harmless: the reading is pinned at its entry number and the snapshot records it.
- A finalised return's snapshot names the service tax basis of its registration version, which Bilimbi's leaves out; its figures are otherwise the stored rows.
- Part A's name in an export is the company's display name (its legal name when it has one), from `bilibi_company::identity`.

## Running proof

From the mounted Platform root, on a fresh disposable database:

```sh
DATABASE_URL=postgres://…/disposable cargo run -p bilibi-finance-walkthrough-fixture --bin sst
```

The binary migrates and installs the disposable walkthrough fixture and then, through the real facades only: refuses figures before any registration, registrations without a number, of another shape, with a thirteen-month cycle, an unknown basis or no first period, and registrations without the settings grant; records four versions (lowercase numbers upcased, a service tax number added with the cycle carried over, a no-op call that records nothing, the invoice basis from March, a varied monthly cycle from July) and lists their two-month and monthly periods with due dates; refuses periods that start mid-cycle or before the registration. It then sets up Receivables and Payables tax codes, issues invoices, a same-period and a prior-period credit note and a payment, posts a bill, a debit note and a manual tax-account entry; refuses mappings to an unknown source, an unknown code, a box of the wrong source and a total; records seven mappings (one deliberately not reported) and pages them; and reads the January–February sales tax figures (boxes 11(a), 11(b), 18(a), 19, items 12 and 14, an unmapped code, a not-reported code, the ledger difference of the manual entry), the service tax figures on the payment basis (the received share of a part-paid invoice plus acquired services net of a debit note), a rate mismatch and its exact reproduction at the earlier mapping number, a late posting that changes the live figures but not the pinned ones, the March–April 13(a) deduction for a prior-period credit note and service tax on the invoice basis. It finishes with grant withdrawal mid-run, another tenant, an archived company, an audit mutation for every row, and PostgreSQL's own refusals of each trigger rule, each rolled back.

Its last section (`fixtures/walkthrough/src/sst_returns.rs`, NS2) builds on those records: it refuses a return without the prepare grant, before the registration and mid-cycle; opens the January–February sales tax return and refuses a second; refuses adjustments to a total, to the other tax's box, negative, to a column the box does not take, finer than the book, of sixteen digits, of nothing, without a value or a description; adds bad debt relief, goods for own use and a deduction, withdraws the deduction (refusing a withdrawal without a reason, twice, and of a withdrawal) and reads item 14 with them. It refuses finalising without the grant, with a new unmapped code and with that code in a box of another rate; refuses the running period as not ended, discards it with a reason and prepares it again; finalises the return as version 1 and shows the snapshot equals the figures read before, refuses every change to it, reproduces it, and refuses a cycle change into its period. A document posted later leaves the snapshot and its reproduction standing and shows in `posted_since` (absent without the receivables view). An amendment, refused without a reason or the grant and while another is open, carries copies of the standing adjustments, withdraws one, shows what changed and is finalised as version 2, superseding version 1. Two sessions finalise one service tax return at once and exactly one succeeds. The CSV of the amendment and the escaped printable draft of another return follow, then a monthly cycle from June that overlaps an open May–June return until it is discarded, the paged list, all eight evidence datasets, another tenant, staff, an archived company, an audit mutation for every return row, and PostgreSQL's refusals of a changed event, a deleted adjustment, a late snapshot row, an adjustment to or a discard of a finalised return, an `opened` event recorded twice, and a withdrawal of another return's adjustment or of one already withdrawn.

**Pages (NS3).** On the same database, `cargo run -p bilibi-finance-walkthrough-fixture --bin sst-ui` proves the mounted page states (a reader sees no prepare control and a forged prepare opens nothing; a finalised return shows Amend to the manager only; an amendment without a reason is refused and keeps its dialog) and the downloads (CSV attachment, inline print page, 403 for an account outside the company, 404 for a discarded return), then serves the app at `BIND_ADDR`/`APP_ORIGIN` (default `0.0.0.0:4316`). Sign in as `manager@example.test` (every SST capability) or `viewer@example.test` (views only) with the walkthrough password. `sst-ui deny <capability>` and `restore <capability>` change one of the manager's grants mid-page; `sst-ui post <date>` posts a manual sales tax entry that an open return's page follows.

It adds no production seeds and no unit tests; the repository gate remains the Platform's `scripts/domain-ci.sh`.

`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.
