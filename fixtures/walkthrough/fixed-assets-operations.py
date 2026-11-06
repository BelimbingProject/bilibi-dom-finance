#!/usr/bin/env python3
"""Live Fixed Assets lifecycle MCP walkthrough, not a unit-test runner. Use only the disposable
database and private credentials provisioned by the agent-operations binary.
Every mutation goes through the real wire edge; SQL observes its effects.
Tokens never appear in output. See the ledger README.
"""
import json
from decimal import Decimal
import os
import subprocess
import time
import urllib.error
import urllib.request
import urllib.parse
import socket

_dns_lookup = socket.getaddrinfo
def _local_lookup(host, port, *args, **kwargs):
    return _dns_lookup("127.0.0.1" if isinstance(host, str) and host.endswith(".localhost") else host, port, *args, **kwargs)
socket.getaddrinfo = _local_lookup

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
BASE = os.environ.get("BILIBI_AGENT_URL", "http://127.0.0.1:8147").rstrip("/")
CREDS = json.load(open(os.environ["BILIBI_AGENT_CREDENTIALS"]))
DB = os.environ["DATABASE_URL"]


def psql(query):
    # Keep credentials in the subprocess environment, never its arguments or
    # a CalledProcessError's printed command line.
    address = urllib.parse.urlsplit(DB)
    environment = os.environ | {
        "PGHOST": address.hostname or "localhost",
        "PGPORT": str(address.port or 5432),
        "PGUSER": urllib.parse.unquote(address.username or ""),
        "PGPASSWORD": urllib.parse.unquote(address.password or ""),
        "PGDATABASE": urllib.parse.unquote(address.path.lstrip("/")),
    }
    return ["psql", "-XAt", "-v", "ON_ERROR_STOP=1", "-c", query], environment


def sql(query):
    command, environment = psql(query)
    return subprocess.check_output(command, text=True, env=environment).strip()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


require(sql("SELECT count(*) FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=1 AND i.email='manager@example.test'") == "1", "not the disposable fixture")


