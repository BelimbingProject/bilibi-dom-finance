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
import socket

# RFC 6761 names resolve to loopback in Chrome; glibc does not resolve the wildcard.
_dns_lookup = socket.getaddrinfo
def _local_lookup(host, port, *args, **kwargs):
    return _dns_lookup("127.0.0.1" if isinstance(host, str) and host.endswith(".localhost") else host, port, *args, **kwargs)
socket.getaddrinfo = _local_lookup

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


require(sql("SELECT current_database()") == "onedoor_finance_ledger", "not the disposable one-door fixture")


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


# These are fixture data, not product defaults.
import datetime
import sys
channel = "mcp"
company = 1
second = 10001
today = datetime.date.today()
late_date = today - datetime.timedelta(days=100)
if sys.argv[-1] == "setup":
    currency = [{"key": "core.currency.enabled.set", "input": {"code": "MYR", "enabled": True}}]
    preview = plan(channel, currency)
    require(preview.get("valid") is True, "currency enable preview refused")
    require("results" in commit(channel, currency, preview), "currency enable failed")
    for target in [company, second]:
        write(channel, "book.open", target, currency_code="MYR", minor_units=2)
        write(channel, "account.create", target, code="proof-asset", name="Proof asset", account_type="asset")
        write(channel, "account.create", target, code="proof-equity", name="Proof equity", account_type="equity", retained_earnings=True)
        first = write(channel, "fiscal_year.open", target, start_date=datetime.date(today.year - 1, 1, 1).isoformat())
        require(len(read(channel, "fiscal_year.list", target, status="open")["records"]) == 1, "open-year filter lost year")
        write(channel, "fiscal_year.close", target, fiscal_year_id=first["id"])
        require(len(read(channel, "fiscal_year.list", target, status="closed")["records"]) == 1, "closed-year filter lost year")
        require(not read(channel, "fiscal_year.list", target, status="open")["records"], "closed year still filtered as open")
        write(channel, "fiscal_year.lock", target, fiscal_year_id=first["id"])
        require(len(read(channel, "fiscal_year.list", target, status="locked")["records"]) == 1, "locked-year filter lost year")
        require(not read(channel, "fiscal_year.list", target, status="closed")["records"], "locked year still filtered as merely closed")
        write(channel, "fiscal_year.open", target)
        require(len(read(channel, "fiscal_year.list", target, status="open")["records"]) == 1, "next open year lost")
    print("MCP: each explicit company book opened through review; fiscal year opening and all state filters agree")
    raise SystemExit(0)

company = second  # Keep the current company's clean opening for the browser proof.
source = {"conversion_date": late_date.isoformat(), "currency_code": "MYR", "source_reference": "onedoor-opening", "csv": "source_id,account_code,debit,credit\nasset,proof-asset,1250,\nequity,proof-equity,,1250\n"}
refused(channel, "opening_balances.submit", company, "balance", **(source | {"csv": source["csv"].replace(",,1250", ",,1249")}))
journal = write(channel, "opening_balances.submit", company, **source)
waiting = read(channel, "journals.list", company, source="opening", status="submitted")["records"]
require(len(waiting) == 1 and waiting[0]["id"] == journal["id"], "opening source did not find waiting journal")
require(not read(channel, "journals.list", company, source="manual")["records"], "opening was mislabeled manual")
correction = write(channel, "opening_balances.submit", company, **(source | {"source_reference": "onedoor-opening-correction", "replaces_journal_id": journal["id"]}))
require(read(channel, "journal.get", company, journal_id=journal["id"])["status"] == "withdrawn", "correction did not withdraw original")
refused(channel, "journal.approve", company, "another eligible person", journal_id=correction["id"])
posted = write(channel, "journal.approve", company, "approver", journal_id=correction["id"])
entry = posted["decision"]["entry_id"]
rows = read(channel, "journals.list", company, source="opening", status="posted", recording="late", review="pending")["records"]
require(len(rows) == 1 and rows[0]["id"] == entry and rows[0]["kind"] == "entry", "opening posting duplicated or missing from late filter")
require(not read(channel, "journals.list", company, source="opening", status="submitted")["records"], "approved opening still waiting")
refused(channel, "entry.review_late", company, "independent", "approver", entry_id=entry, reason="Own posting")
refused(channel, "entry.review_late", company, "blank", entry_id=entry, reason=" ")
review_ops = [operation("entry.review_late", company, entry_id=entry, reason="Source arrived after the transaction date")]
review = plan(channel, review_ops)
require(review.get("valid") is True, "independent review preview failed")
require(read(channel, "entry.get", company, entry_id=entry)["late_recording"]["review"] is None, "preview wrote review")
# A later refusal rolls the successful first review back inside the shared plan.
batch = review_ops + [operation("entry.review_late", company, entry_id=entry, reason="Second review in one plan")]
preview = plan(channel, batch)
require(preview.get("valid") is True, "independent previews should see no review")
require("error" in commit(channel, batch, preview), "dependent duplicate review did not refuse")
require(read(channel, "entry.get", company, entry_id=entry)["late_recording"]["review"] is None, "plan rollback left review")
write(channel, "entry.review_late", company, entry_id=entry, reason="Source arrived after the transaction date")
reviewed = read(channel, "entry.get", company, entry_id=entry)["late_recording"]["review"]
require(reviewed["reason"] == "Source arrived after the transaction date", "review reason lost")
require(not read(channel, "journals.list", company, recording="late", review="pending")["records"], "reviewed entry still pending")
require(len(read(channel, "journals.list", company, recording="late", review="reviewed")["records"]) == 1, "reviewed filter lost entry")
refused(channel, "entry.review_late", company, "reviewed", entry_id=entry, reason="Duplicate")
print("MCP: balanced opening review, correction, independent approval, source/status/late/review filters, independent-review refusal, read-only preview, shared-plan rollback and permanent single review passed")
