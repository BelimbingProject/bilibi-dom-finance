# Payables

`finance/payables` owns a company's supplier documents. It mirrors Bilimbi.Finance.Payables settings, tax codes, bills, debit notes, document approvals, payment requests and payments, including aging, tax totals, control-account tie-out and the three-way match. It also keeps source-backed opening supplier credits and their applications, refunds and reversals. Reviewed agent operations expose the facade-only workflows. Slice NP4 adds the Bills list and bill detail pages. Slice NP5 adds the pages that record a bill or debit note, request, decide and reverse supplier payments, read aged payables and set payables up, so a person who has no AI agent can do every daily task; each page write is the contract the agent doors call.

## Vendors

**Drop target (issue 265).** Companies and Contacts dragged from another tile onto Vendors, or chosen with "Add to…" on their own list, open the Add suppliers dialog with them already chosen (`payables.vendor`, up to 20 at a time, `bilibi_records::drop_target!` in `src/web/mod.rs`). Confirming calls `vendor.create` as "Add vendor" does; a party that is already a supplier, or that the person may not see, is left out with a notice.

Add suppliers by picking Companies or People in the shared Add suppliers dialog, or creating either kind there. Each selected party shows its fixed Core code; payment terms apply to every selected row. Each row uses `vendor.create`, so a refusal keeps that row and its message. Core creation, the permanent Base Records reference, the vendor relationship and Finance settings commit together. Finance owns no vendor code: the party's Core code is used in lists, search, bills, reports and evidence. Finance keeps terms, account and tax defaults, and availability. Core owns identity, contact details and status. A Company or Person can be both customer and supplier. Already linked parties are disabled and link to their Finance record.

New bills take `vendor_id`, including replacement drafts. Both Finance availability and an active company are required. Pending, suspended and archived companies are refused by name. Due date defaults to the document date plus the vendor's payment terms; the form offers its expense and tax defaults. Posting uses the vendor's payable account when configured, otherwise the company default. Posted bills retain their original control account for payments and debit notes.

The bill freezes the Core party code, name and permitted tax and registration facts at commit under Core's view permissions. A restricted fact refuses the write. The shared `PartyLinks` reader and `PartyChooser` in the ledger serve both customers and vendors. The Core code, name and status are read through Base's party identity seam, so they need no Core view permission and are never blank or a placeholder: a missing or other-tenant party refuses the read. An opening register resolves its codes with one `parties_by_codes` call and one vendor SQL read per batch; preparing reads no Core party per vendor, and committing reads each distinct vendor's full facts once (never per row) so opening bills and supplier credits freeze the same tax and registration numbers as ordinary bills. Preview can read the name and status; full facts need the committing connection. No billing address is frozen until Core offers the read on that connection (BelimbingProject/bilibi#231); the company link opens Core's current facts and billing addresses.

### Supplier keys

A vendor's bills, payments and aged balances are grouped under its one key, `vendor:{id}`; no table holds it, and Finance matches no other key to a vendor. A payment request names `vendor_id` and allocates only bills with one key. `supplier_key` is the explicit settlement input only for a bill group that belongs to no vendor. Aging keeps one row per key and adds `vendor_id` when the key is a vendor's. Document, payment-request and payment reads carry that `vendor_id`, and their list contracts accept it. Existing requests can still be approved and payments reversed after the linked company becomes unavailable. Debit notes retain their original bill's snapshot and key. New bills check duplicate supplier invoice numbers against the vendor's key.

### Vendor contracts

All keys below start with `finance.payables.` and take `company_id`.

| Contract | Input |
| --- | --- |
| `vendor.create` | Exactly one of `record_kind` (`company` or `person`) with `record_id`, `new_company`, or `new_person`; optional `payment_terms_days` (default 0), `active` (default true) and `payable_account_id`, `expense_account_id`, `tax_code_id` |
| `vendor.update` | `vendor_id` and the settings to change; omissions preserve values, `clear_payable_account`, `clear_expense_account` or `clear_tax_code` clears a default; the Core party cannot change; no Finance code is accepted |
| `vendor.get` | `vendor_id` |
| `vendor.list` | `after_id`, `page_size`; returns `records`, `next_after_id` |
| `bill.create`, `bill.replace_draft` | `vendor_id` replaces all former party input fields |
| `payment.submit` | `vendor_id`, allocations and payment fields; `supplier_key` may identify a bill group that belongs to no vendor |

Vendor settings require `finance.payables.settings.manage`; reads require `finance.payables.view`. Company finding, creation, facts and relationships use Core's own permissions. Migration `finance-payables-004-vendors` adds the vendor master, and `finance-payables-011-drop-vendor-keys` drops the supplier-key mapping table that went with the removed historical-key review. `finance-payables-005-vendor-company-scope` keeps vendor registration independent of opening a book, as customer registration is. Evidence adds the `vendor` dataset with an explicit field list.

