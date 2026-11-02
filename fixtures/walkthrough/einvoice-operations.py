#!/usr/bin/env python3
"""H4 real-stack proof, mirroring Bilimbi Finance.Einvoice linked issuance.

Fresh disposable database only: agent-operations bootstrap, then the
einvoice-operations binary, then start the real app and run this script.
BILIBI_AGENT_CREDENTIALS and BILIBI_EINVOICE_FIXTURE name their output files.
No secrets are printed. SQL observes except the explicitly
guarded, finally-cleaned late audit fault below. Never run on shared data.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
import urllib.error
import urllib.request
from decimal import Decimal, ROUND_HALF_UP

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
BASE = os.environ.get("BILIBI_AGENT_URL", "http://127.0.0.1:3100").rstrip("/")
with open(os.environ["BILIBI_AGENT_CREDENTIALS"]) as f:
    CREDS = json.load(f)
with open(os.environ["BILIBI_EINVOICE_FIXTURE"]) as f:
    FIXTURE = json.load(f)
DB = os.environ["DATABASE_URL"]


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def sql(query):
    return subprocess.check_output(
        ["psql", DB, "-XAt", "-v", "ON_ERROR_STOP=1", "-c", query],
        text=True, stderr=subprocess.DEVNULL,
    ).strip()


require(sql("SELECT count(*) FROM users WHERE tenant_id=1 AND id=1 AND email='manager@example.test'") == "1", "not the disposable fixture")


def request(path, data=None, who="manager", channel="api"):
    headers = {"Authorization": "Bearer " + CREDS[who][channel], "Content-Type": "application/json"}
    if channel == "mcp":
        headers["Accept"] = "application/json, text/event-stream"
    req = urllib.request.Request(BASE + path, data=None if data is None else json.dumps(data).encode(), headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return json.load(response)


def invoke(channel, tool, args, who="manager"):
    if channel == "mcp":
        body = request("/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}, who, channel)
        if body.get("error") == "temporarily_unavailable":
            return body
        require("result" in body, "MCP protocol refusal")
        return body["result"]["structuredContent"]
    if tool in ("describe", "guide"):
        return request("/api/v1/" + ("guides/" if tool == "guide" else "operations/") + args["key"], who=who)
    path = {"plan": "/api/v1/plans", "commit": "/api/v1/plans/commit"}.get(tool)
    if tool == "read":
        path, args = "/api/v1/operations/" + args["key"] + "/call", {"input": args["input"]}
    return request(path, args, who)


def op(data, key="issue"):
    return {"key": "finance.einvoice.document." + key, "input": copy.deepcopy(data)}


def plan(channel, operations):
    return invoke(channel, "plan", {"operations": operations})


def commit(channel, operations, review):
    require(review.get("valid") is True, "positive preview refused: " + str(review.get("results", review.get("error"))))
    result = invoke(channel, "commit", {"operations": operations})
    if "error" not in result and "results" in result:
        for operation, preview, written in zip(operations, review["results"], result["results"], strict=True):
            if operation["key"] == "finance.einvoice.document.issue":
                verify_review(preview["preview"]["result"]["changes"], written["result"]["result"]["record"])
    return result


def records(body):
    require("error" not in body and "results" in body, "positive commit refused: " + str(body.get("error")))
    values = []
    for result in body["results"]:
        result = result["result"]["result"]
        require(result["path"] is None, "issue unexpectedly navigates")
        values.append(result["record"])
    return values


def read(channel, company, key, who="manager", **kwargs):
    return invoke(channel, "read", op({"company_id": company, **kwargs}, key), who)


def control(*args):
    platform = Path(__file__).resolve().parents[5]
    binary = Path(os.environ.get("CARGO_TARGET_DIR", platform / "target")) / "debug" / "einvoice-operations"
    subprocess.run([str(binary), *map(str, args)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


TABLES = ["documents", "document_lines", "status_events", "source_links", "submissions", "consolidated_sources"]


def state():
    # Snapshot complete rows (not just counts). Preview must not allocate
    # business IDs.
    queries = [f"(SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.id),'[]'::jsonb) FROM finance_einvoice_{table} t)" for table in TABLES]
    queries += ["(SELECT count(*) FROM finance_ledger_entries)", "(SELECT count(*) FROM finance_ledger_accounts)", "(SELECT count(*) FROM base_queue_jobs)", "(SELECT count(*) FROM base_audit_mutations)", "(SELECT count(*) FROM base_audit_actions)"]
    return sql("SELECT jsonb_build_array(" + ",".join(queries) + ")")


def sequences():
    return [sql(f"SELECT last_value,is_called FROM finance_einvoice_{table}_id_seq") for table in TABLES]


def passive():
    return sql("SELECT (SELECT count(*) FROM finance_ledger_entries),(SELECT count(*) FROM finance_einvoice_submissions),(SELECT count(*) FROM base_queue_jobs)")


def refused(channel, data, field=None):
    before = state(), sequences()
    review = plan(channel, [op(data)])
    require(review.get("valid") is False, "invalid issue preview accepted")
    if field:
        require(field in json.dumps(review), "refusal omitted expected field " + field)
    require((state(), sequences()) == before, "refused preview allocated or mutated")


def verify(view, data):
    doc = view["document"]
    require(doc["code_number"] == data["code_number"] and doc["issued_at"] == data["issued_at"], "document identity/instant differs")
    require(view["source_links"] == [data["source"]] and view["recognised_by_source"], "source link lost")
    require(view["ledger_entry_id"] is None and not view["submission_queued"] and view["submission_uid"] is None, "issue posted or submitted")
    rounding = lambda amount: amount.quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)
    net, tax = Decimal(0), Decimal(0)
    for source, line in zip(data["lines"], doc["lines"], strict=True):
        subtotal = rounding(Decimal(source["quantity"]) * Decimal(source["unit_price"]))
        amount = subtotal - Decimal(source["discount_amount"]) + Decimal(source["charge_amount"])
        line_tax = rounding(amount * Decimal(source["tax_rate"]) / 100)
        for key, expected in [("subtotal", subtotal), ("line_extension_amount", amount), ("tax_amount", line_tax)]:
            require(Decimal(line[key]) == expected, "independent line arithmetic differs: " + key)
        net += amount
        tax += line_tax
    require((net, tax, net + tax) == (Decimal("47.60"), Decimal("2.86"), Decimal("50.46")), "fixture arithmetic changed")
    for key, expected in [("line_extension_amount", net), ("tax_exclusive_amount", net), ("tax_amount", tax), ("tax_inclusive_amount", net + tax), ("payable_amount", net + tax)]:
        require(Decimal(doc[key]) == expected, "independent total differs: " + key)
    counterparty = "supplier" if data["document_type"] == "11" else "buyer"
    require(doc[counterparty]["name"] == data[counterparty]["name"], "party snapshot lost")


def verify_review(prepared, view):
    decimal_fields = {"exchange_rate", "quantity", "unit_price", "discount_amount", "charge_amount", "subtotal", "line_extension_amount", "tax_rate", "tax_amount"}
    def equal(key, before, after):
        if key in decimal_fields and before is not None:
            return Decimal(before) == Decimal(after)
        return before == after
    doc = view["document"]
    for key in ["environment", "supplier", "buyer", "details"]:
        require(prepared[key] == doc[key], "persisted snapshot differs from review: " + key)
    require(view["source_links"] == [prepared["source"]], "persisted source differs from review")
    detail_fields = {"payment_means_code", "payment_terms", "billing_period_start", "billing_period_end", "billing_period_description"}
    for key, value in prepared["valid"].items():
        if key not in {"buyer", "supplier", "lines"}:
            stored = doc["details"].get(key) if key in detail_fields else doc[key]
            require(equal(key, value, stored), "persisted value differs from review: " + key)
    for reviewed_line, stored_line in zip(prepared["valid"]["lines"], doc["lines"], strict=True):
        for key, value in reviewed_line.items():
            require(equal(key, value, stored_line[key]), "persisted line differs from review: " + key)


def failed_commit(channel, operations, review, expected=None):
    before = state()
    result = commit(channel, operations, review)
    require("error" in result and (expected is None or result["error"] == expected), "expected commit refusal")
    require(state() == before, "failed commit retained business/audit rows or consumed code")
    return result


for channel, fixture in zip(["api", "mcp"], FIXTURE, strict=True):
    company = fixture["company_id"]
    inputs = fixture["inputs"]
    print(f"H4 {channel}: company {company}", flush=True)
    require(read(channel, company, "list")["result"]["records"] == [], "needs fresh H4 database")
    require(invoke(channel, "guide", {"key": "finance.einvoice"}), "guide missing")
    for key in ["issue", "get", "list"]:
        require(invoke(channel, "describe", {"key": "finance.einvoice.document." + key}), "operation schema missing")
    require(invoke(channel, "read", op(inputs[0])).get("error") == "writes_not_enabled", "direct write enabled")

    for field, value in [("unknown", True), ("actor_id", 1), ("totals", {}), ("payable_amount", "50.46"), ("exchange_rate", 1), ("issued_at", None), ("issued_at", "2026-01-10T04:05:06.1Z"), ("source", None), ("lines", None)]:
        bad = copy.deepcopy(inputs[0]); bad[field] = value
        refused(channel, bad, field)
    for field in ["source", "issued_at"]:
        bad = copy.deepcopy(inputs[0]); del bad[field]
        refused(channel, bad, field)
    for field, value in [("tax_amount", "1"), ("unit_price", 13.29), ("quantity", None), ("tax_rate", "garbage"), ("unknown", "x")]:
        bad = copy.deepcopy(inputs[0]); bad["lines"][0][field] = value
        refused(channel, bad, field)
    for source in [{}, {**inputs[0]["source"], "ledger_entry_id": 999999999}, {**inputs[0]["source"], "ledger_entry_id": inputs[1]["source"]["ledger_entry_id"]}, {**inputs[0]["source"], "record": "document:999999999"}]:
        bad = copy.deepcopy(inputs[0]); bad["source"] = source
        refused(channel, bad, "source")
    bad = copy.deepcopy(inputs[0]); bad["lines"][0]["unit_price"] = "13.30"
    refused(channel, bad, "source")
    for quantity, price in [("10000000000000000", "10000000000000000"), ("100000000000000", "0"), ("99999999999999", "99999999999999")]:
        bad = copy.deepcopy(inputs[0]); bad["lines"].append({**bad["lines"][0], "quantity": quantity, "unit_price": price, "discount_amount": "0", "charge_amount": "0"})
        refused(channel, bad, "quantity")
    for rate in ["1.0000001", "100000000000000"]:
        bad = copy.deepcopy(inputs[0]); bad["exchange_rate"] = rate
        refused(channel, bad, "exchange_rate")

    operations = [op(inputs[0])]
    before = state(), sequences()
    prior_review = None
    for _ in range(3):
        review = plan(channel, operations)
        require(review.get("valid") is True, "valid repeated preview refused")
        if prior_review is not None:
            require(review["results"] == prior_review, "review facts drifted without input/context changes")
        prior_review = review["results"]
        require((state(), sequences()) == before, "preview wrote rows/audit or allocated business sequence")
    control("grant", "finance.einvoice.view", "false")
    quiet = passive()
    try:
        first = records(commit(channel, operations, review))[0]
        require("error" in read(channel, company, "get", document_id=first["document"]["id"]), "read accepted without view")
    finally:
        control("grant", "finance.einvoice.view", "true")
    verify(first, inputs[0])
    require(passive() == quiet, "issue added ledger entry/submission/job")
    require(read(channel, company, "get", document_id=first["document"]["id"])["result"] == first, "get differs from committed full view")
    for target, who in [(3 - company, "manager"), (company, "other")]:
        require("error" in read(channel, target, "get", who, document_id=first["document"]["id"]), "cross-company/tenant get disclosed")
    require("error" in read(channel, company, "list", "other"), "cross-tenant list disclosed")

    # Both steps preview successfully before either executes. The second
    # code collision must roll back the first issue and all its audit rows.
    duplicate = copy.deepcopy(inputs[2]); duplicate["code_number"] = inputs[1]["code_number"]
    operations = [op(inputs[1]), op(duplicate)]
    review = plan(channel, operations)
    failed_commit(channel, operations, review)
    failed_commit(channel, operations, review)  # failed plan is not consumed

    # Interleave non-Einvoice steps so the per-module review fence must
    # retain FIFO order, rather than one replaceable transaction-local slot.
    operations = [op(inputs[1]), {"key": "finance.ledger.account.create", "input": {"company_id": company, "code": "H4-FIFO", "name": "H4 interleaved", "account_type": "asset"}}, op(inputs[2])]
    review = plan(channel, operations)
    quiet = passive()
    views = records(commit(channel, operations, review))
    verify(views[0], inputs[1]); verify(views[2], inputs[2])
    require(passive() == quiet, "interleaved issue posted/submitted/queued")

    operations = [op(inputs[3])]
    review = plan(channel, operations)
    control("profile", company, "Changed after review")
    failed_commit(channel, operations, review, "record_changed")
    control("profile", company, "Retail")
    control("environment", company, "production")
    failed_commit(channel, operations, review, "record_changed")
    control("environment", company, "sandbox")
    # Same unconsumed code recovers when its reviewed setting is restored.
    verify(records(commit(channel, operations, review))[0], inputs[3])
    for capability in ["finance.einvoice.documents.issue", "finance.ledger.view"]:
        operations = [op(inputs[4])]
        review = plan(channel, operations)
        control("grant", capability, "false")
        try:
            failed_commit(channel, operations, review)
            refused(channel, inputs[4])
        finally:
            control("grant", capability, "true")
        # The first capability test deliberately leaves this source free.
    verify(records(commit(channel, operations, review))[0], inputs[4])

    # Explicitly disposable fault injection only: fail the audit capture
    # AFTER document writes, not an early business validation. No business SQL.
    operations = [op(inputs[5])]
    review = plan(channel, operations)
    require(sql("SELECT to_regprocedure('public.h4_einvoice_audit_fault()') IS NULL") == "t", "existing fault function; refusing to replace it")
    sql(f"""CREATE FUNCTION public.h4_einvoice_audit_fault() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN
      IF EXISTS (SELECT 1 FROM finance_einvoice_documents WHERE tenant_id=1 AND company_id={company} AND code_number='H4-{company}-5') THEN
        RAISE EXCEPTION 'disposable H4 late audit fault';
      END IF;
      RETURN NEW;
    END $$;
    CREATE TRIGGER h4_einvoice_audit_fault BEFORE INSERT ON base_audit_mutations
    FOR EACH ROW EXECUTE FUNCTION public.h4_einvoice_audit_fault();""")
    try:
        failed_commit(channel, operations, review)
    finally:
        sql("DROP TRIGGER h4_einvoice_audit_fault ON base_audit_mutations; DROP FUNCTION public.h4_einvoice_audit_fault()")
    verify(records(commit(channel, operations, review))[0], inputs[5])

    # Generic source ownership also permits a payment record. A source
    # reversal earlier in a plan must invalidate the later issue atomically.
    operations = [op(inputs[8])]
    verify(records(commit(channel, operations, plan(channel, operations)))[0], inputs[8])
    reverse = {"key": "finance.receivables.payment.reverse", "input": {"company_id": company, "payment_id": fixture["payment_id"], "reversal_date": "2026-01-11", "reason": "H4 source reversal"}}
    another = copy.deepcopy(inputs[8]); another["code_number"] += "-correction"
    operations = [reverse, op(another)]
    review = plan(channel, operations)
    failed_commit(channel, operations, review)
    operations = [reverse]
    records(commit(channel, operations, plan(channel, operations)))
    refused(channel, another, "source")

    expected = json.loads(sql(f"SELECT json_agg(id ORDER BY id DESC) FROM finance_einvoice_documents WHERE tenant_id=1 AND company_id={company}"))
    ids, before_id = [], None
    while True:
        args = {"page_size": 1}
        if before_id is not None:
            args["before"] = before_id
        page = read(channel, company, "list", **args)["result"]
        require(len(page["records"]) <= 1, "page bound ignored")
        ids.extend(row["id"] for row in page["records"])
        cursor = page["next_before"]
        if cursor is None:
            break
        require(cursor == ids[-1] and (before_id is None or cursor < before_id) and len(ids) < 100, "exclusive newest-first cursor did not advance")
        before_id = cursor
    require(ids == expected, "list skipped/repeated/reordered records")
    require([r["id"] for r in read(channel, company, "list")["result"]["records"]] == expected, "default list differs")
    require([r["id"] for r in read(channel, company, "list", page_size=100)["result"]["records"]] == expected, "maximum list differs")
    by_code = {data["code_number"]: data for data in inputs}
    for document_id in expected:
        view = read(channel, company, "get", document_id=document_id)["result"]
        verify(view, by_code[view["document"]["code_number"]])
    for size in [0, 101]:
        require("error" in read(channel, company, "list", page_size=size), "invalid page bound accepted")
    require("error" in read(channel, company, "list", before=9223372036854775808), "overflowing cursor silently ignored")
    if company == 2:
        operations = [op(inputs[6])]
        review = plan(channel, operations)
        control("archive")
        failed_commit(channel, operations, review)
        refused(channel, inputs[6])
    print("Validated arithmetic, full views, source identity, inert previews, rollback/FIFO, drift, capability and archive fences, audit fault, cursors and isolation.", flush=True)

print("H4 HTTP/MCP real-stack proof completed.", flush=True)