def request(path, body=None, who="manager", channel="api"):
    headers = {"Authorization": "Bearer " + CREDS[who], "Content-Type": "application/json"}
    if channel == "mcp":
        headers["Accept"] = "application/json, text/event-stream"
    req = urllib.request.Request(BASE + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as response:
        return response.code, json.load(response)


def invoke(channel, tool, args, who="manager"):
    if channel == "mcp":
        status, body = request("/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}, who, channel)
        require(status == 200, f"MCP transport {status}")
        require("result" in body, "MCP protocol refusal")
        return body["result"]["structuredContent"]
    paths = {"plan": "/api/v1/plans", "commit": "/api/v1/plans/commit"}
    if tool == "read":
        path = "/api/v1/operations/" + args["key"] + "/call"
        args = {"input": args["input"]}
    elif tool in ("guide", "describe"):
        segment = "guides" if tool == "guide" else "operations"
        return request("/api/v1/" + segment + "/" + args["key"], who=who)[1]
    else:
        path = paths[tool]
    return request(path, args, who)[1]


def operation(key, company, **values):
    return {"key": key, "input": {"company_id": company, **values}}


def read(channel, key, company, who="manager", **values):
    op = operation(key, company, **values)
    body = invoke(channel, "read", op, who)
    require("result" in body, f"{key}: read failed: {body.get('error')}")
    return body["result"]


def plan(channel, ops, who="manager"):
    return invoke(channel, "plan", {"operations": ops}, who)


def commit(channel, ops, review, who="manager"):
    return invoke(channel, "commit", {"operations": ops}, who)


def write(channel, key, company, who="approver", **values):
    ops = [operation(key, company, **values)]
    review = plan(channel, ops, who)
    require(review.get("valid") is True, f"{key}: preview refused: {review.get('results', review.get('error'))}")
    body = commit(channel, ops, review, who)
    require("results" in body, f"{key}: commit refused: {body.get('error')}")
    return body["results"][0]["result"]["result"]["record"]


def refused(channel, key, company, contains, who="manager", **values):
    body = plan(channel, [operation(key, company, **values)], who)
    require(body.get("valid") is False and contains in json.dumps(body), f"{key}: expected refusal {contains}")


# Use the conversion fixture's clean Company 002; browser proof uses Company 001.
channel, company = "mcp", 2
prefix = "finance.fixed_assets."
keys = ["category.list", "category.save", "register.get", "schedule.get", "asset.get", "asset.acquire", "run.list", "run.post", "run.reverse", "asset.dispose", "disposal.reverse", "acquisition.void"]
for key in keys:
    require(invoke(channel, "describe", {"key": prefix + key}), "missing lifecycle schema")
require(invoke(channel, "guide", {"key": prefix + "lifecycle"}), "missing lifecycle guide")
accounts = {a["code"]: a["id"] for a in read(channel, "finance.ledger.account.list", company, "approver")["records"]}
category = dict(name="MCP prospective equipment", cost_account_id=accounts["1200"], accumulated_account_id=accounts["1290"], expense_account_id=accounts["6100"], gain_account_id=accounts["4100"], loss_account_id=accounts["6000"], method="straight_line", life_months=120, reason="Approved prospective defaults")
existing = next((c for c in read(channel, prefix + "category.list", company, "approver")["records"] if c["name"] == category["name"]), None)
if existing is None:
    refused(channel, prefix + "category.save", company, "accumulated_account_id", who="approver", **(category | {"accumulated_account_id": accounts["1200"]}))
    revision = write(channel, prefix + "category.save", company, **category)
    revised = {k: v for k, v in category.items() if k != "name"} | {"category_id": revision["category_id"], "life_months": 144, "reason": "New defaults leave saved assets frozen"}
    write(channel, prefix + "category.save", company, **revised)
else:
    revision = existing["revisions"][0]
    revised = {k: v for k, v in category.items() if k != "name"} | {"category_id": revision["category_id"], "life_months": 144, "reason": "New defaults leave saved assets frozen"}
context = read(channel, prefix + "opening.context", company, "approver")
csv = ",".join(context["header"]) + f"\nasset-111,Prospective MCP equipment,{revision['id']},2025-01-01,2025-01-01,11000,800,96,0\nasset-112,Fully depreciated MCP equipment,{revision['id']},2024-01-01,2024-01-01,1000,1000,0,0\n"
evidence = [{"source_id": "asset-111",
    "history": [{"kind": "residual_value_change", "effective_date": "2026-06-30", "residual_value": "0.00", "reference": "MCP source review", "note": "Source residual estimate revised"}],
    "source_period_expense": [{"from": "2026-01-01", "through": "2026-06-30", "amount": "600.00", "reference": "MCP YTD bridge"}],
    "tax_capital_allowances": [{"from": "2025-01-01", "through": "2025-12-31", "qualifying_expenditure": "12000.00", "allowances": "2400.00", "reference": "MCP tax evidence"}]
}]
opening = dict(source_reference="MCP-ASSET-111", currency_code="MYR", csv=csv, source_evidence=evidence)
refused(channel, prefix + "opening.import", company, "source_evidence", who="approver", **(opening | {"source_evidence": [{"source_id": "unknown"}]}))
review = plan(channel, [operation(prefix + "opening.import", company, **opening)], "approver")
require(review.get("valid") is True, "opening evidence review")
facts = review["results"][0]["preview"]["result"]["changes"]
if facts.get("already_imported"):
    facts = next(d["review"] for d in context["decisions"] if d["review"].get("source_reference") == "MCP-ASSET-111")
require(len(facts["class_totals"]) == 1 and Decimal(facts["class_totals"][0]["cost"]) == 12000, "class/account totals")
require("mid-month" in facts["month_convention"], "cut-off convention missing")
require("results" in commit(channel, [operation(prefix + "opening.import", company, **opening)], review, "approver"), "opening evidence commit")
write(channel, prefix + "opening.import", company, **opening)
changed = json.loads(json.dumps(evidence))
changed[0]["history"][0]["note"] = "Different source evidence"
refused(channel, prefix + "opening.import", company, "source_reference", who="approver", **(opening | {"source_evidence": changed}))

def report(**pins):
    return read(channel, prefix + "register.get", company, "approver", as_of="2026-07-31", **pins)["report"]

def tied(report):
    require(all(Decimal(r["difference"]) == 0 for r in report["reconciliation"]), "register reconciliation difference")

initial = report()
asset = initial["rows"][0]["asset"]
require(asset["category_revision_id"] == revision["id"] and asset["assumptions"]["life_months"] == 96, "opening assumptions not frozen")
require(Decimal(initial["rows"][0]["accumulated"]) == 800 and Decimal(initial["rows"][0]["carrying_value"]) == 10200, "opening source balances")
require(Decimal(initial["rows"][0]["schedule"][0]["amount"]) == Decimal("106.25"), "prospective monthly estimate")
tied(initial)
zero = initial["rows"][1]["asset"]
require(initial["rows"][1]["schedule"] == [] and Decimal(initial["rows"][1]["carrying_value"]) == 0, "zero-life opening schedule")
retained = read(channel, prefix + "asset.get", company, "approver", asset_id=asset["id"])["record"]["opening"]["source_evidence"]
require(retained["history"][0]["note"] == evidence[0]["history"][0]["note"], "source history not retained")
require(Decimal(retained["source_period_expense"][0]["amount"]) == 600 and Decimal(retained["tax_capital_allowances"][0]["allowances"]) == 2400, "source YTD and tax evidence not separate")
print("PASS MCP opening: zero-life schedule, source history/YTD/tax separation, class/account review, immutable retry and exact reconciliation")
# The source aggregate is not an ordinary acquisition debit.
refused(channel, prefix + "asset.acquire", company, "opening", who="approver", source="entry", source_id=asset["entry_id"], line_number=1, category_revision_id=revision["id"], description="Refused aggregate", in_service_date="2026-07-01")
# Another transaction holding the company row is a retryable refusal, never an infrastructure failure or a write.
def counts():
    return sql("SELECT (SELECT count(*) FROM finance_fixed_assets_runs WHERE company_id=2)||'/'||(SELECT count(*) FROM finance_ledger_entries WHERE company_id=2)||'/'||(SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'finance.%')")

before = counts()
command, environment = psql("BEGIN; SELECT 1 FROM companies WHERE tenant_id=1 AND id=2 FOR UPDATE; SELECT pg_sleep(60); ROLLBACK")
holder = subprocess.Popen(command, env=environment, stdout=subprocess.DEVNULL)
try:
    for _ in range(100):
        if sql("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND state='active' AND query LIKE '%pg_sleep(60)%' AND pid<>pg_backend_pid()") == "1":
            break
        time.sleep(0.1)
    else:
        raise RuntimeError("company row holder did not start")
    for key, args in [(prefix + "run.post", dict(period="2026-07-01")), (prefix + "category.save", category | {"name": "Locked category"}), (prefix + "opening.import", dict(source_reference="LOCKED", currency_code="MYR", csv=csv))]:
        started = time.monotonic()
        status, envelope = request("/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "plan", "arguments": {"operations": [operation(key, company, **args)]}}}, "approver", "mcp")
        require(time.monotonic() - started < 2, f"{key}: lock contention was not bounded")
        if status == 422:
            # Shared bearer/workplace proof can refuse before the owner preview.
            require(envelope == {"error": "rejected", "reason": "record_changed"}, f"{key}: expected typed retryable proof refusal")
        else:
            require(status == 200 and "result" in envelope, f"{key}: unexpected contention transport")
            result = envelope["result"]
            body = result["structuredContent"]
            if result.get("isError") is True:
                require(body == {"error": "rejected", "reason": "record_changed"}, f"{key}: expected typed retryable plan refusal")
            else:
                require(body.get("valid") is False and "record_changed" in json.dumps(body) and "error" not in body, f"{key}: lock contention must be a nonfatal retryable record_changed refusal")
finally:
    holder.kill()
    holder.wait()
    sql("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=current_database() AND query LIKE '%pg_sleep(60)%' AND pid<>pg_backend_pid()")
require(counts() == before, "lock contention wrote business or audit rows")
require(plan(channel, [operation(prefix + "run.post", company, period="2026-07-01")], "approver").get("valid") is True, "preview did not recover after the lock was released")
print("PASS MCP contention: authorized write previews refuse a busy company and recover after its lock is released")
# Preview-all-before-execute must refuse the dependent second run and roll back the first.
ops = [operation(prefix + "run.post", company, period="2026-07-01")] * 2
review = plan(channel, ops, "approver")
require(review.get("valid") is True, "duplicate-run plan review")
require(commit(channel, ops, review, "approver").get("error") == "record_changed", "dependent run fence")
require(read(channel, prefix + "run.list", company, "approver")["records"] == [], "dependent plan leaked run")
require(report() == initial, "dependent plan leaked ledger or register state")
# A category revision changes the report boundaries even before its assets change.
ops = [operation(prefix + "category.save", company, **revised), operation(prefix + "run.post", company, period="2026-07-01")]
review = plan(channel, ops, "approver")
require(review.get("valid") is True, "category/run plan review")
require(commit(channel, ops, review, "approver").get("error") == "record_changed", "dependent category/run fence")
require(report() == initial, "dependent category/run plan leaked state")
run = write(channel, prefix + "run.post", company, period="2026-07-01")
require(Decimal(run["total"]) == Decimal("106.25"), "monthly opening continuation charge")
require(write(channel, prefix + "run.post", company, period="2026-07-01")["id"] == run["id"], "run retry created another record")
posted = report()
tied(posted)
require(Decimal(posted["rows"][0]["accumulated"]) == Decimal("906.25"), "posted accumulation")
pins = {k: posted[k] for k in ["through_entry_number", "through_asset_number", "through_event_number", "through_category_revision_id"]}
require(read(channel, prefix + "schedule.get", company, "approver", as_of="2026-07-31", **pins)["report"] == posted, "schedule/register mismatch")
detail = read(channel, prefix + "asset.get", company, "approver", asset_id=asset["id"])["record"]
require(detail["charges"][0]["run_id"] == run["id"] and detail["opening"]["source_reference"] == "MCP-ASSET-111", "asset detail missing provenance/charge")
for who in ["viewer", "other"]:
    body = invoke(channel, "read", operation(prefix + "asset.get", company, asset_id=asset["id"]), who)
    require("error" in body, "unauthorized asset read")
    body = plan(channel, [operation(prefix + "run.post", company, period="2026-08-01")], who)
    require(body.get("valid") is not True, "unauthorized asset write")
refused(channel, prefix + "asset.dispose", company, "proceeds", who="approver", asset_id=asset["id"], effective_date="2026-08-01", proceeds="1.001", reason="Invalid precision")
refused(channel, prefix + "asset.dispose", company, "settlement_account_id", who="approver", asset_id=asset["id"], effective_date="2026-08-01", proceeds="500", reason="Missing account")
blank = json.dumps(plan(channel, [operation(prefix + "asset.dispose", company, asset_id=asset["id"], effective_date="2026-07-31", proceeds="", reason="")], "approver"))
require("\"proceeds\": [\"can't be blank\"]" in blank and "\"reason\": [\"can't be blank\"]" in blank, "blank reason and proceeds must be refused together as can't be blank")
refused(channel, prefix + "asset.dispose", company, "depreciation", who="approver", asset_id=asset["id"], effective_date="2026-08-31", proceeds="0", reason="Unposted August")
disposal = write(channel, prefix + "asset.dispose", company, asset_id=asset["id"], effective_date="2026-07-31", proceeds="9000", settlement_account_id=accounts["1000"], reason="Reviewed sale")
after = report()
require(after["rows"][0]["disposed"] and Decimal(after["rows"][0]["disposal_gain"]) == Decimal("-1093.75"), "disposal loss")
tied(after)
require(report(**pins) == posted, "disposal changed reproduced register")
refused(channel, prefix + "run.reverse", company, "disposals", who="approver", run_id=run["id"], effective_date="2026-07-31", reason="Standing disposal")
write(channel, prefix + "disposal.reverse", company, asset_id=asset["id"], effective_date="2026-07-31", reason="Correct the sale")
restored = report()
require(not restored["rows"][0]["disposed"] and Decimal(restored["rows"][0]["carrying_value"]) == Decimal("10093.75"), "disposal reversal failed to restore asset")
tied(restored)
write(channel, prefix + "run.reverse", company, run_id=run["id"], effective_date="2026-07-31", reason="Correct July charge")
require(write(channel, prefix + "run.post", company, period="2026-07-01")["reversed"], "reversed month was reused")
august = write(channel, prefix + "run.post", company, period="2026-08-01")
require(Decimal(august["total"]) == Decimal("212.50"), "following month did not catch up")
require(report(**pins) == posted, "later correction changed reproduced register")
refused(channel, prefix + "acquisition.void", company, "reversed", who="approver", asset_id=asset["id"], effective_date="2026-08-31", reason="Opening source remains live")
# Ordinary acquisitions and acquisition corrections use the same contracts too.
journal = write(channel, "finance.ledger.journal.submit", company, who="manager", entry_date="2026-09-01", description="Subsequent equipment acquisition", lines=[{"account_id": accounts["1200"], "debit": "600.00"}, {"account_id": accounts["1000"], "credit": "600.00"}])
approved = write(channel, "finance.ledger.journal.approve", company, journal_id=journal["id"])
entry_id = approved["decision"]["entry_id"]
source = read(channel, "finance.ledger.entry.get", company, "approver", entry_id=entry_id)
require(source["owner"] is None, "manual source already owned")
registered = write(channel, prefix + "asset.acquire", company, source="entry", source_id=entry_id, line_number=1, category_revision_id=revision["id"], description="Ordinary MCP acquisition", in_service_date="2026-09-01", life_months=12)
refused(channel, prefix + "asset.acquire", company, "already", who="approver", source="entry", source_id=entry_id, line_number=1, category_revision_id=revision["id"], description="Duplicate source", in_service_date="2026-09-01")
september = write(channel, prefix + "run.post", company, period="2026-09-01")
require(Decimal(september["total"]) == Decimal("156.25"), "ordinary/opening mixed run")
write(channel, "finance.ledger.entry.reverse", company, entry_id=entry_id, entry_date="2026-09-30", description="Reverse ordinary acquisition")
void = write(channel, prefix + "acquisition.void", company, asset_id=registered["id"], effective_date="2026-09-30", reason="Source acquisition corrected")
require(Decimal(void["accumulated"]) == 50, "void reversed another asset's charges")
current = read(channel, prefix + "register.get", company, "approver", as_of="2026-09-30")["report"]
tied(current)
require(next(r for r in current["rows"] if r["asset"]["id"] == registered["id"])["voided"], "void missing from register")
require(read(channel, "finance.ledger.entry.get", company, "approver", entry_id=entry_id)["owner"] is None, "registration stole source ownership")
require(report(**pins) == posted, "ordinary acquisition changed reproduced register")
# Zero-life opening assets use the same disposal/reversal contracts and pins.
write(channel, prefix + "asset.dispose", company, asset_id=zero["id"], effective_date="2026-09-30", proceeds="0", reason="Retire fully depreciated MCP asset")
zero_report = read(channel, prefix + "register.get", company, "approver", as_of="2026-09-30")["report"]
tied(zero_report)
zero_pins = {k: zero_report[k] for k in pins}
require(next(r for r in zero_report["rows"] if r["asset"]["id"] == zero["id"])["disposed"], "zero-life disposal missing")
write(channel, prefix + "disposal.reverse", company, asset_id=zero["id"], effective_date="2026-09-30", reason="Correct zero-life retirement")
require(read(channel, prefix + "register.get", company, "approver", as_of="2026-09-30", **zero_pins)["report"] == zero_report, "zero-life disposal pins changed after reversal")
tied(read(channel, prefix + "register.get", company, "approver", as_of="2026-09-30")["report"])
# Exact ownership and row-audit assertions observe the wire writes, never make them.
require(sql("SELECT count(*) FROM finance_ledger_entries WHERE company_id=2 AND owner_module='finance/fixed_assets'") == "9", "owned posting count")
for table, kind in [("categories", "category"), ("category_revisions", "category_revision"), ("assets", "asset"), ("runs", "run"), ("run_items", "run_item"), ("run_exceptions", "run_exception"), ("events", "event"), ("opening_batches", "opening_batch"), ("opening_items", "opening_item")]:
    require(sql(f"SELECT count(*) FROM finance_fixed_assets_{table} r WHERE r.tenant_id=1 AND r.company_id=2 AND (SELECT count(*) FROM base_audit_mutations a WHERE a.auditable_type='finance.fixed-assets.{kind}' AND a.auditable_id=r.id::text AND a.tenant_id=r.tenant_id AND a.new_values->>'company_id'=r.company_id::text)<>1") == "0", f"missing or duplicate row audit: {table}")
print("Fixed Assets real MCP proof passed: discovery, category preparation/revision, opening import, prospective monthly charge, dependent-plan rollback, precision/access refusals, sale/loss and reversal, run correction/catch-up, ordinary acquisition/void, source ownership, row audits and four-pin reproduced reconciliation.")