Run the linked-vendor proof on a disposable database with `cargo run -p bilibi-finance-walkthrough-fixture --bin vendors`. Set `VENDORS_UI=1`, `APP_ORIGIN` and `BIND_ADDR` to keep the same composed app running for browser proof. After the proof has populated the disposable database, `cargo run -p bilibi-finance-walkthrough-fixture --bin vendors -- serve` serves it again without rerunning the scenarios.

## Facade

Resolve `Payables` through `finance/payables.payables`. Every call takes the sealed scope and an explicit company ID; every call authorizes through Core Company. Writes open through `Ledger::write`, holding the company lock on an audited `MutationTransaction` and refusing an archived company. Book and account validation use the ledger's authorized `get_book_on`/`list_accounts_on` on that same connection. Actor and impersonator always come from the scope.

| Calls | Capability |
| --- | --- |
| `get_settings`, `list_tax_codes`, `list_settings_changes`, `list_tax_code_changes`, `get_document`, `list_documents`, `page_documents` | `finance.payables.view` |
| `put_settings`, `create_tax_code`, `update_tax_code` | `finance.payables.settings.manage` |
| `create_bill`, `create_debit_note` | `finance.payables.documents.create` |
| `replace_draft` | create and approve (`finance.payables.documents.create`, `.approve`) |
| `approve_document`, `void_document` | `finance.payables.documents.approve` |
| `post_document` | `finance.payables.documents.post` |
| `submit_payment` | `finance.payables.payments.submit` |
| `approve_payment`, `approve_payment_with_override`, `reject_payment`, `reverse_payment` | `finance.payables.payments.post` |
| `get_payment_request`, `list_payment_requests`, `page_payment_requests`, `get_payment`, `list_payments`, `aged_payables`, `tax_totals`, `tie_out_control_accounts`, `evidence_page` | `finance.payables.view` |
| `approve_match_exception` | `finance.payables.match.approve` |
| `billed_quantities` | `finance.payables.view` |

Configuration and recording additionally read the ledger (`finance.ledger.view`); approvals ask `Ledger::check_approval` on their own transaction with the creator and any impersonator as preparers. Posting additionally requires `finance.ledger.entries.post`. Ledger refusals, including same-person approvals and closed periods, propagate as `PayablesError::Ledger`. The eight reference capability keys are contributed to `tenant_owner`.

Settings name an active liability account and optional quantity/price tolerances (percent or amount, nonnegative with at most four decimal places). Tax codes have an immutable code, name, percentage below 1000 with up to four decimal places, active flag and an active ledger account. Each actual change captures its previous/new values, optional reason, actor, impersonator and database time in an append-only history. An unchanged write records nothing. Histories page oldest first by `after` ID, with `limit` from 1 to 500 and `next_after`.

## Documents

A bill records a party snapshot, the party's own number, dates, description and positive lines against active asset/expense accounts. A duplicate party number is refused until the original is voided. Due date defaults to the document date plus the vendor’s payment terms and cannot precede the document date. A debit note copies a posted bill's party, cannot precede the bill, and cannot exceed its remaining balance either when recorded or when posted. That balance is the bill less posted debit notes and allocated payments, counting reversal dates. Recording and posting a debit note checks the minimum balance on every later date, so a backdated note cannot overdraw a bill that a later payment temporarily settled.

Amounts are `Money`, revalidated against the book's minor units. Tax snapshots group lines by code in first-use order, copying the code, rate and account. Computed tax rounds half up once per code; explicit party amounts may override it, with unused or repeated codes refused. Later tax-code changes never change a recorded document.

Documents move `draft → approved → posted`, or `draft/approved/held → voided` with a reason; a bill naming order lines may be `held` instead of posted (see Three-way match). Approval invokes the ledger's separation of duties, including impersonation. Approval makes attachments final; a void also keeps them. All content and lines remain immutable. The database stamps each transition and assigns a gapless document number per company/type only on posting.

Posting calls `Ledger::post_entry_in` on the document's own transaction, owned as `finance/payables`, `document:<id>`. A bill debits line/tax accounts and credits the current payable account. A debit note mirrors this against its bill's original payable account even after configuration changes; only that settlement account may be inactive. The ledger validates the document's actual date, and a refusal rolls everything back without taking a document number. No ledger table is read or written here.

