# Payroll posting

`finance/payroll` recognises finalised payroll runs and approved staff claims in the ledger. It mirrors Bilimbi `Bilimbi.Finance.Payroll` at `4abfaf829321f8540581d833752a086b21b45ba9`. It does not calculate payroll, submit statutory returns, pay salaries or settle reimbursements.

## Pages

`/finance/payroll` (“Payroll and claims”) shows approved runs and claims copied from People events, together with their posting standing and reversal reasons. Without People it shows the existing absent-source notice and local postings. `/finance/payroll/setup` (“Payroll posting accounts”) shows the company's posting accounts, component/category mappings and their two change histories. Both routes require `finance.payroll.view`; their menu item is “Payroll and claims” in the Payroll group under Finance. Setup is linked from Postings, not a second menu item.

All lists use shared FlexTable/ListColumns on ListState, with independent column arrangement and row-height controls. Bilimbi enables no sort, filter or pager here, so these pages add none. Accounts are searchable choices, name first with the code muted, offering only active accounts of the required types; retained records still label inactive accounts, and a reader without Ledger view sees "Account unavailable". The two accounts owed to staff are chosen in place; before the setup the first waits on the page until the other is chosen, then both save together. "+ Mapping" opens a dialog for a new component or claim category that offers only the accounts its direction posts to (claim categories always save direction `claim`); its kind, code and direction are fixed once saved, and its accounts are changed in place on its row. Refusals show beside their field (a dialog keeps its draft); typing never writes. The change histories fold under History and name each person. Every write-shaped event reauthorizes before calling the facade.

