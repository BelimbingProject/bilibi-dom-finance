#!/usr/bin/env python3
"""Opening-credit MCP proof against the same disposable launcher fixture as the browser.
Prepare with conversion prepare-credits-launcher and launcher-credentials.
Credentials stay in memory and never appear in proof output.
"""
import concurrent.futures
import json
import os
import ssl
import socket
import urllib.parse
import urllib.error
import urllib.request

if os.environ.get("BILIBI_AGENT_PROOF") != "disposable":
    raise SystemExit("BILIBI_AGENT_PROOF=disposable is required")
base = os.environ["BILIBI_AGENT_URL"].rstrip("/")
hostname = urllib.parse.urlsplit(base).hostname or ""
if not base.startswith("https://") or not hostname.endswith(".localhost"):
    raise SystemExit("Use the disposable launcher's HTTPS localhost name")
credentials = json.load(open(os.environ["BILIBI_AGENT_CREDENTIALS"]))
config = next(row for row in json.load(open(os.environ["BILIBI_OPENING_PROOF_CONFIG"])) if row["company_id"] == 9)
company = config["company_id"]
# Chrome resolves .localhost without DNS; Python's resolver may not. Keep
# the original HTTPS hostname/SNI and route only this disposable name locally.
resolve = socket.getaddrinfo
socket.getaddrinfo = lambda host, *args, **kwargs: resolve("127.0.0.1" if host == hostname else host, *args, **kwargs)
context = ssl._create_unverified_context()  # Disposable local launcher certificate only.


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def invoke(tool, arguments):
    request = urllib.request.Request(base + "/mcp", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}).encode(), headers={"Authorization": "Bearer " + credentials["mcp"], "Accept": "application/json, text/event-stream", "Content-Type": "application/json"})
    with urllib.request.urlopen(request, context=context, timeout=60) as response:
        result = json.load(response)
    require("result" in result, "MCP protocol refusal")
    return result["result"]["structuredContent"]


def read(key, **values):
    result = invoke("read", {"key": key, "input": {"company_id": company, **values}})
    require("result" in result, key + " read refused")
    return result["result"]


def operation(key, **values):
    return {"key": key, "input": {"company_id": company, **values}}


def preview(op):
    return invoke("plan", {"operations": [op]})


def commit(op, review):
    return invoke("commit", {"operations": [op]})


def succeeded(result):
    return "error" not in result and "results" in result


def write(op):
    review = preview(op)
    require(review.get("valid") is True, op["key"] + " preview refused")
    result = commit(op, review)
    require(succeeded(result), op["key"] + " commit refused")
    return result["results"][0]["result"]["result"]["record"]


def pins(report):
    return {key: report[key] for key in ("through_entry_number", "through_opening_batch_id", "through_settlement_number")}


def aging(module, **values):
    return read("finance." + module + ".aging.get", as_of="2026-07-31", **values)


def tie(module):
    report = read("finance." + module + ".control.tie_out", as_of="2026-07-31")
    require(report["tied_out"] is True, module + " controls differ")
    return report


# Both imports keep their own review and original source tax facts. No postings.
for module, source in [("receivables", "ar"), ("payables", "ap")]:
    values = {"source_reference": "MCP-CREDIT-" + source.upper(), "currency_code": "MYR", "csv": config[source], "opening_account_classifications": [{"account_id": int(account), "classification": kind} for account, kind in config["opening_account_classifications"].items()]}
    if module == "receivables":
        values["series_id"] = config["invoice_series"]
    op = operation("finance." + module + ".opening.import", **values)
    review = preview(op)
    require(review.get("valid") is True, module + " mixed-register review refused")
    facts = review["results"][0]["preview"]["result"]["changes"]
    require(facts["posts"] is False, "import review invented a posting")
    if not facts.get("already_imported"):
        require(len(facts["rows"]) == 5, "import review omitted source rows")
        require(any(row["source_tax_facts"] == {"status": "unknown"} for row in facts["rows"]), "unknown tax facts lost")
        require(any(row["source_tax_facts"].get("tax_total") == "0" for row in facts["rows"]), "supplied zero tax lost")
    result = commit(op, review)
    require(succeeded(result), module + " mixed-register commit refused")
    batch = result["results"][0]["result"]["result"]["record"]
    require(write(op)["id"] == batch["id"], "identical import retry created a new batch")
    changed = operation(op["key"], **(values | {"csv": values["csv"].replace("source-system", "changed-system")}))
    require(preview(changed).get("valid") is False, "changed import retry accepted")
    tie(module)