`list_documents` is ordered by `(document_date,id)`, with a following cursor, date/type/status/party filters and limit 1–500 (default 100). `page_documents` is newest first, with case-insensitive literal party-name or party-document-number search (up to 255 characters), page and sizes 25/50/100. It clamps an out-of-range page to the last page, or 1 with no results. Reads return complete documents including lines, tax snapshots, correction IDs and a posted bill's outstanding balance.

## Bills pages

`/finance/payables/bills` requires `finance.payables.view`. It lists ordinary bills and debit notes with opening bills and supplier credits, showing their type and source. It preserves Bilimbi's `search`, `status`, `document_type`, `page` and `page_size` query names. Held bills applies the held filter while preserving search, type and page size. The FlexTable has eight built-in columns, column customization and compact/normal rows; no sortable headings, as in the reference. Search matches the party snapshot or the party's own number, not the internally assigned posting number. "Record a bill" opens the recording form (below). Totals show the book's decimal places with digit groups.

`/finance/payables/bills/{id}` shows the party snapshot, immutable lines and their order-line references, totals, outstanding, actors (including impersonation), ledger entry number, last comparison, exceptions and match history. An opening supplier credit also shows its retained source facts and settlement history, with Apply credit, Receive supplier refund and reversal actions through their reviewed contracts. All detail lists are FlexTables. Timestamps use the reader's display policy at second precision. Record links and announcements use the workspace kind `finance/payables`.