Post, Retry posting and Reverse are icon buttons on their rows; every posting links its journal (and its reversal's). Reverse opens a confirmation shaped as the shared one (consequence first, Cancel beside the danger verb) with the reason and optional date as its body; its verb submits them, and confirming consumes the server-held posting ID, reason and validated date exactly once. Cancel, a stale confirmation or a revoked grant makes no change. Only a blank date selects the company's today. Source lists and posting controls appear while People declares all three source events. Before the first delivery the lists are empty. Unmounting People hides the source lists; existing postings and their corrections remain available.

## People events and availability

Finance keeps its own copy of three documented People events, all version 1:

| Event | Copied facts |
| --- | --- |
| `people/payroll.run_approved` | Company, run ID, reference, currency, pay date, exact components by code and direction |
| `people/claims.request_approved` | Company, claim ID, reference, currency, exact approved amount, approval date, category code |
| `people/claims.request_reimbursed` | The same approval facts, plus reimbursement standing |

`source.rs` parses these contracts into private Finance types. Migration `finance-payroll-002-people-source` creates `finance_payroll_source_runs`, `finance_payroll_source_components` and `finance_payroll_source_claims`. Exact amounts stay decimal text until the posting code rounds them to the book's minor units. Delivery locks the company and commits every changed projection row with the Base Events receipt and audit. A reimbursement retains the original approval facts and cannot make a posted claim eligible for another entry.

There is no People Cargo dependency, descriptor edge, port call or table read. `people_available()` checks the mounted event declarations. When any required publisher is absent, authorized source lists and posting calls return `PeopleUnavailable` (“The payroll and claims source is not installed.”), without attempts or entries. Local settings, mappings, logs, evidence and correction still work. Remounting restores the source lists from the retained copy.

The tenant administrator must grant the `domain-events` principal `events.delivery.deliver`. For events recorded before this subscriber was mounted, use the Events page to replay `finance/payroll.people`; nothing replays on boot. See the Platform's `apps/base/events/README.md` for delivery, retry and replay rules. The People `payroll/README.md` and `claims/README.md` define the wire contracts.

## Facade and authorization

Resolve `Payroll` at `finance/payroll.payroll`. Every company-scoped call takes a sealed `Scope` and authorizes through Core Company. Actor and impersonator are derived only from that scope. The three verbatim capabilities are granted to `tenant_owner`:

| Capability | Operations |
| --- | --- |
| `finance.payroll.view` | `get_settings`, `list_mappings`, settings/mapping change logs, source lists, `list_postings`, `get_posting`, `list_posting_events`, `evidence_page` |
| `finance.payroll.settings.manage` | `put_settings`, `put_mapping` |
| `finance.payroll.postings.post` | `post_payroll_run`, `post_claim`, `post_claims`, `reverse_posting` |

Writes open `Ledger::write`, recheck the owner's capability inside the audited transaction and hold the writable-company lock. Ledger independently authorizes its reads/posting/reversal. An archived company cannot change. Every affected Payroll row is a `Change`; settings and mapping histories commit atomically with their current values.

## Posting accounts and mappings

`SettingsChange` accepts optional `net_pay_account_id`, `claims_payable_account_id` and `reason`. Both accounts are required initially, active liabilities of this company; omitted values retain existing settings. `MappingChange` upserts by the trimmed, case-sensitive `(kind, code, direction)` key. Code is 1–60 characters. Keys never change; no delete exists.

| Kind | Direction | Expense-side account | Liability-side account |
| --- | --- | --- | --- |
| `component` | `earning` | required asset or expense | absent |
| `component` | `deduction` | absent | required liability |
| `component` | `employer` | required asset or expense | required liability |
| `claim_category` | `claim` | required asset or expense | absent |

All accounts must be active and company-owned at configuration time. Optional reasons trim blank to null and allow at most 500 characters. A reason alone or unchanged account values create no history. Histories preserve old/new account IDs, actor, impersonator and database-clock time. Settings and mappings remain retained; their identities and all history/event rows are protected from change/delete/truncate.

## Accounting contract

Runs aggregate components by code/direction, resolve every mapping even for a rounded-zero amount, then round each aggregate half away from zero to the book's minor units and construct `Money`. Earnings debit expense-side accounts; deductions credit liabilities; employer amounts do both. Net pay is rounded earnings minus rounded deductions: credit the net-pay liability when positive, debit when negative, omit when zero. Employer contributions never affect net pay. Negative components are not flipped to another side; Ledger refuses invalid lines. A zero run is refused.

Claims round the approved amount the same way, debit the category account and credit claims payable. Zero/negative rounded amounts, missing category/date, missing mappings and currency disagreement are refused. There is no currency conversion. Runs use pay date. Claims use the approval date recorded by People (the UTC approval date in v1). Entry total is sum of debits, not net pay.

Entries are owned by `finance/payroll` with `payroll_run:<id>` or `claim:<id>`. `Ledger::post_entry_in` and the Payroll `posted` event share one transaction. Company locking and database unique indexes allow one original per source, including after reversal. A batch takes one source snapshot, preserves request order and posts each claim in its own transaction, returning individual outcomes; source/authorization refusal fails the whole batch before attempts.

`get_posting` takes the **posted-event ID**, not the ledger entry ID. `reverse_posting` needs a trimmed, nonblank reason of at most 500 characters. An omitted date is company-local today from Base DateTime's `localization.timezone`. It calls `Ledger::reverse_owned_entry` in the transaction recording `reversed`; Ledger enforces dates/periods and one mirror entry. Source availability is irrelevant. Originals and all failures remain retained; a later successful post/reversal clears the displayed latest failure.

Preflight and duplicate refusals create no attempt. Other accounting refusals roll back before a fresh authorized write records `posting_failed`/`reversal_failed`; this is best effort if the company became unwritable or the database cannot be reached. Structured transient SQL states (lock unavailable, serialization, deadlock, cancellation), I/O and pool failures retry once in fresh transactions, reconciling durable success before retry/failure after a potentially lost commit acknowledgement. Nontransient infrastructure errors remain errors. The pinned Platform hides the underlying driver's closed-connection flag; a connection failure with no exposed cause cannot be classified precisely and is returned rather than guessed retryable.

## Evidence and bounds

`evidence_datasets()` lists `mapping`, `mapping_change`, `posting_event`, `settings`, `settings_change`, `source_run`, `source_component`, `source_claim`. `evidence_page(scope, company, dataset, limit, after)` exposes explicit field order, JSON rows and next cursor; IDs ascend, `limit` is 1–500 (default 500), `after` is nonnegative (default zero). A full final page returns its last ID, so the caller may need an empty follow-up. Exact decimal totals and ISO dates/times are strings. Each call is a live authorized read, not a pinned multi-page snapshot. Change/event convenience lists traverse those pages in ascending ID order. The unpaged Postings page returns all originals with standing, newest first, so old originals remain reachable for correction. Mappings are ordered by kind/code/direction.

## Running proof

On an empty disposable PostgreSQL 18 database, from the mounted Platform root:

```sh
DATABASE_URL=postgres://…/disposable BILIBI_SETTINGS_KEY=$(openssl rand -hex 32) \
  cargo run -p bilibi-finance-walkthrough-fixture
```

The Payroll section asserts configuration/no-op histories, account roles, isolation and withheld writes, source `unavailable` without attempts when People is absent, empty source lists before delivery when People is mounted, Ledger owner rollback and independent grants, an explicitly labelled historical posting's reasoned reversal, earlier-date failure and retry, concurrent reversal, mirrored lines, all evidence cursors, audited affected rows, and PostgreSQL retention/uniqueness/event-shape refusals. No unit tests, provider stubs or production seeds.

Its page scenarios run through the registered PageService: accounts chosen in place (alone once set up), the mapping dialog's field refusals and direction-bound accounts, row accounts changed in place, independent column controls, Ledger-withheld labels, grants revoked after mount, malformed reversal dates that keep the dialog, cancel and one-shot confirmation, and complete change history beyond 500 rows. Browser proof uses this disposable database and the real app; it never seeds a product installation.

`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.

The shared `clear-flash` event clears only the current message kind, so a delayed success timeout cannot erase a newer settings or mapping refusal.

### People event proof

`bilibi-finance-payroll-source` extends the walkthrough without importing People. Run `prepare` on an empty database with both Domains mounted. Through the People facades, create and approve `FY1-run` with an earning of `12.345678 × 1.234567`, and three claims of `80.00` in the fixture book's currency; reimburse the newest claim. Run the binary with `post` to deliver the real outbox through Base Queue and post the run and one claim.

The proof checks the twelve-place source amount, rounding to the book's minor units, a missing-mapping refusal and retry, local standing, duplicate posting, evidence pagination, authorization, audit and a safe replay. The remaining claims are available for the browser's Post and Post every unposted claim actions. Rebuild without People and run `absent` against the same database: source lists and writes refuse, while local postings and evidence remain. After remounting, run `retained` to check the copied lists and original postings.
