#!/usr/bin/env python3
"""Actual HTTP MCP proof after `auditor-pack -- serve` on a disposable database.

Credentials remain private. SQL observes journal counts; every business write
uses MCP or the fixture's Authz control. See ledger/accountant-access.md.
"""
import json
import os
import subprocess
import urllib.request

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
BASE = os.environ["APP_ORIGIN"].rstrip("/")
with open(os.environ["AUDITOR_PACK_PROOF_DIR"] + "/mcp-credentials.json", encoding="utf-8") as source:
    CREDENTIALS = json.load(source)
CONTROL = os.environ["BILIBI_ACCOUNTANT_CONTROL"]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sql(query):
    # libpq reads ~/.pgpass; never put the database password in argv.
    return subprocess.check_output(["psql", "-XAt", "-v", "ON_ERROR_STOP=1", "-c", query], text=True).strip()


require(sql("SELECT email FROM users WHERE tenant_id=1 AND id=2") == "viewer@example.test", "not the disposable fixture")


def invoke(tool, arguments, who="first"):
    request = urllib.request.Request(
        BASE + "/mcp",
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}).encode(),
        headers={"Authorization": "Bearer " + CREDENTIALS[who], "Content-Type": "application/json", "Accept": "application/json, text/event-stream"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        body = json.load(response)
    require("result" in body, "MCP protocol error")
    return body["result"]["structuredContent"]


def op(key, company=1, **values):
    return {"key": key, "input": {"company_id": company, **values}}


def read(key, who="first", company=1, **values):
    body = invoke("read", op(key, company, **values), who)
    require("result" in body, f"{key}: read refused")
    return body["result"]


def plan(operations, who="first"):
    return invoke("plan", {"operations": operations}, who)


def commit(operations, review, who="first"):
    return invoke("commit", {"operations": operations}, who)


def refused(body):
    return "error" in body or (body.get("valid") is False)


def control(action):
    subprocess.run([CONTROL, action], check=True, stdout=subprocess.DEVNULL)


mixed_id = int(sql("SELECT id FROM finance_ledger_entries WHERE tenant_id=1 AND company_id=1 AND description='Historical payroll and sales allocation' ORDER BY id LIMIT 1"))
for who in ["first", "second"]:
    chart = read("finance.ledger.account.list", who)
    require(len(chart["records"]) >= 9, "chart incomplete")
    require(read("finance.ledger.trial_balance.get", who, as_of="2026-03-31")["rows"], "trial balance missing")
    mixed = read("finance.ledger.entry.get", who, entry_id=mixed_id)
    require(len(mixed["lines"]) == 4, "mixed entry filtered")
    for company in [2, 100]:
        require(refused(invoke("read", op("finance.ledger.book.get", company), who)), "company isolation failed")
    denied = invoke("read", op("finance.payables.settings.get"), who)
    require(denied.get("error") == "forbidden" and denied.get("reason") == "missing_capability", "source read did not enforce its capability")
    accounts = {row["code"]: row["id"] for row in chart["records"]}
    adjustment = op("finance.ledger.journal.submit", entry_date="2026-03-15", description=f"MCP {who} adjusting journal", lines=[{"account_id": accounts["1000"], "debit": "2.00"}, {"account_id": accounts["4000"], "credit": "2.00"}])
    review = plan([adjustment], who)
    require(review.get("valid") is True, "adjustment preview failed")
    result = commit([adjustment], review, who)
    require("results" in result, "adjustment commit failed")
    journal = result["results"][0]["result"]["result"]["record"]
    require(journal["status"] == "submitted", "submission posted")
    approval = [op("finance.ledger.journal.approve", journal_id=journal["id"])]
    require(refused(plan(approval, who)), "accountant approval allowed")
    supervisor_review = plan(approval, "supervisor")
    require(supervisor_review.get("valid") is True, "independent review failed")
    require("results" in commit(approval, supervisor_review, "supervisor"), "independent approval failed")
    require(refused(plan([op("finance.ledger.account.update", account_id=accounts["1000"], name="Forbidden")], who)), "chart change allowed")
    require(refused(plan([op("finance.ledger.period.close", period_id=1)], who)), "period close allowed")
    print(f"MCP {who}: complete chart/trial balance/mixed entry; journal preview/commit and independent approval; other companies, source reads, chart changes and close denied")

# Authority is rechecked between review and commit; denial leaves no journal.
review = plan([adjustment])
require(review.get("valid") is True, "revocation preview failed")
before = sql("SELECT count(*) FROM finance_ledger_journals WHERE tenant_id=1 AND company_id=1")
control("deny-journals")
try:
    require(refused(commit([adjustment], review)), "revoked submit committed")
    require(sql("SELECT count(*) FROM finance_ledger_journals WHERE tenant_id=1 AND company_id=1") == before, "revoked plan left journal")
finally:
    control("restore-journals")
control("deny-ledger")
try:
    require(refused(invoke("read", op("finance.ledger.book.get"))), "existing token retained access")
    read("finance.ledger.book.get", "second")
finally:
    control("restore-ledger")
print("MCP: revocation between preview/commit rolled back; existing token read revoked; second accountant unaffected")
