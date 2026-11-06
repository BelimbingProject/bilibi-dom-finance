#!/usr/bin/env python3
"""Live opening-balance MCP walkthrough, not a unit-test runner. Use only the disposable
database and private credentials provisioned by the agent-operations binary.
Every mutation goes through the real wire edge; SQL observes its effects.
Tokens never appear in output. See the ledger README.
"""
import json
from decimal import Decimal
import os
import subprocess
import urllib.error
import urllib.request
import urllib.parse

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
BASE = os.environ.get("BILIBI_AGENT_URL", "http://127.0.0.1:3157").rstrip("/")
CREDS = json.load(open(os.environ["BILIBI_AGENT_CREDENTIALS"]))
DB = os.environ["DATABASE_URL"]


def sql(query):
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
    return subprocess.check_output(
        ["psql", "-XAt", "-v", "ON_ERROR_STOP=1", "-c", query], text=True, env=environment
    ).strip()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


require(sql("SELECT count(*) FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=1 AND i.email='manager@example.test'") == "1", "not the disposable fixture")


def request(path, body=None, who="manager", channel="api"):
    headers = {"Authorization": "Bearer " + CREDS[who][channel], "Content-Type": "application/json"}
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
    return {"key": "finance.ledger." + key, "input": {"company_id": company, **values}}


def read(channel, key, company, who="manager", **values):
    op = operation(key, company, **values)
    body = invoke(channel, "read", op, who)
    require("result" in body, f"{key}: read failed: {body.get('error')}")
    return body["result"]


def plan(channel, ops, who="manager"):
    return invoke(channel, "plan", {"operations": ops}, who)


def commit(channel, ops, review, who="manager"):
    return invoke(channel, "commit", {"operations": ops}, who)


def write(channel, key, company, who="manager", **values):
    ops = [operation(key, company, **values)]
    review = plan(channel, ops, who)
    require(review.get("valid") is True, f"{key}: preview refused: {review.get('results', review.get('error'))}")
    body = commit(channel, ops, review, who)
    require("results" in body, f"{key}: commit refused: {body.get('error')}")
    return body["results"][0]["result"]["result"]["record"]


def refused(channel, key, company, contains, who="manager", **values):
    body = plan(channel, [operation(key, company, **values)], who)
    require(body.get("valid") is False and contains in json.dumps(body), f"{key}: expected refusal {contains}")


# Run after agent-operations on its own fresh database, never after the full
# agent-operations.py proof (which deliberately fills both books).
channel, company = "mcp", 2
require(read(channel, "book.get", company)["book"] is None, "needs a clean company")
require(invoke(channel, "describe", {"key": "finance.ledger.opening_balances.submit"}), "opening schema missing")
write(channel, "book.open", company, currency_code="MYR", minor_units=2)
for code, name, kind in [("1000", "Cash at bank", "asset"), ("1100", "Trade receivables", "asset"), ("5000", "Cost of sales", "expense"), ("2000", "Trade payables", "liability"), ("3000", "Share capital", "equity"), ("4000", "Sales", "income")]:
    write(channel, "account.create", company, code=code, name=name, account_type=kind)
write(channel, "fiscal_year.open", company, start_date="2026-01-01")
source = {"conversion_date": "2026-07-01", "currency_code": "MYR", "source_reference": "SOURCE-CLOSE-2026-06-30", "csv": "source_id,account_code,debit,credit\nbank,1000,10000,\nreceivables,1100,3000,\ncost,5000,2000,\npayables,2000,,1500\ncapital,3000,,8000\nsales,4000,,5500\n"}
for changed, text in [({"csv":source["csv"].replace("5500", "5501")}, "balance"), ({"csv":source["csv"].replace("1000,", "missing,")}, "active account"), ({"currency_code":"USD"}, "currency"), ({"csv":source["csv"].replace("receivables,", "bank,")}, "unique"), ({"csv":source["csv"].replace("10000,", "10000.001,")}, "decimal")]:
    refused(channel, "opening_balances.submit", company, text, **(source | changed))
print("MCP: schema discovered; unbalanced, unmapped, wrong-currency, duplicate-row and precision previews refused")
ops = [operation("opening_balances.submit", company, **source)]
preview = plan(channel, ops)
require(preview.get("valid") is True, "opening preview failed")
require(sql("SELECT count(*) FROM finance_ledger_opening_balances") == "0", "preview wrote source evidence")
require(read(channel, "trial_balance.get", company, as_of="2026-07-01")["through_entry_number"] == 0, "preview posted")
facts = preview["results"][0]["preview"]["result"]["changes"]
require(len(facts["rows"]) == 6 and Decimal(facts["debit"]) == Decimal("15000") == Decimal(facts["credit"]), "review omitted mapped source totals")

