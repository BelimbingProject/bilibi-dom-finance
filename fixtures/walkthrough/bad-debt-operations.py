#!/usr/bin/env python3
"""Live MCP proof on bad-debt's disposable database, with agent-operations OAuth.
Tokens stay private. Start bad-debt serve, run agent-operations
with BILIBI_AGENT_PROOF=disposable and a new BILIBI_AGENT_CREDENTIALS path, then
run this script with the same path and BILIBI_AGENT_URL set to that host.
"""
import json
from decimal import Decimal
import os
from pathlib import Path
import urllib.error
import urllib.request

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
BASE = os.environ["BILIBI_AGENT_URL"].rstrip("/")
CREDS = json.loads(Path(os.environ["BILIBI_AGENT_CREDENTIALS"]).read_text())
PREFIX = "finance.receivables."


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def invoke(tool, args, who="manager"):
    req = urllib.request.Request(BASE + "/mcp", data=json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": args},
    }).encode(), headers={"Authorization": "Bearer " + CREDS[who]["mcp"],
                         "Content-Type": "application/json",
                         "Accept": "application/json, text/event-stream"})
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            data = json.load(response)
    except urllib.error.HTTPError as response:
        data = json.load(response)
    require("result" in data, "MCP protocol/transport refusal")
    return data["result"]["structuredContent"]


def op(key, **values):
    return {"key": PREFIX + key, "input": {"company_id": 1, **values}}


def read(key, **values):
    reply = invoke("read", op(key, **values))
    require("result" in reply, "read refused: " + key)
    return reply["result"]


def write(key, who="manager", **values):
    ops = [op(key, **values)]
    review = invoke("plan", {"operations": ops}, who)
    require(review.get("valid") is True, "review refused: " + key)
    result = invoke("commit", {"operations": ops}, who)
    require("results" in result, "commit refused: " + key)
    return result["results"][0]["result"]["result"]["record"]


for key in ("submit", "approve", "reject", "reverse"):
    require(invoke("describe", {"key": PREFIX + "write_off." + key}), "missing operation")
accounts = read("write_off.accounts")["accounts"]
expense = next(a["id"] for a in accounts if a["code"] == "BD-expense")
documents = read("document.list", kind="invoice", status="issued")["records"]
invoice = next(d for d in documents if d["description"] == "Browser and MCP proposal")
require(all(a["id"] != invoice["receivable_account_id"] for a in accounts), "control offered as an offset")
require(all(a["code"] != "1000" for a in accounts), "receipt deposit account offered as an offset")
initial = invoice["outstanding"]
proposal = dict(document_id=invoice["id"], entry_date="2026-10-28", amount="10",
                offset_account_id=expense, reason="MCP specimen collection assessment",
                evidence="Original invoice source documents; MCP proof")
bad = invoke("plan", {"operations": [op("write_off.submit", **{**proposal, "amount": "100000"})]})
require(bad.get("valid") is False, "excess amount was reviewable")
created = write("write_off.submit", **proposal)
require(read("document.get", document_id=invoice["id"])["outstanding"] == initial, "proposal posted")
decision = dict(write_off_id=created["id"], reason="MCP independent review")
own = invoke("plan", {"operations": [op("write_off.approve", **decision)]})
require(own.get("valid") is False, "maker approved own journal")
write("write_off.approve", who="approver", **decision)
written = read("document.get", document_id=invoice["id"])
require(Decimal(str(written["outstanding"])) == Decimal(str(initial)) - 10, "approval did not reduce balance")
write("write_off.reverse", who="approver", write_off_id=created["id"],
      reversal_date="2026-10-28", reason="MCP correction restores original invoice")
require(read("document.get", document_id=invoice["id"])["outstanding"] == initial, "reversal did not restore balance")

# Every step previews before any executes. The second invoice review must
# become stale after the first proposal, and the shared plan rolls both back.
before = read("document.get", document_id=invoice["id"])
ops = [op("write_off.submit", **proposal), op("write_off.submit", **proposal)]
review = invoke("plan", {"operations": ops})
require(review.get("valid") is True, "multi-step review refused too early")
result = invoke("commit", {"operations": ops})
require(result.get("error") == "record_changed", "second step did not reject changed invoice")
require(read("document.get", document_id=invoice["id"]) == before, "plan rollback lost atomicity")
other = invoke("read", op("document.get", document_id=invoice["id"]), who="other")
require("result" not in other, "other tenant read invoice")
print("MCP passed: describe/read, eligible accounts, reviewed proposal/approval/reversal, own/excess/reach refusals, multi-step stale-plan rollback and original invoice history.")
