#!/usr/bin/env python3
"""H2 live HTTP/MCP walkthrough against the disposable agent-operations
bootstrap. All business writes use the wire API; SQL observes and injects one
late audit failure. No tokens or credentials are printed.
"""
import json
import os
from pathlib import Path
import subprocess
import urllib.error
import urllib.request

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
BASE = os.environ.get("BILIBI_AGENT_URL", "http://127.0.0.1:3100").rstrip("/")
CREDS = json.loads(Path(os.environ["BILIBI_AGENT_CREDENTIALS"]).read_text())
DB = os.environ["DATABASE_URL"]
CONTROL = os.environ.get("BILIBI_AGENT_CONTROL", "target/debug/receivables-control")


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def sql(query):
    return subprocess.check_output(["psql", DB, "-XAt", "-v", "ON_ERROR_STOP=1", "-c", query], text=True).strip()


require(sql("SELECT count(*) FROM users WHERE id=1 AND email='manager@example.test'") == "1", "not the disposable fixture")


def request(path, body=None, who="manager", channel="api"):
    headers = {"Authorization": "Bearer " + CREDS[who][channel], "Content-Type": "application/json"}
    if channel == "mcp":
        headers["Accept"] = "application/json, text/event-stream"
    req = urllib.request.Request(BASE + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as response:
        return json.load(response)


def invoke(channel, tool, args, who="manager"):
    if channel == "mcp":
        body = request("/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}, who, channel)
        # Storage/audit faults are an HTTP edge refusal, not an MCP tool reply.
        if body.get("error") == "temporarily_unavailable":
            return body
        require("result" in body, "MCP protocol refusal")
        return body["result"]["structuredContent"]
    if tool in ("guide", "describe"):
        return request("/api/v1/" + ("guides/" if tool == "guide" else "operations/") + args["key"], who=who)
    if tool == "read":
        return request("/api/v1/operations/" + args["key"] + "/call", {"input": args["input"]}, who)
    return request("/api/v1/plans" + ("/commit" if tool == "commit" else ""), args, who)


def op(key, company, **values):
    prefix = "finance." if key.startswith("ledger.") else "finance.receivables."
    return {"key": prefix + key, "input": {"company_id": company, **values}}


def read(channel, key, company, who="manager", **values):
    body = invoke(channel, "read", op(key, company, **values), who)
    require("result" in body, f"{key}: read failed: {body.get('error')}")
    return body["result"]


def plan(channel, ops):
    return invoke(channel, "plan", {"operations": ops})


def commit(channel, ops, review):
    return invoke(channel, "commit", {"operations": ops})


def reviewed(channel, ops):
    result = plan(channel, ops)
    require(result.get("valid") is True, f"preview refused: {result.get('results', result.get('error'))}")
    return result


def write(channel, key, company, **values):
    ops = [op(key, company, **values)]
    body = commit(channel, ops, reviewed(channel, ops))
    require("results" in body, f"{key}: commit refused: {body.get('error')}")
    return body["results"][0]["result"]["result"]["record"]


def refused(channel, key, company, contains, **values):
    body = plan(channel, [op(key, company, **values)])
    require(body.get("valid") is False and contains in json.dumps(body), f"{key}: expected refusal {contains}; got {body}")


def control(action, value):
    subprocess.run([CONTROL, action, str(value)], check=True, stdout=subprocess.DEVNULL)


def state(company):
    # Excludes lifecycle/decision logs, includes every persisted business row,
    # its mutation audit and queued retention work. Ordered row JSON also catches
    # replacements that leave counts unchanged.
    tables = ["setups", "setup_changes", "series", "tax_codes", "customers", "documents", "document_lines", "document_tax_lines", "payments", "allocations", "payment_reversals"]
    values = []
    for table in tables:
        values.append(sql(f"SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY id),'[]') FROM finance_receivables_{table} r WHERE company_id={company}"))
    for table in ["finance_ledger_entries", "finance_ledger_entry_lines"]:
        values.append(sql(f"SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY id),'[]') FROM {table} r WHERE company_id={company}"))
    values.append(sql("SELECT count(*) FROM base_audit_mutations"))
    values.append(sql("SELECT count(*) FROM base_queue_jobs"))
    return values


def sequences():
    return sql("SELECT jsonb_agg(jsonb_build_array(sequencename,last_value) ORDER BY sequencename) FROM pg_sequences WHERE schemaname='public' AND (sequencename LIKE 'finance_receivables_%' OR sequencename LIKE 'finance_ledger_entries_%')")


def rollback(channel, company, ops, expected="record_changed"):
    review = reviewed(channel, ops)
    before = state(company)
    result = commit(channel, ops, review)
    require(result.get("error") == expected, f"expected {expected}, got {result.get('error')}")
    require(state(company) == before, "partial business, posting, audit or job survived plan rollback")
    return review


for channel, company in [("api", 1), ("mcp", 2)]:
    print(f"\n== {channel}: receivables company {company}", flush=True)
    require(read(channel, "setup.get", company)["setup"] is None, "proof needs fresh receivables")
    require(invoke(channel, "guide", {"key": "finance.receivables"}), "guide missing")
    for key in ["setup.configure", "series.create", "series.update", "tax_code.create", "tax_code.update", "customer.create", "customer.update", "draft.create", "draft.update", "draft.discard", "document.issue", "payment.record", "payment.reverse"]:
        require(invoke(channel, "describe", {"key": "finance.receivables." + key}), f"{key} not described")
    require(invoke(channel, "read", op("customer.create", company, record_kind="company", record_id=25)).get("error") == "writes_not_enabled", "direct write accepted")
    write(channel, "ledger.book.open", company, currency_code="MYR", minor_units=2)
    accounts = {}
    for code, kind in [("control", "asset"), ("other-control", "asset"), ("deposit", "asset"), ("income", "income"), ("other-income", "income"), ("tax", "liability")]:
        accounts[code] = write(channel, "ledger.account.create", company, code=code, name=code, account_type=kind)["id"]
    year = write(channel, "ledger.fiscal_year.open", company, start_date="2026-01-01", end_date="2026-12-31")
    first_period = year["periods"][0]["id"]
    write(channel, "setup.configure", company, receivable_account_id=accounts["control"], income_account_id=accounts["income"])
    history = read(channel, "setup.changes", company)["records"]
    require(len(history) == 2 and all(r["changed_by_user_id"] == 1 for r in history), "setup history or actor missing")
    write(channel, "setup.configure", company)
    require(read(channel, "setup.changes", company)["records"] == history, "no-op setup wrote history")
    require(read(channel, "setup.changes", company, page_size=1, after=history[0]["id"])["records"] == history[1:], "history cursor skipped row")
    series = {}
    for kind, prefix in [("invoice", "INV-"), ("credit_note", "CR-"), ("payment", "PAY-")]:
        series[kind] = write(channel, "series.create", company, code=kind, name=kind, kind=kind, prefix=prefix, number_width=3)["id"]
    second_payment = write(channel, "series.create", company, code="payment2", name="Other receipts", kind="payment", prefix="P2-")["id"]
    write(channel, "series.update", company, series_id=second_payment, name="Other receipts renamed", active=False)
    write(channel, "series.update", company, series_id=second_payment, active=True)
    tax = write(channel, "tax_code.create", company, code="T", name="Tax", rate="7.5", account_id=accounts["tax"])
    tax = write(channel, "tax_code.update", company, tax_code_id=tax["id"], name="Tax renamed")
    require(tax["rate"] == "7.5000" or tax["rate"] == "7.5", "tax rate changed")
    # A customer is a Core company (#75): Company 025 of this tenant.
    refused(channel, "customer.create", company, "does not exist", record_kind="company", record_id=999999)
    refused(channel, "customer.create", company, "cannot be the current company itself", record_kind="company", record_id=company)
    refused(channel, "customer.create", company, "name", record_kind="company", record_id=25, name="not accepted")
    customer = write(channel, "customer.create", company, record_kind="company", record_id=25, payment_terms_days=7)
    require(customer["code"] == "company_025" and customer["party"]["id"] == 25 and customer["party"]["status"] == "active" and customer["name"] == customer["party"]["name"], "linked customer differs")
    require(customer["party"]["readable"] and customer["party"]["email"] and customer["party"]["tax_id"], "company facts not read")
    refused(channel, "customer.update", company, "name", customer_id=customer["id"], name="Customer at issue")
    refused(channel, "customer.create", company, "is already a customer of the current company", record_kind="company", record_id=25)
    refused(channel, "customer.update", company, "record_id", customer_id=customer["id"], record_kind="company", record_id=26)
    customer = write(channel, "customer.update", company, customer_id=customer["id"], payment_terms_days=7)
    require(read(channel, "customer.get", company, customer_id=customer["id"]) == customer, "customer read differs")
    party_name = customer["name"]
    refused(channel, "series.update", company, "prefix", series_id=second_payment, prefix="changed")
    refused(channel, "tax_code.update", company, "rate", tax_code_id=tax["id"], rate="8")
    refused(channel, "customer.update", company, "actor_id", customer_id=customer["id"], actor_id=7)
    print("setup history/no-op, series and tax create/update, normalized customer and strict immutable/actor schemas", flush=True)

    def draft(price="12.3456", tax_id=tax["id"]):
        return write(channel, "draft.create", company, kind="invoice", series_id=series["invoice"], customer_id=customer["id"], issue_date="2026-01-10", lines=[{"description": "First line", "quantity": "3", "unit_price": price, **({"tax_code_id": tax_id} if tax_id else {})}, {"description": "Second line", "quantity": "2", "unit_price": "4.015"}])

    empty = write(channel, "draft.create", company, kind="invoice", series_id=series["invoice"], customer_id=customer["id"])
    refused(channel, "document.issue", company, "issue_date", document_id=empty["id"])
    discarded = write(channel, "draft.discard", company, document_id=empty["id"])
    require(discarded["status"] == "discarded" and discarded["document_number"] is None, "discard numbered a draft")
    invoice = draft()
    require(invoice["subtotal"] == "45.07" and invoice["tax_total"] == "2.78" and invoice["total"] == "47.85", "per-line half-up amounts differ from independently derived values")
    child_ids = sql(f"SELECT array_agg(id ORDER BY id) FROM finance_receivables_document_lines WHERE document_id={invoice['id']}")
    kept = write(channel, "draft.update", company, document_id=invoice["id"], description="Header only")
    require(kept["lines"] == invoice["lines"] and sql(f"SELECT array_agg(id ORDER BY id) FROM finance_receivables_document_lines WHERE document_id={invoice['id']}") == child_ids, "omission rebuilt snapshots")
    dated = write(channel, "draft.update", company, document_id=invoice["id"], due_date="2026-01-30")
    cleared_dates = write(channel, "draft.update", company, document_id=invoice["id"], issue_date="", due_date="")
    require(cleared_dates["issue_date"] is None and cleared_dates["due_date"] is None and cleared_dates["lines"] == dated["lines"], "clearing dates lost saved snapshots")
    write(channel, "draft.update", company, document_id=invoice["id"], issue_date="2026-01-10")
    unrelated = [op("ledger.account.create", company, code="unrelated", name="Unrelated expense", account_type="expense"), op("draft.update", company, document_id=invoice["id"], description="Header with unrelated chart change")]
    require(len(commit(channel, unrelated, reviewed(channel, unrelated))["results"]) == 2, "unrelated chart change invalidated header-only update")
    unrelated = [op("setup.configure", company, income_account_id=accounts["other-income"]), op("draft.update", company, document_id=invoice["id"], description="Header with unrelated default change")]
    require(len(commit(channel, unrelated, reviewed(channel, unrelated))["results"]) == 2, "unused setup default invalidated header-only update")
    write(channel, "setup.configure", company, income_account_id=accounts["income"])
    disposable = draft("1", None)
    cleared = write(channel, "draft.update", company, document_id=disposable["id"], lines=[])
    require(cleared["total"] == "0" and cleared["lines"] == [], "empty array did not clear")
    replaced = write(channel, "draft.update", company, document_id=disposable["id"], lines=[{"description": "Replacement", "quantity": "1", "unit_price": "2.31"}])
    kept = write(channel, "draft.discard", company, document_id=replaced["id"])
    require(kept["total"] == "2.31" and kept["lines"] == replaced["lines"], "discard lost lines")
    refused(channel, "draft.update", company, "Not a draft", document_id=kept["id"], description="never")
    ops = [op("document.issue", company, document_id=invoice["id"])]
    before, ids = state(company), sequences()
    review = reviewed(channel, ops)
    reviewed(channel, ops)
    require(state(company) == before and sequences() == ids, "preview mutated business, audit, queue or identity state")
    require("INV-001" in json.dumps(review) and "2026-01-17" in json.dumps(review), "issue review omitted exact reference or defaulted due date")
    result = commit(channel, ops, review)
    require("results" in result, f"issuance failed: {result.get('error')}")
    invoice = result["results"][0]["result"]["result"]["record"]
    require(invoice["document_reference"] == "INV-001" and invoice["party_name"] == party_name and invoice["party_tax_identifier"] == customer["party"]["tax_id"] and invoice["due_date"] == "2026-01-17", "issue snapshots differ")
    entry = read(channel, "ledger.entry.get", company, entry_id=invoice["ledger_entry_id"])
    require([(l["account_id"], l["debit"], l["credit"]) for l in entry["lines"]] == [(accounts["control"], "47.85", "0"), (accounts["income"], "0", "45.07"), (accounts["tax"], "0", "2.78")], "owned posting sides/amounts differ")
    sql("UPDATE companies SET legal_name='Customer after issue' WHERE tenant_id=1 AND id=25")
    require(read(channel, "customer.get", company, customer_id=customer["id"])["name"] == "Customer after issue", "company rename not read")
    require(read(channel, "document.get", company, document_id=invoice["id"])["party_name"] == party_name, "issued party changed")
    print("draft keep/clear/replace/discard; repeated preview leaves rows/sequences untouched; INV-001 freezes due date/party and posts exact 47.85/45.07/2.78", flush=True)

    one, two = draft("1", None), draft("2", None)
    combined = [op("document.issue", company, document_id=d["id"]) for d in [one, two]]
    failed = rollback(channel, company, combined)
    require(commit(channel, combined, failed).get("error") == "record_changed", "invalid batch changed outcome on retry")
    one = write(channel, "document.issue", company, document_id=one["id"])
    two = write(channel, "document.issue", company, document_id=two["id"])
    require([one["document_reference"], two["document_reference"]] == ["INV-002", "INV-003"], "rollback consumed series numbers")
    closed_draft = draft("3", None)
    rollback(channel, company, [op("ledger.period.close", company, period_id=first_period), op("document.issue", company, document_id=closed_draft["id"])], "invalid_input")
    write(channel, "ledger.period.close", company, period_id=first_period)
    refused(channel, "document.issue", company, "closed period", document_id=closed_draft["id"])
    write(channel, "ledger.period.reopen", company, period_id=first_period, reason="Correction window")
    independent_issue = draft("4", None)
    unrelated = [op("ledger.fiscal_year.open", company, start_date="2027-01-01", end_date="2027-12-31"), op("document.issue", company, document_id=independent_issue["id"])]
    require(len(commit(channel, unrelated, reviewed(channel, unrelated))["results"]) == 2, "unrelated future year invalidated posting in existing open period")
    independent_issue = draft("5", None)
    unrelated = [op("setup.configure", company, income_account_id=accounts["other-income"]), op("document.issue", company, document_id=independent_issue["id"])]
    issued = commit(channel, unrelated, reviewed(channel, unrelated))["results"][1]["result"]["result"]["record"]
    posting = read(channel, "ledger.entry.get", company, entry_id=issued["ledger_entry_id"])
    require(any(l["account_id"] == accounts["income"] for l in posting["lines"]) and all(l["account_id"] != accounts["other-income"] for l in posting["lines"]), "changed income default replaced frozen draft account")
    write(channel, "setup.configure", company, income_account_id=accounts["income"])
    print("same-series multi-issue and mixed ledger-close/issue roll back business, entries, audit and code; separate issues number consecutively; closed-period preview refuses", flush=True)

    credit = write(channel, "draft.create", company, kind="credit_note", series_id=series["credit_note"], customer_id=customer["id"], credited_document_id=invoice["id"], issue_date="2026-01-11", lines=[{"description": "Credit", "quantity": "1", "unit_price": "5"}])
    credit = write(channel, "document.issue", company, document_id=credit["id"])
    require(credit["document_reference"] == "CR-001" and read(channel, "document.get", company, document_id=invoice["id"])["outstanding"] == "42.85", "credit did not settle original invoice")
    write(channel, "setup.configure", company, receivable_account_id=accounts["other-control"])
    second = write(channel, "draft.create", company, kind="invoice", series_id=series["invoice"], customer_id=customer["id"], issue_date="2026-01-12", lines=[{"description": "Other control", "quantity": "1", "unit_price": "31.11"}])
    second = write(channel, "document.issue", company, document_id=second["id"])
    payment_input = {"series_id": series["payment"], "customer_id": customer["id"], "payment_date": "2026-01-15", "amount": "28.96", "deposit_account_id": accounts["deposit"], "allocations": [{"document_id": invoice["id"], "amount": "17.85"}, {"document_id": second["id"], "amount": "11.11"}]}
    refused(channel, "payment.record", company, "decimal places", **{**payment_input, "amount": "28.961"})
    refused(channel, "payment.record", company, "allocations.1.document_id", **{**payment_input, "allocations": [{"document_id": invoice["id"], "amount": "17.85"}, {"document_id": invoice["id"], "amount": "11.11"}]})
    write(channel, "customer.update", company, customer_id=customer["id"], active=False)
    pop = [op("payment.record", company, **payment_input)]
    before, ids = state(company), sequences()
    pre = reviewed(channel, pop)
    require(state(company) == before and sequences() == ids, "payment preview reserved an identity")
    payment = commit(channel, pop, pre)["results"][0]["result"]["result"]["record"]
    pentry = read(channel, "ledger.entry.get", company, entry_id=payment["ledger_entry_id"])
    require({l["account_id"]: (l["debit"], l["credit"]) for l in pentry["lines"]} == {accounts["deposit"]: ("28.96", "0"), accounts["control"]: ("0", "17.85"), accounts["other-control"]: ("0", "11.11")}, "payment used current setup instead of original controls")
    require(read(channel, "document.get", company, document_id=invoice["id"])["outstanding"] == "25", "partial allocation wrong")
    control("deny", "finance.ledger.view")
    try:
        reversed_payment = write(channel, "payment.reverse", company, payment_id=payment["id"], reversal_date="2026-01-16", reason="Correct allocation")
    finally:
        control("allow", "finance.ledger.view")
    require(reversed_payment["reversal"]["reason"] == "Correct allocation", "owned reversal missing")
    require(read(channel, "document.get", company, document_id=invoice["id"])["outstanding"] == "42.85", "reversal did not restore balance")
    refused(channel, "payment.reverse", company, "Already reversed", payment_id=payment["id"], reversal_date="2026-01-17", reason="Twice")
    require(read(channel, "payment.get", company, payment_id=payment["id"]) == reversed_payment, "receipt read lost reversal")
    small = {**payment_input, "amount": "1", "allocations": [{"document_id": invoice["id"], "amount": "1"}]}
    rollback(channel, company, [op("payment.record", company, **small), op("payment.record", company, **{**small, "series_id": second_payment})])
    print("credit note; inactive-customer multi-control receipt; precision/duplicate-allocation refusals; reversal without ledger view restores 42.85; outstanding-only intra-plan drift rolls back", flush=True)

    # Audit capture fails after entry posting. Its rollback must undo both the
    # preceding independent customer update and every issue/ledger mutation.
    write(channel, "customer.update", company, customer_id=customer["id"], active=True)
    ops = [op("customer.update", company, customer_id=customer["id"], payment_terms_days=9), op("document.issue", company, document_id=closed_draft["id"])]
    # A customer change affects issuance review, so use a DIFFERENT customer
    # for the first step to reach the intended late capture failure.
    independent = write(channel, "customer.create", company, record_kind="company", record_id=31)
    ops[0] = op("customer.update", company, customer_id=independent["id"], payment_terms_days=9)
    review = reviewed(channel, ops)
    before = state(company)
    sql("CREATE FUNCTION h2_fail_capture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.auditable_type='finance.receivables.document' AND NEW.event='updated' THEN RAISE EXCEPTION 'H2 disposable capture failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER h2_fail_capture BEFORE INSERT ON base_audit_mutations FOR EACH ROW EXECUTE FUNCTION h2_fail_capture()")
    try:
        result = commit(channel, ops, review)
        require("error" in result, "audit failure accepted")
    finally:
        sql("DROP TRIGGER h2_fail_capture ON base_audit_mutations; DROP FUNCTION h2_fail_capture()")
    require(state(company) == before, "late capture failure escaped outer transaction")
    require("results" in commit(channel, ops, review), "same operations did not recover after capture repair")

    # A successful interleaving exercises both FIFOs without shared facts.
    ops = [op("customer.update", company, customer_id=independent["id"], payment_terms_days=1), op("ledger.account.create", company, code="independent", name="Independent", account_type="asset"), op("customer.update", company, customer_id=customer["id"], payment_terms_days=2)]
    require(len(commit(channel, ops, reviewed(channel, ops))["results"]) == 3, "independent mixed-module batching failed")
    print("late audit failure rolls back earlier independent step and owned posting; same operations recover; independent interleaved H1/H2 plan succeeds", flush=True)

    # Real second-session authority changes via the fixture's normal facades.
    pending = write(channel, "draft.create", company, kind="invoice", series_id=series["invoice"], customer_id=customer["id"], issue_date="2026-01-20", lines=[{"description": "Authority check", "quantity": "1", "unit_price": "3.21"}])
    ops = [op("document.issue", company, document_id=pending["id"])]
    review = reviewed(channel, ops)
    control("deny", "finance.receivables.documents.issue")
    try:
        require("error" in commit(channel, ops, review), "revoked capability allowed commit")
        require(read(channel, "document.get", company, document_id=pending["id"])["status"] == "draft", "revoked write survived")
    finally:
        control("allow", "finance.receivables.documents.issue")
    # Lack of source view is not a new issue prerequisite.
    control("deny", "finance.receivables.view")
    try:
        issued = write(channel, "document.issue", company, document_id=pending["id"])
        require(issued["retention_refresh"] == "error: unauthorized", "issue without source view was not preserved")
    finally:
        control("allow", "finance.receivables.view")
    require("error" in invoke(channel, "read", op("document.get", company, document_id=invoice["id"]), "other"), "cross-tenant disclosure")
    # Customer cursors follow Finance IDs; codes are read from Core.
    for linked in [26, 28, 29]:
        write(channel, "customer.create", company, record_kind="company", record_id=linked)
    expected = json.loads(sql(f"SELECT json_agg(id ORDER BY id) FROM finance_receivables_customers WHERE company_id={company}"))
    found, cursor = [], None
    while True:
        page = read(channel, "customer.list", company, page_size=1, **({"after": cursor} if cursor is not None else {}))
        found.extend(r["id"] for r in page["records"])
        cursor = page["next_after_id"]
        if cursor is None:
            break
        require(len(found) <= len(expected), "customer cursor failed to advance")
    require(found == expected, "customer cursor dropped or repeated rows")
    for key, values in [("series.list", {}), ("tax_code.list", {}), ("document.list", {"status": "issued", "kind": "invoice", "customer_id": customer["id"]}), ("payment.list", {"customer_id": customer["id"]})]:
        require(read(channel, key, company, page_size=1, **values)["records"], f"{key} empty")
    rows, cursor = [], None
    while True:
        page = read(channel, "document.list", company, page_size=2, **({"after": cursor} if cursor else {}))
        rows.extend(r["id"] for r in page["records"])
        cursor = page["next_after"]
        if not page["records"]:
            break
    expected = json.loads(sql(f"SELECT json_agg(id ORDER BY id) FROM finance_receivables_documents WHERE company_id={company}"))
    require(rows == expected, "document ID cursor skipped or duplicated records")
    print("grant revoked after preview refused; issue without source view preserved; cross-tenant refusal; ICU code and document-ID cursors traverse exactly", flush=True)

# Archival happens through another real facade session after review.
company = 2
channel = "mcp"
customer = read(channel, "customer.list", company)["records"][0]
ops = [op("customer.update", company, customer_id=customer["id"], payment_terms_days=8)]
review = reviewed(channel, ops)
control("archive", company)
require("error" in commit(channel, ops, review), "archived target accepted commit")
require(read(channel, "customer.get", company, customer_id=customer["id"])["phone"] != "archive fence", "archived write survived")
print("\nArchived target changed in a second session refuses a previously reviewed commit; historical reads remain available.")
print("HTTP and MCP Receivables walkthrough completed: 13 safe writes and 10 reads, exact amounts, refusals and atomic rollback observed.")