# All steps are previewed before any execute. Two opening submissions cannot
# silently reuse the first's generated journal: stale review rolls both back.
double = ops + ops
review = plan(channel, double)
require(review.get("valid") is True, "independent initial previews")
require(commit(channel, double, review).get("error") == "record_changed", "dependent duplicate did not roll back")
require(sql("SELECT count(*) FROM finance_ledger_opening_balances") == "0", "plan rollback left evidence")
committed = commit(channel, ops, preview)
require("results" in committed, "reviewed opening commit failed")
journal = committed["results"][0]["result"]["result"]["record"]
opening_rows = read(channel, "journals.list", company, source="opening", status="submitted")["records"]
require(len(opening_rows) == 1 and opening_rows[0]["id"] == journal["id"] and opening_rows[0]["kind"] == "journal", "opening source did not find the waiting journal")
require(not read(channel, "journals.list", company, source="manual")["records"], "opening leaked into manual source")
require(read(channel, "trial_balance.get", company, as_of="2026-07-01")["through_entry_number"] == 0, "submission posted")
retry = write(channel, "opening_balances.submit", company, **source)
require(retry["id"] == journal["id"], "retry duplicated journal")
refused(channel, "opening_balances.submit", company, "different opening journal", **(source | {"csv":source["csv"].replace("10000", "10001")}))
refused(channel, "opening_balances.submit", company, "already has an opening journal", **(source | {"source_reference":"renamed-export"}))
refused(channel, "journal.approve", company, "separation", journal_id=journal["id"])
posted = write(channel, "journal.approve", company, "approver", journal_id=journal["id"])
entry_id = posted["decision"]["entry_id"]
opening_rows = read(channel, "journals.list", company, source="opening", status="posted")["records"]
require(len(opening_rows) == 1 and opening_rows[0]["id"] == entry_id and opening_rows[0]["kind"] == "entry", "approved opening was duplicated or lost from Journals")
require(not read(channel, "journals.list", company, source="opening", status="submitted")["records"], "approved opening still listed as waiting")
trial = read(channel, "trial_balance.get", company, as_of="2026-07-01")
expected = {"1000": ("10000", "0"), "1100": ("3000", "0"), "5000": ("2000", "0"), "2000": ("0", "1500"), "3000": ("0", "8000"), "4000": ("0", "5500")}
for row in trial["rows"]:
    code = row["code"]
    require((Decimal(row["debit_balance"]), Decimal(row["credit_balance"])) == tuple(map(Decimal, expected[code])), "account does not reconcile: " + code)
require(Decimal(trial["total_debit"]) == Decimal("15000") == Decimal(trial["total_credit"]), "target totals differ")
require(write(channel, "opening_balances.submit", company, **source)["id"] == journal["id"], "posted retry duplicated source")
require(sql("SELECT count(*) FROM finance_ledger_entries WHERE company_id=2") == "1", "duplicate or unintended posting")
write(channel, "entry.reverse", company, entry_id=entry_id, entry_date="2026-07-01", description="Correct opening balances against revised source", reference="CORRECTION-1")
require(Decimal(read(channel, "trial_balance.get", company, as_of="2026-07-01")["total_debit"]) == 0, "reversal failed")
entry = read(channel, "entry.get", company, entry_id=entry_id)
lines = [{"account_id": line["account_id"], "debit": line["debit"], "credit": line["credit"], "description": line["description"]} for line in entry["lines"]]
account_ids = {row["code"]: row["id"] for row in trial["rows"]}
for line in lines:
    if line["account_id"] == account_ids["1000"]: line["debit"] = "10100"
    if line["account_id"] == account_ids["3000"]: line["credit"] = "8100"
corrected = write(channel, "journal.submit", company, entry_date="2026-07-01", description="Corrected opening balances", reference="CORRECTION-2", lines=lines)
write(channel, "journal.approve", company, "approver", journal_id=corrected["id"])
revised = read(channel, "trial_balance.get", company, as_of="2026-07-01")
require(Decimal(revised["total_debit"]) == Decimal("15100"), "correction failed")
for row in revised["rows"]:
    if row["code"] == "1000": require(Decimal(row["debit_balance"]) == Decimal("10100"), "revised bank differs")
    if row["code"] == "3000": require(Decimal(row["credit_balance"]) == Decimal("8100"), "revised capital differs")
require(read(channel, "trial_balance.get", company, as_of="2026-07-01", through_entry_number=trial["through_entry_number"]) == trial, "original trial-balance pin changed")
print("MCP: preview is read-only; dependent plan rolls back; source retry is idempotent; separate independent approval posts exactly six balances totaling 15000.00; reversal and approved correction reconcile revised balances to 15100.00; original pin reproduces 15000.00")
print("Reviews: 8 setup, 2 conversion (submission and independent approval); additional reviews exercise retry, rollback and correction. EXCLUDED: open items, settlement, aging, bank reconciliation items, assets, tax detail and historical transactions.")
