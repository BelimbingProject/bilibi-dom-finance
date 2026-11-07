# Finance browser walkthrough

`finance-ui` prepares one disposable database with an ordinary tenant's recorder, independent reviewer and read-only reader, plus bills, source documents, a ledger acquisition, an asset, a bank account, receivables customers, invoices and receipts, and e-invoices backed by a local authority. It also checks the Fixed Assets list/detail flash lifecycle through their real mounted page adapters.

Run from the mounted Platform root with `DATABASE_URL` on a fresh disposable database, `BILIBI_SETTINGS_KEY` set to a fresh 64-character hex key, `FINANCE_UI_STORAGE` naming a private directory, and matching `APP_ORIGIN` and `BIND_ADDR`:

```sh
cargo run -p bilibi-finance-walkthrough-fixture --bin finance-ui
```

Sign in as `manager@other.example.test`, `reviewer@other.example.test` or `viewer@other.example.test`, using the Platform walkthrough password. Tenant 2 is not the Platform operator. The startup output gives each record's URL and the acquisition/account IDs. Add `serve` to reopen the app after a code change while keeping browser-created records; retain the same storage directory and settings key. The local authority is restarted and existing e-invoices are kept.

No fixture is a production seed. The module-specific binaries remain the detailed accounting proofs; `scripts/domain-ci.sh` in the Platform is the repository gate.

## Running them in CI

`scripts/domain-ci.sh` only compiles these binaries. The **Walkthroughs** workflow (`.github/workflows/walkthroughs.yml`) runs them through `scripts/run-walkthroughs.sh`, one empty disposable database each: every night (and on a manual run) against the Platform's `main`, and on a pull request that touches `fixtures/`, that script or the workflow, against the pinned revision. The script lists what runs (`WALKTHROUGHS`) and what is known to fail (`KNOWN_BROKEN`, run and reported but not failing the run, with the reason beside each; remove a line when it passes). To run it by hand from a mounted Platform checkout after `cargo build -p bilibi-web -p bilibi-finance-walkthrough-fixture --bins`:

```sh
DATABASE_URL=postgres://user:password@localhost:5432/postgres scripts/run-walkthroughs.sh [name ...]
```

Add a walkthrough by building its binary and adding a line to `WALKTHROUGHS`. A fixture account that acts in a company needs a Person, an affiliation and a desk there: `install` gives them to Company 001's administrator and viewer and tenant 2's manager through `install_staff`, `give_desk` adds one in a company a walkthrough created, and `sign_in` also writes the session row Agent Access needs. A binary that serves the app ends by serving: the script counts it as passed once it accepts connections and stops it by its PID. Not run, because they need more than a database: `agent-operations`, `one-door`, `bank-bridge`, `einvoice-operations` and `receivables-control` (OAuth credentials and a second process), `sst-ui` (needs `sst` on the same database first) and `payroll-source` (needs People mounted).

## Accounting system conversion

On a fresh disposable PostgreSQL 18 database, run the composed fixture with the same private settings key and loopback origin throughout:

```sh
cargo run -p bilibi-finance-walkthrough-fixture --bin conversion -- serve
```

It prepares source opening balances and setup through the owning facades, leaving the opening registers empty. Company 001 is the browser company; Company 002 is the separate MCP company. Company 004 has two receivables controls and two payables controls, including old-system controls that were never configured in Finance. Sign in as `admin@example.test` for the independent reviewer. On the Company 001 Bills, Invoices and Fixed assets lists, open each opening-register dialog and import the printed customer, supplier and asset CSVs. Open the same dialogs on Company 004's lists, preview both multi-control registers, check both account reconciliations, then cancel those previews. Inspect aging and the asset schedule, record a 1,000 customer receipt dated 2026-07-15, and submit a 500 supplier payment as `manager@example.test`, approving it as the reviewer. The conversion date is 2026-07-01. Do not run depreciation in these two companies before comparing them.

Provision private disposable OAuth credentials with the `agent-operations` binary (`BILIBI_AGENT_PROOF=disposable`, `BILIBI_AGENT_CREDENTIALS` naming a new private file), then run `conversion-operations.py` with that environment and `BILIBI_AGENT_URL` pointing to the fixture. It discovers the import contracts and guides, exercises mismatch refusals, dependent-plan rollback and exact retries, previews both Company 004 multi-control registers, proves that omitted old-system controls are named and refused, and repeats the imports and settlements in Company 002 through MCP. It compares the final account balances with Company 001 and checks that imports add no ledger entry or historical tax lines. Source setup is fixture preparation, including the asset category; it is not evidence of general asset lifecycle MCP operations.

To check all three merged lists through the normal Platform launcher, use a separate fresh disposable database and prepare it without serving:

```sh
BILIBI_OPENING_PROOF_CONFIG=/private/path/opening-sources.json \
  cargo run -p bilibi-finance-walkthrough-fixture --bin conversion -- prepare-launcher
```

Start that database with the Platform's `./bilibi start` on a unique HTTPS `.localhost` name. Sign in as `ai@agent.my` with the walkthrough password. Startup retires the old walkthrough accounts. The three record lists show their opening actions, source and status filters, and the same record pages for imported records.

