#!/usr/bin/env python3
"""Actual HTTP MCP proof of #95 after `engagement-access serve` on a disposable
database (see ledger/appointed-auditor-and-sales-access.md). Credentials stay in
the private proof directory; SQL only observes counts.

Set BILIBI_AGENT_PROOF=disposable, APP_ORIGIN, AUDITOR_PACK_PROOF_DIR,
BILIBI_ENGAGEMENT_CONTROL=/absolute/path/to/target/debug/engagement-access, and
DATABASE_URL (for the control binary) plus libpq's PGHOST/PGPORT/PGUSER/PGDATABASE.
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
CONTROL = os.environ["BILIBI_ENGAGEMENT_CONTROL"]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sql(query):
    return subprocess.check_output(["psql", "-XAt", "-v", "ON_ERROR_STOP=1", "-c", query], text=True).strip()


require(sql("SELECT email FROM users WHERE tenant_id=1 AND id=2") == "viewer@example.test", "not the disposable fixture")


def invoke(tool, arguments, who):
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


def read(key, who, company=1, **values):
    body = invoke("read", op(key, company, **values), who)
    require("result" in body, f"{key} for {who}: read refused")
    return body["result"]


def read_refused(key, who, company=1, **values):
    body = invoke("read", op(key, company, **values), who)
    require(body.get("error") == "forbidden" or "error" in body, f"{key} for {who} company {company}: not refused")


def plan(operations, who):
    return invoke("plan", {"operations": operations}, who)


def commit(operations, review, who):
    return invoke("commit", {"operations": operations}, who)


def refused(body):
    return "error" in body or (body.get("valid") is False)


def control(action):
    subprocess.run([CONTROL, action], check=True, stdout=subprocess.DEVNULL)


# ---- Appointed auditor: reads for company 1, nothing for company 2, no writes.
chart = read("finance.ledger.account.list", "auditor")
accounts = {row["code"]: row["id"] for row in chart["records"]}
require(read("finance.ledger.trial_balance.get", "auditor", as_of="2026-03-31")["rows"], "trial balance missing")
read("finance.receivables.customer.list", "auditor")
read("finance.payables.settings.get", "auditor")
for company in (2, 100):
    read_refused("finance.ledger.book.get", "auditor", company)
    read_refused("finance.receivables.customer.list", "auditor", company)
journal = op("finance.ledger.journal.submit", entry_date="2026-03-15", description="auditor adjustment", lines=[{"account_id": accounts["1000"], "debit": "1.00"}, {"account_id": accounts["4000"], "credit": "1.00"}])
require(refused(plan([journal], "auditor")), "auditor submitted a journal")
require(refused(plan([op("finance.ledger.account.update", account_id=accounts["1000"], name="Forbidden")], "auditor")), "auditor changed the chart")
require(refused(plan([op("finance.receivables.draft.create", kind="invoice", series_id=1, customer_id=1)], "auditor")), "auditor drafted an invoice")
require(refused(plan([journal | {"input": journal["input"] | {"company_id": 2}}], "auditor")), "auditor wrote to company 2")
print("MCP auditor: Company 1 ledger and source reads pass; Company 2 reads and every write plan are refused")

# ---- Sales workflow: customers and drafts, no ledger.
customers = read("finance.receivables.customer.list", "sales")["records"]
require(customers, "no customers")
figures = read("finance.receivables.book_figures.get", "sales")
require(set(figures["book"]) == {"company_id", "currency_code", "minor_units"}, "book figures leaked more")
for key in ["finance.ledger.book.get", "finance.ledger.account.list", "finance.ledger.entry.list", "finance.ledger.trial_balance.get"]:
    read_refused(key, "sales", **({"as_of": "2026-03-31"} if key.endswith("trial_balance.get") else {}))
read_refused("finance.receivables.aging.get", "sales", as_of="2026-03-31")
read_refused("finance.payables.settings.get", "sales")
read_refused("finance.receivables.book_figures.get", "sales", 2)
series = read("finance.receivables.series.list", "sales")["records"][0]["id"]
draft = op("finance.receivables.draft.create", kind="invoice", series_id=series, customer_id=customers[0]["id"], description="MCP sales draft")
review = plan([draft], "sales")
require(review.get("valid") is True, "sales draft preview failed")
result = commit([draft], review, "sales")
require("results" in result, "sales draft commit failed")
issue_id = result["results"][0]["result"]["result"]["record"]["id"]
require(refused(plan([op("finance.receivables.document.issue", document_id=issue_id)], "sales")), "sales issued a document")
require(refused(plan([op("finance.ledger.journal.submit", entry_date="2026-03-15", description="x", lines=[])], "sales")), "sales submitted a journal")
require(refused(plan([draft | {"input": draft["input"] | {"company_id": 2}}], "sales")), "sales wrote to company 2")
print("MCP sales: customers, book figures and a draft pass; ledger, aging, issue, journals, payables and Company 2 are refused")

# ---- Revocation between preview and commit rolls back; a retained token loses reads at once.
review = plan([draft], "sales")
require(review.get("valid") is True, "revocation preview failed")
before = sql("SELECT count(*) FROM finance_receivables_documents WHERE tenant_id=1 AND company_id=1")
control("revoke-sales")
try:
    require(refused(commit([draft], review, "sales")), "revoked plan committed")
    require(sql("SELECT count(*) FROM finance_receivables_documents WHERE tenant_id=1 AND company_id=1") == before, "revoked plan left a draft")
    read_refused("finance.receivables.customer.list", "sales")
finally:
    control("restore-sales")
read("finance.receivables.customer.list", "sales")
control("revoke-auditor")
try:
    read_refused("finance.ledger.book.get", "auditor")
finally:
    control("restore-auditor")
read("finance.ledger.book.get", "auditor")
print("MCP: revoked sales plan rolled back with no draft; retained tokens lose reads at once; restored assignments work again")
