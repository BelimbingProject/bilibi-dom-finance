#!/usr/bin/env python3
"""Live opening bank bridge MCP walkthrough, not a unit-test runner. Use only the disposable
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
BASE = os.environ.get("BILIBI_AGENT_URL", "http://127.0.0.1:8147").rstrip("/")
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


# Company 002 is the disposable MCP company prepared by conversion --serve.
channel, company = "mcp", 2
account = int(sql("SELECT id FROM finance_reconciliation_bank_accounts WHERE company_id=2 AND name='Conversion bank'"))
key = "finance.reconciliation.opening"
source = dict(bank_account_id=account, source_reference="BANK-BRIDGE-1", statement_source="Source statement 2026-07-01, retained with conversion records", statement_balance="10200", currency_code="MYR", csv="source_id,item_date,description,amount\nPAY-1,2026-06-28,Outstanding payment,-500\nDEP-1,2026-06-30,Deposit in transit,300\n")
context = read(channel, key+".context", company, "approver", bank_account_id=account)
before = sql("SELECT count(*) FROM finance_ledger_entries WHERE company_id=2")
require(context["bridge"] is None, "needs an empty bridge")
refused(channel, key+".import", company, "statement balance", who="approver", **(source | {"statement_balance":"10201"}))
require(sql("SELECT count(*) FROM finance_reconciliation_opening_bridges WHERE company_id=2") == "0", "mismatch wrote a bridge")
record = write(channel, key+".import", company, **source)
retry = write(channel, key+".import", company, **source)
require(record["id"] == retry["id"], "retry did not return the same bridge")
corrected = source | {"source_reference":"BANK-BRIDGE-2", "replaces_bridge_id":record["id"], "reason":"Correct source description", "csv":source["csv"].replace("Outstanding payment", "Outstanding cheque")}
record = write(channel, key+".import", company, **corrected)
require(sql("SELECT count(*) FROM finance_reconciliation_opening_bridges WHERE company_id=2") == "2", "correction did not retain both bridges")
require(sql("SELECT count(*) FROM finance_ledger_entries WHERE company_id=2") == before, "bridge posted another entry")
subprocess.run([os.environ["BANK_BRIDGE_PROOF_BIN"], "prepare"], check=True)
for source_id, ref in [("PAY-1", "PAY-1"), ("DEP-1", "DEP-1")]:
    item = int(sql("SELECT id FROM finance_reconciliation_opening_items WHERE company_id=2 AND bridge_id=" + str(record["id"]) + " AND source_id='" + source_id + "'"))
    line = int(sql("SELECT id FROM finance_reconciliation_statement_lines WHERE company_id=2 AND reference='" + ref + "'"))
    write(channel, key+".match", company, bank_account_id=account, opening_item_id=item, statement_line_id=line)
    refused(channel, key+".match", company, "matched", who="approver", bank_account_id=account, opening_item_id=item, statement_line_id=line)
subprocess.run([os.environ["BANK_BRIDGE_PROOF_BIN"], "report"], check=True)
require(sql("SELECT count(*) FROM finance_ledger_entries WHERE company_id=2") == before, "clearing posted another entry")
print("MCP opening bridge proof passed: mismatch, exact retry, correction, conversion-day and later clearings, retained statement, duplicate refusal and independent finalisation; no new ledger entry")
