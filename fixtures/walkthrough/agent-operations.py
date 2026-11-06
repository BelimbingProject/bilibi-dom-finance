#!/usr/bin/env python3
"""Live HTTP/MCP walkthrough, not a unit-test runner. Use only the disposable
database and private credentials provisioned by the agent-operations binary.
Every mutation goes through the real wire edge; SQL observes its effects.
Tokens never appear in output. See the ledger README.
"""
import json
import os
import subprocess
import urllib.error
import urllib.request
from decimal import Decimal
from pathlib import Path

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
BASE = os.environ.get("BILIBI_AGENT_URL", "http://127.0.0.1:3100").rstrip("/")
CREDS = json.load(open(os.environ["BILIBI_AGENT_CREDENTIALS"]))
DB = os.environ["DATABASE_URL"]


def sql(query):
    return subprocess.check_output(
        ["psql", DB, "-XAt", "-v", "ON_ERROR_STOP=1", "-c", query], text=True
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


def control(*args):
    platform = Path(__file__).resolve().parents[5]
    binary = Path(os.environ.get("CARGO_TARGET_DIR", platform / "target")) / "debug" / "agent-operations"
    subprocess.run([str(binary), "payables-control", *map(str, args)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


unavailable_proved = False
for channel, company in [("api", 1), ("mcp", 2)]:
    print(f"\n== {channel}: company {company}")
    require(read(channel, "book.get", company)["book"] is None, "proof needs an unopened book")
    require(invoke(channel, "guide", {"key": "finance.ledger"}), "guide missing")
    require(invoke(channel, "describe", {"key": "finance.ledger.journal.submit"}), "schema missing")
    currencies = read(channel, "book.currencies", company)
    if sql("SELECT count(*) FROM currencies") == "0" and not unavailable_proved:
        # The ISO list is not installed: a well-formed code opens unchecked and needs its decimal places.
        unavailable_proved = True
        require(currencies["reference"] == "unavailable" and currencies["currencies"] == [], "list should be unavailable")
        refused(channel, "book.open", company, "minor_units", currency_code="MYR")
        unchecked = plan(channel, [operation("book.open", company, currency_code="MYR", minor_units=2)])
        require(unchecked["results"][0]["preview"]["result"]["changes"]["after"]["currency"]["reference"] == "unchecked", "unchecked review not marked")
        print("currency list unavailable: code accepted unchecked, decimal places required, review marked unchecked")
    else:
        # A disposable list of four currencies with three different decimal places; a seeded list keeps its own rows.
        sql("INSERT INTO currencies (code, numeric_code, name, minor_units) VALUES ('MYR','458','Malaysian ringgit',2),('JPY','392','Japanese yen',0),('KWD','414','Kuwaiti dinar',3),('CHF','756','Swiss franc',2) ON CONFLICT DO NOTHING")
        tenant = sql(f"SELECT tenant_id FROM companies WHERE id={company}")
        sql(f"DELETE FROM currency_tenant_enabled WHERE tenant_id={tenant}")
        # Without admin.currency.manage, nothing enabled means no book opens, and asking to enable changes nothing.
        control("grant", "admin.currency.manage", "false")
        none = read(channel, "book.currencies", company)
        require(none["reference"] == "none_enabled" and none["currencies"] == [] and none["can_enable"] is False, "installed list with nothing enabled should say so")
        refused(channel, "book.open", company, "no currency is enabled yet", currency_code="MYR", minor_units=2)
        refused(channel, "book.open", company, "no currency is enabled yet", currency_code="QQQ", minor_units=2)
        refused(channel, "book.open", company, "no currency is enabled yet", currency_code="MYR", enable_currency=True)
        print("catalog installed, nothing enabled, no currency manage: reported as none_enabled, every book.open refused in the same words, enable_currency refused")
        # With it, every current currency is offered and the review says opening enables the chosen one.
        control("grant", "admin.currency.manage", "true")
        offered = read(channel, "book.currencies", company)
        require(offered["reference"] == "iso4217" and offered["can_enable"] is True, "a currency manager should be offered the list")
        require(any(c["code"] == "CHF" and c["enabled"] is False for c in offered["currencies"]), "a currency manager should see currencies not enabled")
        refused(channel, "book.open", company, "no currency is enabled yet", currency_code="CHF", minor_units=2)
        enabling = plan(channel, [operation("book.open", company, currency_code="CHF", enable_currency=True)])
        require(enabling.get("valid") is True, f"enable on open preview refused: {enabling}")
        require(enabling["results"][0]["preview"]["result"]["changes"]["after"]["currency"]["enables_currency"] is True, "review should say the currency is enabled")
        require(sql(f"SELECT count(*) FROM currency_tenant_enabled WHERE tenant_id={tenant}") == "0", "preview enabled a currency")
        print("currency manager: every current currency offered with its enabled state; enable_currency reviewed, preview enabled nothing")
        sql(f"INSERT INTO currency_tenant_enabled (tenant_id, code) VALUES ({tenant},'MYR'),({tenant},'JPY'),({tenant},'KWD')")
        currencies = read(channel, "book.currencies", company)
        require(currencies["reference"] == "iso4217", "list should be available once currencies are enabled")
        found = read(channel, "book.currencies", company, search="dinar")["currencies"]
        require(any(c["code"] == "KWD" and c["minor_units"] == 3 for c in found), "search by name failed")
        require(read(channel, "book.currencies", company, search="jpy")["currencies"][0]["minor_units"] == 0, "search by code failed")
        refused(channel, "book.open", company, "has invalid format", currency_code="RM1", minor_units=2)
        refused(channel, "book.open", company, "is not a currency code", currency_code="QQQ", minor_units=2)
        refused(channel, "book.open", company, "is not enabled", currency_code="CHF", minor_units=2)
        after = lambda **v: plan(channel, [operation("book.open", company, currency_code="KWD", **v)])["results"][0]["preview"]["result"]["changes"]["after"]
        standard, nonstandard = after(), after(minor_units=2)
        require(standard["minor_units"] == 3 and standard["currency"]["currency_name"] == "Kuwaiti dinar" and standard["currency"]["nonstandard_minor_units"] is False, "default decimal places not the standard")
        require(nonstandard["minor_units"] == 2 and nonstandard["currency"]["nonstandard_minor_units"] is True and nonstandard["currency"]["standard_minor_units"] == 3, "nonstandard not flagged")
        print("currency list: search by name and code, refusals, standard default, nonstandard flagged, proposal:", (currencies["proposed"] or {}).get("source", "none"))
        # The book opens in MYR before MYR is enabled: one write enables it and opens the book.
        sql(f"DELETE FROM currency_tenant_enabled WHERE tenant_id={tenant} AND code='MYR'")
    enable = sql(f"SELECT count(*) FROM currencies WHERE code='MYR'") == "1" and sql(f"SELECT count(*) FROM currency_tenant_enabled e JOIN companies c ON c.tenant_id=e.tenant_id WHERE c.id={company} AND e.code='MYR'") == "0"
    direct = invoke(channel, "read", operation("book.open", company, currency_code="MYR", minor_units=2))
    require(direct.get("error") == "writes_not_enabled", "direct write was not refused")
    ops = [operation("book.open", company, currency_code="myr", minor_units=2, **({"enable_currency": True} if enable else {}))]
    preview = plan(channel, ops)
    require(preview.get("valid") is True, "book preview failed")
    require(sql(f"SELECT count(*) FROM finance_ledger_books WHERE company_id={company}") == "0", "preview wrote a book")
    require("results" in commit(channel, ops, preview), "book commit failed")
    require(read(channel, "book.get", company)["book"]["currency_code"] == "MYR", "currency not normalized")
    if enable:
        require(sql(f"SELECT count(*) FROM currency_tenant_enabled e JOIN companies c ON c.tenant_id=e.tenant_id WHERE c.id={company} AND e.code='MYR'") == "1", "opening the book did not enable MYR")
        print("book opened in MYR and enabled MYR in the same write")
    print("book preview wrote no business rows; commit normalized currency; direct write refused")

    asset = write(channel, "account.create", company, code="A", name="Asset", account_type="asset")
    income = write(channel, "account.create", company, code="I", name="Income", account_type="income")
    retained = write(channel, "account.create", company, code="E", name="Equity", account_type="equity", retained_earnings=True)
    group = write(channel, "account_group.create", company, code="G", name="Asset group", account_type="asset")
    child = write(channel, "account.create", company, code="B", name="Child", account_type="asset", parent_id=group["id"])
    child = write(channel, "account.update", company, account_id=child["id"], name="Changed", clear_parent=True, active=False)
    require(child["parent_id"] is None and child["active"] is False, "account update lost fields")
    page = read(channel, "account.list", company, page_size=2)
    page2 = read(channel, "account.list", company, page_size=2, after_code=page["next_after_code"])
    require([a["code"] for a in page["records"] + page2["records"]] == ["A", "B", "E", "G"], "account cursor skipped/duplicated rows")
    # Locale-aware PostgreSQL ordering must also govern the cursor predicate.
    # The original Rust string comparison skipped B after a in en_US.UTF-8.
    for code in ["a", "É", "z"]:
        write(channel, "account.create", company, code=code, name=code, account_type="asset")
    expected_codes = json.loads(sql(f"SELECT json_agg(code ORDER BY code) FROM finance_ledger_accounts WHERE company_id={company}"))
    codes, cursor = [], None
    while True:
        page = read(channel, "account.list", company, page_size=1, **({"after_code": cursor} if cursor else {}))
        codes.extend(a["code"] for a in page["records"])
        cursor = page["next_after_code"]
        if cursor is None:
            break
        require(len(codes) <= len(expected_codes), "account cursor did not advance")
    require(codes == expected_codes, "mixed-case/non-ASCII account cursor disagrees with database order")
    year = write(channel, "fiscal_year.open", company, start_date="2026-01-01", end_date="2026-02-28")
    first, last = year["periods"]
    print("created and changed accounts; exact two-page chart; opened two monthly periods")

    lines = [{"account_id": asset["id"], "debit": "101.23"}, {"account_id": income["id"], "credit": "101.23"}]
    refused(channel, "journal.submit", company, "decimal places", entry_date="2026-01-19", description="Precision", lines=[{**lines[0], "debit": "101.231"}, lines[1]])
    refused(channel, "journal.submit", company, "do not balance", entry_date="2026-01-19", description="Unbalanced", lines=[{**lines[0], "debit": "101.22"}, lines[1]])
    journal = write(channel, "journal.submit", company, entry_date="2026-01-19", description="Manual adjustment", lines=lines)
    require(journal["status"] == "submitted" and read(channel, "entry.list", company)["records"] == [], "submission posted")
    refused(channel, "journal.approve", company, "Someone else must approve", journal_id=journal["id"])
    approved = write(channel, "journal.approve", company, who="approver", journal_id=journal["id"])
    require(approved["status"] == "approved", "independent approval failed")
    entry_id = approved["decision"]["entry_id"]
    entry = read(channel, "entry.get", company, entry_id=entry_id)
    require(entry["lines"][0]["debit"] == "101.23" and entry["lines"][1]["credit"] == "101.23", "posted amounts differ")
    require(read(channel, "journal.get", company, journal_id=journal["id"])["decision"]["entry_id"] == entry_id, "read lost decision")
    rejected = write(channel, "journal.submit", company, entry_date="2026-01-20", description="Reject this", lines=lines)
    rejected = write(channel, "journal.reject", company, journal_id=rejected["id"], reason="Superseded")
    require(rejected["status"] == "rejected" and rejected["decision"]["entry_id"] is None, "rejection posted")
    wrong = write(channel, "journal.submit", company, entry_date="2026-01-20", description="Wrong descripton", lines=lines)
    refused(channel, "journal.withdraw", company, "Only the person who submitted", who="approver", journal_id=wrong["id"])
    refused(channel, "journal.replace", company, "do not balance", journal_id=wrong["id"], entry_date="2026-01-20", description="Wrong description", lines=[{**lines[0], "debit": "101.22"}, lines[1]])
    require(read(channel, "journal.get", company, journal_id=wrong["id"])["status"] == "submitted", "a refused correction withdrew the journal")
    fixed = write(channel, "journal.replace", company, journal_id=wrong["id"], reason="Typo in the description", entry_date="2026-01-20", description="Right description", lines=lines)
    withdrawn = read(channel, "journal.get", company, journal_id=wrong["id"])
    require(fixed["status"] == "submitted" and fixed["replaces_journal_id"] == wrong["id"] and withdrawn["status"] == "withdrawn" and withdrawn["replaced_by_journal_id"] == fixed["id"], "replacement did not link both journals")
    require(withdrawn["decision"]["reason"] == "Typo in the description" and withdrawn["decision"]["entry_id"] is None, "withdrawal lost its reason or posted")
    refused(channel, "journal.approve", company, "no longer waiting", who="approver", journal_id=wrong["id"])
    refused(channel, "journal.withdraw", company, "no longer waiting", journal_id=wrong["id"])
    alone = write(channel, "journal.submit", company, entry_date="2026-01-20", description="Submitted by mistake", lines=lines)
    gone = write(channel, "journal.withdraw", company, journal_id=alone["id"])
    require(gone["status"] == "withdrawn" and [j["id"] for j in read(channel, "journal.list", company, status="withdrawn")["records"]] == [wrong["id"], alone["id"]], "withdrawal not listed")
    require([e["id"] for e in read(channel, "entry.list", company)["records"]] == [entry_id], "a withdrawn or waiting journal posted")
    reversed_entry = write(channel, "entry.reverse", company, entry_id=entry_id, entry_date="2026-01-21", description="Correction")
    require(reversed_entry["reverses_entry_id"] == entry_id and reversed_entry["lines"][0]["credit"] == "101.23", "reversal is not the mirror")
    refused(channel, "entry.reverse", company, "already reversed", entry_id=entry_id, entry_date="2026-01-22", description="Twice")
    print("precision/balance refusals; submit without posting; same-person refusal; independent posting; rejection; withdrawal and replacement of a waiting journal (only its submitter, atomic, never posted); exact mirror and double-reversal refusal")

    # A line with balance true takes the amount that balances the others; preview and commit must agree.
    payable = write(channel, "account.create", company, code="L", name="Payable", account_type="liability")

    def find(value, key):
        if isinstance(value, dict):
            if isinstance(value.get(key), dict) and "lines" in value[key]:
                return value[key]
            value = list(value.values())
        if isinstance(value, list):
            for item in value:
                found = find(item, key)
                if found is not None:
                    return found
        return None

    def sides(lines):
        return [(l["account_id"], Decimal(l.get("debit") or 0), Decimal(l.get("credit") or 0)) for l in lines]

    def balanced(key, **extra):
        lines = [{"account_id": asset["id"], "debit": "106.00"}, {"account_id": income["id"], "credit": "100.00"}, {"account_id": payable["id"], "balance": True}]
        ops = [operation(key, company, entry_date="2026-01-26", description="Balance line", lines=lines, **extra)]
        review = plan(channel, ops)
        require(review.get("valid") is True, f"{key}: balance preview refused: {review.get('results', review.get('error'))}")
        previewed = sides(find(review, "entry")["lines"])
        body = commit(channel, ops, review)
        require("results" in body, f"{key}: balance commit refused: {body.get('error')}")
        record = body["results"][0]["result"]["result"]["record"]
        committed = sides(read(channel, "journal.get", company, journal_id=record["id"])["lines"])
        require(previewed == committed, f"{key}: preview and commit resolved the balance line differently")
        require(committed[2] == (payable["id"], Decimal(0), Decimal("6.00")), f"{key}: surplus debit did not become a credit of 6.00")
        return record

    submitted = balanced("journal.submit")
    short = plan(channel, [operation("journal.submit", company, entry_date="2026-01-26", description="Short side", lines=[{"account_id": asset["id"], "credit": "1.00"}, {"account_id": payable["id"], "balance": True}])])
    require(short.get("valid") is True and sides(find(short, "entry")["lines"])[1] == (payable["id"], Decimal("1.00"), Decimal(0)), "a credit surplus did not become a debit of 1.00")
    refused(channel, "journal.submit", company, "cannot be combined", entry_date="2026-01-26", description="Both", lines=[{"account_id": asset["id"], "debit": "106.00"}, {"account_id": payable["id"], "credit": "5.00", "balance": True}])
    refused(channel, "journal.submit", company, "only one line", entry_date="2026-01-26", description="Two", lines=[{"account_id": asset["id"], "debit": "1.00"}, {"account_id": income["id"], "balance": True}, {"account_id": payable["id"], "balance": True}])
    refused(channel, "journal.submit", company, "nothing to balance", entry_date="2026-01-26", description="Matched", lines=[{"account_id": asset["id"], "debit": "1.00"}, {"account_id": income["id"], "credit": "1.00"}, {"account_id": payable["id"], "balance": True}])
    refused(channel, "journal.submit", company, "decimal places", entry_date="2026-01-26", description="Too fine", lines=[{"account_id": asset["id"], "debit": "1.005"}, {"account_id": payable["id"], "balance": True}])
    require(sql(f"SELECT count(*) FROM finance_ledger_journals WHERE company_id={company} AND description IN ('Short side','Both','Two','Matched','Too fine')") == "0", "a refused balance journal was recorded")
    replacement = balanced("journal.replace", journal_id=submitted["id"])
    require(replacement["replaces_journal_id"] == submitted["id"] and read(channel, "journal.get", company, journal_id=submitted["id"])["status"] == "withdrawn", "balance replacement did not withdraw the original")
    print("balance line: debit or credit filled to balance at the book's places, same in preview and commit for submit and replace; refused beside an amount, twice, when matched and when finer than the places; nothing recorded on refusal")

    # Keep nonzero income so close/reopen must show actual generated entries.
    posted = write(channel, "journal.submit", company, entry_date="2026-02-17", description="Year activity", lines=lines)
    write(channel, "journal.approve", company, who="approver", journal_id=posted["id"])
    pending = write(channel, "journal.submit", company, entry_date="2026-01-25", description="Period will close", lines=lines)
    write(channel, "period.close", company, period_id=first["id"], reason="Complete")
    refused(channel, "journal.approve", company, "closed period", who="approver", journal_id=pending["id"])
    write(channel, "period.reopen", company, period_id=first["id"], reason="Correction window")
    write(channel, "period.close", company, period_id=first["id"])
    write(channel, "period.lock", company, period_id=first["id"])
    refused(channel, "period.reopen", company, "locked", period_id=first["id"], reason="Never")
    closing_ops = [operation("fiscal_year.close", company, fiscal_year_id=year["id"])]
    closing_review = plan(channel, closing_ops)
    require(closing_review.get("valid") is True and "101.23" in json.dumps(closing_review), "close review missing generated amounts")
    closed = commit(channel, closing_ops, closing_review)["results"][0]["result"]["result"]["record"]
    require(closed["status"] == "closed" and closed["closing_entry_id"] is not None, "year did not post closing entry")
    close_entry = read(channel, "entry.get", company, entry_id=closed["closing_entry_id"])
    require(any(l["account_id"] == retained["id"] and l["credit"] == "101.23" for l in close_entry["lines"]), "wrong retained-earnings side")
    reopened = write(channel, "fiscal_year.reopen", company, fiscal_year_id=year["id"], reason="Year correction")
    require(reopened["status"] == "open" and reopened["periods"][0]["status"] == "locked" and reopened["periods"][-1]["status"] == "open", "reopen changed other periods")
    require(read(channel, "entry.get", company, entry_id=close_entry["id"])["reversed_by_entry_id"] is not None, "reopen did not reverse close")
    write(channel, "fiscal_year.close", company, fiscal_year_id=year["id"])
    locked = write(channel, "fiscal_year.lock", company, fiscal_year_id=year["id"], reason="Final")
    require(all(p["status"] == "locked" for p in locked["periods"]), "year lock missed periods")
    refused(channel, "fiscal_year.reopen", company, "locked", fiscal_year_id=year["id"], reason="Never")
    print("period close/reopen/permanent lock; closed-period approval refusal; year close generated exact retained-earnings entry; reopen reversed it; whole-year permanent lock")

    # Both previews default to 2026-03-01. A naive implementation would open
    # two different years. The owner fence must roll back the FIRST insert too.
    doubles = [operation("fiscal_year.open", company), operation("fiscal_year.open", company)]
    review = plan(channel, doubles)
    require(review.get("valid") is True, "composition setup preview failed")
    before = sql(f"SELECT count(*) FROM finance_ledger_fiscal_years WHERE company_id={company}")
    body = commit(channel, doubles, review)
    require(body.get("error") == "record_changed", "within-plan default drift was not fenced")
    require(sql(f"SELECT count(*) FROM finance_ledger_fiscal_years WHERE company_id={company}") == before, "partial first operation survived rollback")
    require(commit(channel, doubles, review).get("error") == "record_changed", "invalid batch changed outcome on retry")
    require("results" in commit(channel, [operation("fiscal_year.open", company)], plan(channel, [operation("fiscal_year.open", company)])), "fresh plan after rollback failed")
    print("two defaulted years refused as stale; first insert rolled back; corrected write succeeds")

    foreign = invoke(channel, "read", operation("book.get", company), "other")
    require("error" in foreign and "result" not in foreign, "cross-tenant disclosure")
    extra = invoke(channel, "read", operation("book.get", company, actor_id=1))
    require(extra.get("error") == "invalid_input", "caller-supplied actor accepted")
    print("cross-tenant read and caller-supplied actor refused")

print("\nHTTP and MCP ledger walkthrough completed; all observed records matched the reviewed contracts.")