Approve and Post act immediately, as Bilimbi does, and call `finance.payables.document.approve` / `.post` / `.void`. The shared flash reserves page space so approval feedback cannot cover Post and consume its first click. Void and Approve exceptions use shared reason modals with the reference confirmations and 1–500-character reasons. Exception approval retains the displayed hold ID when the dialog opens; a new hold refuses the stale approval and requires a fresh review. Every write-shaped event checks its capability again, and the facade checks inside the audited write. Refusals stay on the page as flashes, including separation of duties, archival and ledger refusals (the ledger's own access or archive refusal reads as the page's, as Bilimbi's shared `:unauthorized`/`:company_archived` do); a successful post returning a held bill is a warning, not success.

The page mounts `finance.attachments-panel` for `finance/payables/document`. The source re-reads finality on every action: approval hides replace/remove but still allows additional evidence. Attachments owns uploads, retention and downloads; Payables owns no CSS or JavaScript.

## Recording and payment pages (NP5)

Bilimbi recorded documents, payments and settings through its API, so these pages have no Bilimbi counterpart. Each page write calls the operation contract an agent calls, through `PageContracts` (`Pages::write` in `src/web/mod.rs`; the events and contracts are the page service's `PageWrites`), so a person and an agent get the same validation, capability check, owner preview, audited transaction and separation of duties. Only approving match exceptions still writes through the facade, a listed `Pending` allowance, because no contract exists for it.

| Page | Path | Needs | What it does |
| --- | --- | --- | --- |
| Suppliers | `/finance/payables/vendors` | view; changes need `finance.payables.settings.manage` | Vendors linked to Companies, added through the shared company picker (see [Vendors](#vendors)) |
| New bill (dialog) | `/finance/payables/bills` ("+ Bill", or `?record=bill`) | `finance.payables.documents.create` | Vendor (picked from the company's vendors), invoice number, dates, lines with an account picker and tax code, live totals; saves a draft (`bill.create`) and opens it. Choosing a vendor fills the due date and each line's account and tax code from the vendor's defaults, replacing only values empty or last filled by a vendor default, never a value the person entered |
| Correct a draft (dialog) | `/finance/payables/bills?correct={id}` | create and approve | The same form filled from a draft. Documents are immutable, so saving calls `bill.replace_draft`, which voids the draft and records the corrected bill in one transaction: a refusal leaves the draft as it was, and the corrected bill may reuse its supplier invoice number. A draft that names order lines is not offered, because the form has no place for them |
| New debit note (dialog) | `/finance/payables/bills?debit_note={id}` | create | Lines against a posted bill (`debit_note.create`); the supplier is the bill's |
| Payments | `/finance/payables/payments` | view | Payment requests newest first with status filter and search, paged in SQL by `page_payment_requests` (page, size 25/50/100, the company's total), so no request is left out |
| Request a payment | `/finance/payables/payments/new` | `finance.payables.payments.submit` | Date, supplier (from the bills still owed on that date, `aged_payables`), paid-from account, an amount per bill; saves `payment.submit`. `?bill={id}` starts from one bill |
| Payment request | `/finance/payables/payments/{id}` | view | Facts, bills paid, the payment and its reversal; Approve and pay, Reject (reason) and Reverse payment (date) call `payment.approve`, `.reject` and `.reverse` (`finance.payables.payments.post`); mounts the payment attachments panel |
| Aged payables | `/finance/payables/aged` | view | One table by supplier with a Total row and credit balances apart, as of a day (`?as_of=`, one date in the toolbar that updates live, so not the shared period filter); each bucket amount links to the bills behind it (`?supplier=`, `bucket=`) |
| Payables setup | `/finance/payables/setup` | view; change needs `finance.payables.settings.manage` | For the company named under the title: the payable account and each tolerance edited in place (`settings.put`), tax codes with name, status and account edited in their rows (`tax_code.update`) and a dialog for a new one (`tax_code.create`), and the change history folded under History |

The recording forms keep their draft on the server and send every field as `bill[...]` (a line's are `bill[line_<id>_...]`), because `params::form_values` stops at 64 fields. Totals are worked out on the page with the facade's rules (tax rounded half up once per code) for display only; the saved draft has the facade's own. An amount, date or account the contract refuses is shown on its field and in the summary in the contract's words. Lines are limited to 100 here (the contract allows 500).

## Source documents

The contributed `finance/payables/document` source reads on the connection Attachments hands it and reauthorizes `finance.payables.view`. It labels a posted document by number and a draft by record ID. Attachments remain replaceable/removable only while the document is a draft. Dates cannot change after recording, so no retention refresh is needed at approval.

`external_order_line_id` is stored uninterpreted.

## Three-way match

A bill line may name an order line of the order source with `order_line_id` and the `quantity` billed against it (positive, at most four decimal places); the two come together. The unit price billed is the line's amount over that quantity, rounded half up to four places. When such a bill posts, each such line is compared within the company's tolerances, exactly as Bilimbi does:

| Exception | Rule |
| --- | --- |
| `quantity_exceeds_ordered` | the quantities every posted bill billed on the line, this one included, exceed the ordered quantity |
| `quantity_exceeds_received` | the quantity exceeds what was received (net of reversals) and not yet billed by other posted bills, less posted debit notes |
| `price_differs` | the billed unit price differs from the ordered unit price, either way |
| `order_not_open` | the order is not approved, sent, partially received or closed |
| `order_line_superseded` | an amendment replaced the line |
| `currency_mismatch` | the order's currency is not the book's, so the prices are not compared |
| `order_line_not_found` | the copy of the order source has no order facts for the line |
| `order_source_unavailable` | no order source is mounted |

A tolerance is `percent` of the ordered figure or an `amount` per unit; none means exact. A bill whose lines are all within tolerance posts as any other. Otherwise `post_document` records a `held` match event, makes (or keeps) the bill `held`, takes no number and posts nothing, and returns the held document. `approve_match_exception` takes a reason and the `held_event_id` the approver saw (`StaleHold` when a newer hold exists, `AlreadyApproved` when the last hold is approved, `NotHeld` otherwise); it asks `Ledger::check_approval` with the bill's recorder and whoever posted it into the hold as preparers, always listed as a `match_override`. The next post matches afresh and posts when every exception it finds is one the approved hold had with figures no worse (more billed over the order, less received, a wider price gap hold it again), or when it finds none. A bill naming one order line on several lines is matched cumulatively.

Every attempt is an append-only match event (`held`, `exception_approved`, `posted`) with the actor, impersonator and database time, and for a hold or a posting one match line per bill line with the billed quantity and price, what was billed before, the ordered quantity and price, the received quantity, the tolerances and the exceptions with their detail. Documents carry `match_events` and `match_status` (`MATCH_STATUSES`: `pending`, `held`, `exception_approved`, `matched`, `posted_with_exceptions`). PostgreSQL refuses a bill naming order lines posting without its `posted` match record in the same transaction, a held bill posting over exceptions without an approval later than its last hold, an approval not naming the last hold, a match event on anything but an approved or held bill, a match line or exception added to a recorded event, and any change to a match record.

`billed_quantities` is this module's own record: posted bill lines naming an order line less posted debit note lines, so the same received quantity is never billed twice; the company lock serializes postings. A debit note line may name an order line its bill names, with the quantity credited back within what the bill billed less earlier posted notes, checked when recorded (a field refusal) and when posted (`ExceedsBilledQuantity`).

### The order source copy

Bilimbi's runtime `OrderSource` read Supply Chain's facades. Here Supply Chain is another container, so Payables keeps its own copy (`finance_payables_order_lines`, `finance_payables_order_receipts`), fed by Supply Chain's documented domain events by name only: no Cargo dependency, descriptor edge, port or SQL. The `finance/payables.goods_receipts` subscriber reads `supply_chain/receiving.goods_received` and `receipt_reversed` v1 (Supply Chain `receiving/README.md`): each receipt line event creates the copied order line with its snapshot (order, line number, description, unit, ordered quantity), never overwriting a fact the copy holds, and records the accepted and rejected quantities once per event key. Received quantity is the receipts' accepted quantities less the reversals'.

- **Availability.** The order source is available while every event the copy reads is a mounted event type (`order_source_available`). Unavailable, a bill line cannot name an order line (`names no installed order source`) and a bill recorded earlier is held with `order_source_unavailable`; Finance composes and boots alone.
- **Recording.** Available, a bill line names only an order line the copy knows (`is not an order line of this company in the order source`).
- **Order facts.** The `finance/payables.purchase_orders` subscriber reads `supply_chain/purchasing.order_status_changed`, `order_approved` and `order_amended` v1 (Supply Chain `purchasing/README.md`, "Order events v1"): each part carries one line generation (current or superseded) as the order stands after the operation, and its quantity, unit price, currency, order status and superseding amendment replace what the copy held for that line, keeping the line number and description a receipt brought. Delivery keeps one order's parts in order per publisher, and a receipt may arrive before the order's facts: a line with no order facts yet matches as `order_line_not_found`. An empty part (an empty draft cancelled) changes nothing.
- **Delivery.** Base events delivers as the `domain-events` system principal: a tenant administrator grants it `events.delivery.deliver` (`Grants::set_system`) or deliveries wait, and `Events::replay` hands a newly mounted Payables the events recorded before it.
- **Remounting.** Unmounting Finance keeps its tables, the copy included, but Base events creates deliveries only for the subscribers mounted when an event is recorded: an order sent or a receipt posted while Finance was out has no Payables delivery, and once Finance is back a bill line naming that order line is refused (`is not an order line of this company in the order source`). Replay both subscribers (`finance/payables.purchase_orders`, `finance/payables.goods_receipts`) from the first event recorded while Finance was out; delivered events are skipped, so replaying from an earlier event is safe. `TODO(platform)`: `Events::replay` has no page or agent operation yet, so a release build can only replay through the facade.
- **Synchronization.** Both subscribers lock the tenant's company row before reading or changing the copy, through the delivery transaction. This keeps matching facts stable throughout a reviewed Payables write without changing the system principal's authorization or refusing delivery to an archived company.

## Proof

Run the mounted disposable database walkthrough: `DATABASE_URL=… cargo run -p bilibi-finance-walkthrough-fixture`. With Supply Chain unmounted (as in this repository's CI) its matching scenario shows the unavailable source, the recording, debit-note and approval refusals, `billed_quantities` and PostgreSQL's match guards; with Supply Chain mounted it shows the copy refusing an unknown order line. Matching against real receipts needs both containers' facades, which no crate of either repository may name; the PR records that run. The payables scenarios prove configuration histories, immutable tax snapshots, bills and notes, independent approvals, atomic owned posting and rollback, `replace_draft` (a refused replacement leaves the draft as it was), newest-first paging of payment requests and payments past one page, attachment finality, authorization, archived companies, period/account refusals, concurrent posting and PostgreSQL's own guards. The composition gate is Platform `scripts/domain-ci.sh`.

For the running bills pages, use a fresh disposable database with `DATABASE_URL=… APP_ORIGIN=… BIND_ADDR=… cargo run -p bilibi-finance-walkthrough-fixture --bin bills-ui`. It creates 30 bills, a debit note and a source file through the facades, checks literal party-number search, last-page clamping and read-only live-event refusal, that the recording, payment, aged and setup pages render for the recorder and hide their controls from a reader, then serves the real app with the queue executor. The standard walkthrough accounts and password apply. With Supply Chain mounted and its real order/receipt events delivered, `bills-ui hold <order-line-id>` creates an order-linked held bill; `post <bill-id>` records a newer hold for a two-session stale-review check. `deny <capability>` and `restore <capability>` change the recorder's direct grant for a stale-control authorization check. These commands are disposable fixture tools, not production entry points.

## Payments and reports

`submit_payment` freezes a payment request and its positive, unique bill allocations without reserving the balance. Each bill must be posted, belong to the party and be dated on or before the payment. The paid-from account must be an active asset or liability account. Amounts are revalidated with the book's minor units. The request is numbered by the database; its single append-only decision is `posted` or `rejected` (with a required reason). `list_payment_requests` filters by status and pages by request number, oldest first, default 100 and maximum 500; `newest_first` reverses the order and `after` is then the lowest number seen (none starts from the newest). `page_payment_requests` is newest first by page number, with the status filter, a case-insensitive literal search in the supplier's name and code, the reference and the request number (up to 255 characters), sizes 25/50/100 and the matching total; an out-of-range page is clamped to the last one. `list_payments` orders by `(payment_date, id)` and takes `newest_first` the same way, with `after` as the lowest pair seen.

`approve_payment` invokes ledger duty separation against the submitter and impersonator, then repeats every validation against current data. `approve_payment_with_override` additionally records an explicit override reason through the ledger. It debits each allocated bill's original payable account and credits the paid-from account, atomically storing the owned entry (`payment:<id>`), payment, allocations and decision. Historical payable accounts may be inactive for this settlement. A failed approval leaves the request submitted and consumes no payment number. Requests do not reserve funds; concurrent approvals serialize on the company lock.

`reverse_payment` calls the ledger's owned reversal path in the same audited write as the reversal record. Its date cannot precede the original; a payment reverses once, and a reversal cannot itself reverse. Reversals carry no allocations. A bill becomes owed again from the reversal date. Payment reads include their reversal IDs; lists filter by party, document (including its payment reversals), inclusive dates and a `(payment_date,id)` cursor, bounded at 500.

`aged_payables` groups positive outstanding bills by party and due date and lists opening supplier credits separately by source date, with default 30/60/90-day boundaries and a not-yet-due bucket. Negative balances are explicit exceptions. Its complete pin has the ledger-entry, opening-batch and settlement boundaries, so past readings survive later backdated postings and entryless credit applications. `tax_totals` groups immutable posted tax snapshots by type, code, rate and account, with optional date window and posting fence. Debit notes retain their positive totals under their own type.

`tie_out_control_accounts` first reads `Ledger::account_totals_by_owner`, then reads the subledger at that exact posting number. It includes every historical/configured payable account, shows credits less debits, and lists other owners' postings as differences. No ledger table is read. Retain its complete entry, opening-batch and settlement boundaries for other report columns: same-control credit applications create no entry. `report.pins()` supplies all three through `BalancePins` to `tie_out_control_accounts`; `AgingQuery` accepts the same tuple. Zero excludes that stream.

The `finance/payables/payment` attachment source describes a payment request from submission onward, so pre-approval remittance evidence stays linked to its posted payment. Its attachments can be replaced or removed while submitted and become final when posted or rejected. `evidence_datasets` and authorized `evidence_page` export all owned tables, the match records and the order source copy included, through frozen field lists, ID-ordered pages of 1–500, with exact numeric text and UTC timestamps. The NP3 walkthrough exercises the workflow, refusals, temporal balances, pinned reports, tie-out, evidence, attachments and database immutability.

## Reviewed agent operations (H3)

The `finance.payables` guide is contributed to Base Agent API. Reads cover vendors, settings, tax codes, document pages and records, payment requests, payments, aging, opening context, credit choices and control tie-outs. Reviewed writes cover vendor setup and mapping, payables setup, tax codes, bills and debit notes, document decisions, payments, opening imports, credit applications, supplier refunds and settlement reversals. Prefix each with `finance.payables.`. The host runs the dry check, shows it for explicit approval, then sends the unchanged operations to the write; an agent cannot call a write directly. Capability keys remain those of the facade, including additional Ledger checks. See [guide.md](guide.md) for exact fields, omissions, clear flags and the workflow.

Reads use stable `after_id` cursors and `page_size` 1–100, not the facade's date/request-number cursors. They list oldest first; `payment_request.list` and `payment.list` take `newest_first` to start from the newest, the cursor then being the lowest ID seen (omit it for the first page), and `next_after_id` is null when no more exist. Document reads omit the potentially unbounded `debit_note_ids` collection; traverse `document.list` with `corrects_document_id` instead. They expose bounded `match_context` (the latest match event and its approved hold, at most two), not complete match history. Immutable document lines/tax groups and payment allocations are bounded by their recording contracts. Agent operations add no pages or assets.

The private transaction-joining methods repeat company authorization and the writable-company lock, and never commit independently of the plan. Previews share facade validation and Ledger's authorized posting/approval/reversal previews. They retain normalized amounts, tax snapshots, original control accounts, dated minimum balances and approval exceptions, but allocate no business IDs or numbers. A transaction-local FIFO review fence handles the pinned host's preview-all-then-execute lifecycle: if an earlier step changes any later review, the whole plan rolls back. Separate plans are required for dependent steps. Chart/period context is intentionally conservative; unrelated chart or period edits can also require another review. No Platform changes are needed.

### Real HTTP and MCP proof

Use a fresh disposable PostgreSQL database and the composed app, never production. In the Platform root:

```sh
export DATABASE_URL=postgres://…/disposable_h3
export BILIBI_AGENT_PROOF=disposable
export BILIBI_AGENT_CREDENTIALS=/tmp/h3-credentials.json
export APP_ORIGIN=http://localhost:3100
cargo run -p bilibi-finance-walkthrough-fixture --bin agent-operations
# Run bilibi-web on 3100 with the same DATABASE_URL and APP_ORIGIN.
python3 apps/domains/finance/fixtures/walkthrough/payables-operations.py
```

The bootstrap creates real OAuth API and resource-bound PKCE MCP credentials in a new private 0600 file. The proof never prints tokens, and SQL only observes effects. The existing facade walkthrough and H1 wire walkthrough run on separate fresh databases. No unit tests or mocks are introduced.

`evidence_page_on` is the same authorized evidence contract on a caller-supplied PostgreSQL client or transaction. Use it for every page of one repeatable-read export (Reports auditor pack); the pooled `evidence_page` wrapper is a live read per page. Schema helpers remain private.

Bills show account codes and names from the ledger account-list contract, and money with thousands separators and the book's decimal places. A separation-of-duties refusal links to Approval exceptions so a one-person company can review its approval control.

## Opening supplier register at accounting system conversion

Import the complete supplier opening register against an approved opening trial balance, before posting ordinary bills. The register may contain unpaid bills, unapplied supplier credits or both. Configure the payables control account first, then choose **Opening bills** on **Payables → Bills** (`/finance/payables/bills`) for the current company. The reviewed import is a dialog on that list. After approval, the list shows the opening records and links to their document pages. Source and status are columns; the source filter selects all, ordinary or opening records. Pagination and totals apply after filtering. There is no separate opening-bills route or menu item.

The UTF-8 CSV header is:

```csv
source_id,vendor_code,vendor_name,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts
```

Create the linked Finance vendors first. `vendor_code` is the fixed Core code, and `party_kind` (`company` or `person`) distinguishes the Core kind. They must name an active supplier of the current company with an active Core party. `vendor_name` preserves the historical source name; it does not create or rename a vendor. The recorded bill freezes the linked Core company's permitted identity facts and uses the vendor's canonical settlement key. A Core kind and code cannot identify different source names within one export. The source document reference must be unique for that vendor and cannot duplicate a recorded bill. Use original gross totals and the positive residual at cut-off, including partially settled bills. Read `opening_accounts` from the context and classify every non-zero asset and liability as its compatible control type or as not a control. Unclassified accounts block review. Set each `control_account_code` to a liability classified as a payables control; this can be an inactive old-system control that was never configured here. Every payables control is reconciled, so omitting all rows for one refuses the import with that account and its approved balance. Preserve historical document and due dates; document dates precede the conversion date, which is the first day Bilibi owns processing.

**Preview import** shows the source items and exact reconciliation to each mapped opening control balance. **Approve import** records the complete register and source evidence in one audited transaction without posting purchases, tax or the control balance again. Approval uses Ledger's impersonation-aware duty separation against the opening journal's source preparer; an authorized exception requires its reason. The import and its source opening entry are permanent dependencies, so the opening entry cannot subsequently be reversed.

UI and MCP call the same `finance.payables.opening.import` contract. Read `finance.payables.opening.context` for the book, approved opening entry, opening accounts, CSV header and retained batch. Write input is `company_id`, `source_reference`, `currency_code`, `csv`, `opening_account_classifications`, and optional `override_reason`; staged batches and corrections add the fields in the [opening guide](opening-guide.md). The public facade is `Payables::import_opening_bills`; agent commits use its private transaction-joining implementation.

Imported bills use the normal payment-request, independent payment approval and reversal operations, retaining their original control account. From conversion onward, bills age from their historical due dates and unapplied credits age from their source dates. Aging excludes opening records before conversion. Settlement before conversion is refused for payments and for debit notes against an opening bill, which name the offending field. Original gross totals, residuals and settlements before conversion remain source facts; no historical payment or source-period tax lines are fabricated. Control-account tie-outs include the residuals and retain the original source journal's owner, with separate opening-recognition evidence.

A direct import accepts 1–500 bills and unapplied credits with positive residuals, at most 128 KiB. Retries with the same reference, opening account classifications and byte-identical CSV return the recorded batch, including after settlement; changed retries and duplicate source IDs are refused. Previewed differences can be corrected before approval. Committed corrections use independently approved staged replacements. Foreign-currency translation is refused. Every row must use the book currency; never re-enter these balances as ordinary new purchases.

The `opening_batch` and `opening_item` evidence datasets retain the exact CSV, recorded identity and source facts; `opening_stage` and `opening_decision` retain staged batches and each reconciliation or replacement decision. The accountant reviews completeness, vendor mapping, source and target control reconciliation, cut-off, tax and retained historical evidence.

### Opening credits and source tax facts

Use `item_kind` `bill` or `supplier_credit` and a positive `outstanding` amount. A credit is an immutable opening source, not a newly issued taxable note. `source_kind` is `bill` for an unpaid document, or `credit_note` / `payment` for a credit. `source_system` names the system the export came from. Each row's `currency_code` must equal the book currency. A credit's due date may be empty; credits age from their source dates, separately from unpaid bills aged by due date.

`source_tax_facts` is either `unknown`, `not_applicable`, or a quoted CSV field containing a JSON object. For example, `{"status":"supplied","net":"100.00","tax_total":"6.00","lines":[{"code":"source-code","rate":"6","taxable_amount":"100.00","net":"100.00","tax_amount":"6.00","gross":"106.00"}]}` describes a source gross of 106.00. Escape JSON quotes according to CSV rules. A supplied zero (`{"status":"supplied","tax_total":"0.00"}`) stays distinct from unknown or not applicable. Optional `net`, `tax_total` and `lines` preserve only facts actually supplied. Amounts use book precision. Every line needs either gross or both net and tax. The line totals must add up to the original gross. If the source also supplies an overall net or tax total, every line needs that amount and the line amounts must add up to it. Taxable amount times rate must round to tax amount at book precision. Codes and rates remain source facts. No tax share of a residual or earlier allocation is inferred, and no source tax lines enter current-period tax reports.

Classify every non-zero opening asset and liability. You may also explicitly classify an eligible zero-balance control, including one with no line in the approved opening journal. Review every classified control. Reconciliation shows gross debits, gross credits, net, approved opening balance and difference per account. Offsetting rows, zero and contra-sign net balances are accepted only when each control reconciles exactly. Zero recognition protects the same approved journal against reversal. It changes neither the journal nor its owner.

### Apply, refund or reverse a credit

On the credit's existing bill record page, choose **Apply credit** or **Receive supplier refund**. Both page actions and agent plans use `finance.payables.credit.apply` and `finance.payables.credit.refund`. The review shows the linked party, original controls, residuals, effective date, currency, approval and posting. `finance.payables.settlement.reverse` appends a reversal from the same record's settlement history. The public owner methods are `Payables::apply_credit`, `refund_credit` and `reverse_settlement`; agent commits join the handed audited transaction.

An application must fit both available balances, including every later recorded balance-changing date. Both items must belong to the same company, linked party and currency. It cannot precede conversion or the target's date. Same-control application posts no entry. Different controls transfer between the two saved controls only. A refund uses an eligible active cash/deposit account and the credit's saved control, in an owned entry. It implies no income, expense or tax. Saved inactive controls remain eligible only for their own settlements.

Every application, refund and reversal is an append-only `settlement` evidence row. Its per-company settlement number is assigned under the company write lock, in commit order; rollback exposes no event or boundary advance. Refusals, stale reviews and concurrent over-allocation leave no partial event or entry. A reversal names its original event and date, reverses any owned entry atomically and restores the affected balances. An independently reviewed replacement must first reverse dependent settlements and satisfy existing receipt/payment, note, request and write-off restrictions. Old items remain available to interpret earlier events and report pins.

Keep all three boundaries and the as-of date when replaying aging or a control tie-out. Entry-backed events require their entry within the ledger pin as well as their settlement within the settlement pin. Reversals follow their own date and boundaries. The figures retain other owners' postings as visible differences per control. Pins freeze amounts and membership; issue a frozen report when labels and layout must also remain immutable.

`AgingQuery` and `BalancePins` accept and return the same three accounting boundaries through `report.pins()`. Pool-owning reads capture one repeatable-read snapshot.

## Opening register corrections

Stage a replacement export, then approve the complete reconciliation independently. Source batches, original records and approvals remain immutable. The [opening guide](opening-guide.md) describes batch limits, the conversion-date effect, dependent reversals and historical report pins. `fixtures/walkthrough/src/bin/conversion.rs` proves the lifecycle on a disposable composed database.

For browser/MCP parity, serve the disposable `conversion` fixture, provision credentials with `agent-operations conversion-credentials`, then run `fixtures/walkthrough/conversion-lifecycle-operations.py`. This mode uses the same independent reviewer for both doors; the default contractor fixture retains its Core fact masks.

The Bills page reads `finance.payables.document.page`, with `source` (`ordinary` or `opening`), `status`, `document_type`, `search`, `page` and `page_size` (25, 50 or 100). Its result contains only displayed bill-row fields, not document lines, tax groups, corrections or match history. The cursor-based `finance.payables.document.list` also accepts `source`. Both retain the caller's company and view permission. The opening dialog uses the existing `finance.payables.opening.import` and `.stage` preview/commit contracts.

Readers can open the opening dialog to inspect retained source batches. The import form still requires the module's existing opening-write permission, and approval checks run in the shared contract.