exception = "Disposable parity proof: explicitly record the permitted same-person approval exception."
for module in ["receivables", "payables"]:
    prefix = "finance." + module
    records = read(prefix + ".document.list", source="opening")["records"]
    documents = {row["opening"]["source_id"]: row for row in records}
    kind = {"kind" if module == "receivables" else "document_type": "opening_credit"}
    require(len(read(prefix + ".document.list", source="opening", **kind)["records"]) == 3, "credit list filter omitted rows")
    require(read(prefix + ".document.page", source="opening", **kind)["total"] == 3, "credit page filter omitted rows")
    credit, target = documents["credit"]["id"], documents["invoice"]["id"]
    before = aging(module)
    wrong_party = operation(prefix + ".credit.apply", credit_document_id=documents["zero-credit"]["id"], target_document_id=documents["zero-invoice"]["id"], amount="1", effective_date="2026-07-10", reference="MCP-wrong-party")
    require(preview(wrong_party).get("valid") is False, "different-party application accepted")
    foreign = {"key": prefix + ".credit.apply", "input": {"company_id": 10, "credit_document_id": credit, "target_document_id": target, "amount": "1", "effective_date": "2026-07-10", "reference": "MCP-no-workplace"}}
    require(preview(foreign).get("valid") is False, "application outside the person's workplaces accepted")
    common = {"credit_document_id": credit, "effective_date": "2026-07-10", "override_reason": exception}
    apply = operation(prefix + ".credit.apply", **common, target_document_id=target, amount="20", reference="MCP-same-control")
    review = preview(apply)
    require(review.get("valid") is True, "application review refused: " + json.dumps([r.get("error") for r in review.get("results", [])]))
    require(review["results"][0]["preview"]["result"]["changes"]["posts"] is False, "same-control review invented an entry")
    stale = operation(prefix + ".credit.refund", **common, amount="20", cash_account_id=config["bank"], reference="MCP-stale-refund")
    stale_review = preview(stale)
    require(stale_review.get("valid") is True, "pre-application refund review refused")
    result = commit(apply, review)
    require(succeeded(result), "same-control application failed")
    event = result["results"][0]["result"]["result"]["record"]
    require(event["entry_id"] is None, "same-control application created an entry")
    require(write(apply)["id"] == event["id"], "identical settlement retry created another event")
    changed = operation(prefix + ".credit.apply", **common, target_document_id=target, amount="1", reference="MCP-same-control")
    require(preview(changed).get("valid") is False, "changed settlement retry accepted")
    require(not succeeded(commit(stale, stale_review)), "stale refund review double-spent a credit")
    incompatible = invoke("read", {"key": prefix + ".aging.get", "input": {"company_id": company, "as_of": "2026-07-10", "through_entry_number": 0, "through_opening_batch_id": before["through_opening_batch_id"], "through_settlement_number": event["settlement_number"]}})
    require("error" in incompatible, "entryless application was split from a referenced posting")
    after = aging(module)
    require(before["through_entry_number"] == after["through_entry_number"] and before["through_opening_batch_id"] == after["through_opening_batch_id"] and before["through_settlement_number"] < after["through_settlement_number"], "entryless application moved the wrong boundaries")
    require(aging(module, **pins(before)) == before, "old tuple changed after application")
    original = read(prefix + ".document.get", document_id=credit, as_of="2026-07-31", **pins(before))
    current = read(prefix + ".document.get", document_id=credit, as_of="2026-07-31", **pins(after))
    require(str(original["outstanding"]).startswith("30") and str(current["outstanding"]).startswith("10"), "record drill-down ignored the captured tuple")
    too_much = operation(prefix + ".credit.apply", **common, target_document_id=target, amount="11", reference="MCP-overdraw")
    refusal = preview(too_much)
    require(refusal.get("valid") is False and "amount" in json.dumps(refusal), "amount refusal did not identify its field")
    reverse = operation(prefix + ".settlement.reverse", settlement_id=event["id"], effective_date="2026-07-15", reference="MCP-reverse-same", override_reason=exception)
    require(write(reverse)["entry_id"] is None, "entryless reversal created an entry")
    tie(module)
    backdated = operation(prefix + ".credit.apply", **(common | {"effective_date": "2026-07-12"}), target_document_id=target, amount="25", reference="MCP-backdated-overdraw")
    require(preview(backdated).get("valid") is False, "backdating ignored the later restoration")
    # Competing reviewed writes may commit only one reservation of the residual.
    races = [operation(prefix + ".credit.apply", **(common | {"effective_date": "2026-07-16"}), target_document_id=target, amount="20", reference="MCP-race-" + str(i)) for i in range(2)]
    reviews = [preview(op) for op in races]
    require(all(review.get("valid") is True for review in reviews), "race reviews refused before any commit")
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
        results = list(executor.map(lambda pair: commit(*pair), zip(races, reviews)))
    require(sum(succeeded(result) for result in results) == 1, "competing reviews double-spent or both failed")
    winner = next(result["results"][0]["result"]["result"]["record"] for result in results if succeeded(result))
    write(operation(prefix + ".settlement.reverse", settlement_id=winner["id"], effective_date="2026-07-17", reference="MCP-reverse-race", override_reason=exception))
    transfer = write(operation(prefix + ".credit.apply", credit_document_id=documents["historical-credit"]["id"], target_document_id=target, amount="10", effective_date="2026-07-20", reference="MCP-transfer", override_reason=exception))
    require(transfer["entry_id"] is not None, "different-control application has no owned entry")
    transferred=aging(module)
    split=pins(transferred) | {"through_settlement_number": transfer["settlement_number"] - 1}
    incompatible=invoke("read", {"key": prefix + ".aging.get", "input": {"company_id": company, "as_of": "2026-07-31", **split}})
    require("error" in incompatible, "split entry/settlement pins accepted")
    tie(module)
    refund = write(operation(prefix + ".credit.refund", credit_document_id=credit, amount="5", cash_account_id=config["bank"], effective_date="2026-07-21", reference="MCP-refund", override_reason=exception))
    require(refund["entry_id"] is not None, "refund has no owned entry")
    tie(module)
    for event in [refund, transfer]:
        reversal = write(operation(prefix + ".settlement.reverse", settlement_id=event["id"], effective_date="2026-07-25", reference="MCP-reverse-" + str(event["id"]), override_reason=exception))
        require(reversal["entry_id"] is not None, "owned reversal entry missing")
        tie(module)
    require(aging(module, **pins(before)) == before, "old tuple changed after later refunds and reversals")
print("PASS: both MCP mixed imports retain tax facts and refuse changed retries; shared reviews, field refusals, stale and competing plans, entryless replay, transfers, refunds, owned reversals and per-control tie-outs")
