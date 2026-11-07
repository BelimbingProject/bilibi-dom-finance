# Appointed auditor and sales workflow access

This addresses [Finance #95](https://github.com/BelimbingProject/bilibi-dom-finance/issues/95). It builds on the Platform's company- and date-bound role assignments (Authz, [ADR 20](https://github.com/BelimbingProject/bilibi/blob/main/docs/architecture/decisions/20-company-role-assignments.md)); Finance adds no permission system of its own. [Accountant access](accountant-access.md) still describes the External accountant role, which is neither of these.

## Appointed auditor

A company gives a person the **Appointed auditor** role for one company, with a purpose (the appointment reference the company holds). The role cannot be assigned without both. The start and end dates are optional: leave them empty for an engagement with no end date, or set them for a time-limited one. After the end date the assignment stops matching on its own; nothing needs to run. Remove the assignment to end it earlier. The person also needs a desk in that company, as for any work.

The role holds read keys only: ledger view, the view key of every Finance source module except Payroll (attachments, receivables, payables, reconciliation, e-invoice, SST, fixed assets), and the report export. It holds no post, approve, submit, settings, close or issue key, so an auditor cannot change a record or approve one. Proposed audit adjustments go to management outside this role.

**Payroll is a second, separate grant.** Payroll holds per-person pay and identity detail, and whether an auditor may read it is a decision for the company and its advisers. The role **Appointed auditor: payroll evidence** adds only Payroll view, with the same company and purpose rules. Give it when the engagement needs it; remove it to take it back.

The export follows the keys. With both roles the auditor generates the full pack (`all-finance`: ledger, reports and every source dataset). With the auditor role alone, the pack is the labelled ledger-and-reports pack: its manifest and the page say source records are excluded and that it is not a complete audit evidence pack. Source pages and operation reads still work for the modules the auditor holds. A stored full pack is rechecked on every download, so removing the payroll role (or ending either assignment) refuses an old full pack at once.

### Open questions for the company and its advisers

These are decisions Finance cannot make. The stricter choice is implemented.

- Whether the engagement entitles the auditor to Payroll person-level detail (Companies Act 2016 s. 266(4) gives access to accounting and other records the auditor reasonably requires). Default: not granted; grant the payroll evidence role to allow it.
- Auditor independence. Do not give the same person the External accountant role, posting delegation or any write key. Authz cannot refuse that combination; review the person's roles and direct grants when appointing. Direct grants stay tenant-wide.
- Export purpose. The pack records who, when, company and coverage, but not a typed purpose.
- Partial packs. A missing source key still drops all source evidence from the pack, never a single module. Per-module packs are a follow-up.
- Optional posting delegation stays a separate, later grant and must never be combined with the auditor role.

## Sales workflow

**Sales workflow** is a workflow boundary, not data segregation. It holds Receivables view, manage customers and manage documents (drafts). It holds no ledger, reports, issue, receipt, setup, payables or Payroll key. A person with it can list customers, read drafts and invoices, and prepare, edit and discard a draft invoice; they cannot issue it, record a receipt, see the chart, entries, trial balance, reports, aging or any pack, and every one of those refuses for a page, an operation read and a plan alike.

What the role does **not** hide, because Receivables records it: the income and tax accounts a draft line uses, saved lines and amounts of drafts and issued invoices, receipts and their deposit accounts as Receivables shows them, and customer facts as Core allows. Strict segregation (hiding records by account, customer or line, with a mixed-entry rule, restricted drill-downs, search, notifications and exports) is **not** built. If a company needs it, define the authorized records and the mixed-entry treatment first; the practitioner's advice is that mixed journals stay hidden from a restricted person unless every line is authorized, and that a restricted total is never shown as a complete statement. Until then a person who must see complete statements needs an oversight grant (ledger view), not this role.

Two details to know:

- Money amounts on Receivables pages use `finance.receivables.book_figures.get`, which returns only the book's currency and decimal places under Receivables view. Preparing a draft checks the chart and currency under Receivables' own capability; it never returns the chart.
- Preparing an invoice draft checks only that the customer and its Core company or person are active, so it needs no Core Companies permission. Issuing (outside this role) copies the company's facts onto the invoice and needs Core's tenant-wide `admin.company.view` and `admin.address.view`.

## Every door uses the same decision

The role keys opt in to company scope (`CapabilityDefinition::company`). Each facade decides with `authorize_work_target` for the explicit company, so a role assigned for Company A refuses Company B on pages, operation reads, plans, exports and downloads alike, even when the person has desks in both. An expired, future-dated or removed assignment refuses an already-signed-in session on the next call.

## Running proof

Run `cargo run -p bilibi-finance-walkthrough-fixture --bin engagement-access` on a fresh disposable PostgreSQL 18 database, migrated with `bilibi-web migrate` and with `AUDITOR_PACK_PROOF_DIR` naming a private directory. It decides all Finance capabilities for the assigned and the other company, proves reads and write refusals through the facades, the restricted and full pack, payroll grant and revoke, future start, passed end date, open-ended engagement and removal against retained scopes and old pack links, and the sales workflow's allowed and refused calls. Append `serve` to keep the app running for the browser: `viewer@example.test` is the auditor, `staff@example.test` the sales person, both for Company 001 only, password `bilibi-dev`.

With that server running, run `BILIBI_AGENT_PROOF=disposable BILIBI_ENGAGEMENT_CONTROL=/absolute/path/to/target/debug/engagement-access python3 apps/domains/finance/fixtures/walkthrough/engagement-access.py`, keeping `APP_ORIGIN`, `AUDITOR_PACK_PROOF_DIR`, `DATABASE_URL` and libpq's `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE` set to the same disposable instance. It uses OAuth-authenticated `/mcp` calls for both people: reads, refused reads and plans, a company 2 write refused even with the session desk in company 1, a sales draft committed, and an assignment revoked between plan and commit (nothing is written) and between two reads (the retained token is refused).