To use MCP against that launched app, run the same binary on the same database with a new private credentials path and the exact launcher origin:

```sh
BILIBI_AGENT_PROOF=disposable \
APP_ORIGIN=https://your-name.localhost \
BILIBI_AGENT_CREDENTIALS=/private/path/opening-agent.json \
  cargo run -p bilibi-finance-walkthrough-fixture --bin conversion -- launcher-credentials
```

The command grants the launcher's administrator a short-lived OAuth connection and never prints its token. After approving the opening asset register in the browser, run the same binary with `launcher-ordinary` and `BILIBI_AGENT_PROOF=disposable` on the same database. It adds an ordinary asset through the Ledger and Fixed Assets facades, so the Fixed assets list shows both sources.

For the focused Receivables paging proof, use another fresh disposable database:

```sh
cargo run -p bilibi-finance-walkthrough-fixture --bin conversion -- opening-list-scale
```

This mode stages and reconciles more than 2,000 opening invoices through the public facade, adds ordinary and nonmatching records, then checks filtered search, sorting, count, final-page clamping and lightweight page rows through `finance.receivables.document.page`. It exits before the full replacement lifecycle.

Run `conversion` without `serve` afterward for Company 003's facade proof:
source accumulation differing from lifetime projection, prospective depreciation,
settlement reversals, report reproduction across entryless imports, independent
approval, unauthorized-write refusal and permanent source evidence. It also
retains comparative, monthly movement, SST and payroll source schedules without
creating postings, returns, payments or tax lines. On Company 005
(`src/source_bridge.rs`) it reads the year-to-date bridge with no months
retained, with January to June agreeing to the opening journal and a balance
sheet row left out, and with June retained twice. `serve` and `prepare-launcher` retain
January to May for Company 001, so its Monthly movements tab shows the bridge. Around the staged 10,000-row
and replacement lifecycle (`src/conversion_lifecycle.rs`), it runs the immutable
conversion-report and sign-off proof described in [Conversion
reports](../../reports/README.md#conversion-reports). Use `-- report-browser` to
stop after preparing the unsigned browser report and before the bulk correction
proof. Each binary refuses ordinary production setup; these are accounting
walkthroughs, not production seeds or a historical transaction migration tool.

### Opening customer and supplier credits

Run `cargo run -p bilibi-finance-walkthrough-fixture --bin conversion -- credits` on a fresh disposable database for the focused proof. Omit `-- credits` to run the existing conversion scenarios and the credit proof together. The credit proof imports mixed, credit-only, partially settled, contra-sign and zero-net registers, exercises inactive saved controls and independent staged approval, and checks source tax facts and exact export evidence. It applies and reverses same-control credits without entries, transfers across controls, refunds and reverses owned entries, and replays all three report boundaries after later actions and a register correction. It also checks ordinary receipt/payment capacity, concurrent spending, posting rollback, closed periods and visible unrelated-owner differences.

For browser and MCP parity, use the normal Platform launcher on that disposable database. Set `BILIBI_DEV_HOST` to your own task's HTTPS localhost name in the Platform `.env`; run `cargo run -p bilibi-finance-walkthrough-fixture --bin conversion -- prepare-credits-launcher` with `BILIBI_OPENING_PROOF_CONFIG` naming a private output file, then `./bilibi start`. Sign in as the launcher's `ai@agent.my` administrator with the development password. Run the same Cargo command with `launcher-credentials` instead, setting `BILIBI_AGENT_PROOF=disposable`, `BILIBI_CREDITS_PROOF=disposable` and `BILIBI_AGENT_CREDENTIALS` to a new private file. This assigns that person's desks in Company 008 (browser) and Company 009 (MCP) and creates a disposable MCP grant. For these two single-person proof companies only, it records a reasoned change permitting same-person approvals; all such approvals still record exceptions. The facade proof keeps separation required and uses independent preparers and reviewers.

Use Company 008's existing Invoices and Bills lists to import the `ar` and `ap` CSVs from the configuration file. Classify each non-zero opening account and explicitly select the zero controls; review every source row, tax fact and reconciliation. On the existing credit records, exercise application, refund and reversal, reviewing the same party, controls, dates and posting decision as the MCP operations. A permitted same-person approval exception must have an explicit reason; it remains audited.

For a repeatable browser run, prepend `const proofConfig = <the Company 008 JSON object>;` to `conversion-credit-browser.js` and pass the combined script to `chrome-devtools-axi run` in your own browser session, after signing in and choosing Company 008. It uses visible controls for the mixed import, field refusal, reviewed application/refund/reversal and different-control transfer in both areas. Start with the prepared fixture; its references are permanent proof identities.

Run `conversion-credit-operations.py` with the same configuration and private credentials, `BILIBI_AGENT_PROOF=disposable` and `BILIBI_AGENT_URL=https://<your-task>.localhost`. It uses Company 009 to exercise both mixed imports, exact/changed retries, field refusals, stale and competing reviewed writes, dated record drill-downs, three-pin replay, transfers, refunds and owned reversals. It prints no credentials. Keep proof captures outside the repository. Run the mounted Platform `scripts/domain-ci.sh` on a separate empty disposable database.
