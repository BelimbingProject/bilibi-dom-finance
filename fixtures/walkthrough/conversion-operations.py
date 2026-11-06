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


def opening_classifications(context):
    roles = {
        "1100": "receivables_control",
        "1110": "receivables_control",
        "2000": "payables_control",
        "2010": "payables_control",
    }
    return [
        {"account_id": account["id"], "classification": roles.get(account["code"], "not_a_control")}
        for account in context["opening_accounts"]
    ]


# Run after the conversion fixture's Company 001 browser proof.
channel, company = "mcp", 2
for module in ["receivables", "payables", "fixed_assets"]:
    key=f"finance.{module}.opening"
    require(invoke(channel,"describe",{"key":key+".import"}), "missing import schema")
    require(invoke(channel,"guide",{"key":key}), "missing conversion guide")
    context=read(channel,key+".context",company,"approver")
    require(context["batch"] is None and context["items"]==[], "needs a clean opening register")

multi_company = 4
multi_ar_context = read(channel, "finance.receivables.opening.context", multi_company, "approver")
multi_ap_context = read(channel, "finance.payables.opening.context", multi_company, "approver")
multi_series = read(channel, "finance.receivables.series.list", multi_company, "approver")["records"]
multi_inputs = {
    "receivables": {
        "source_reference": "MULTI-AR",
        "currency_code": "MYR",
        "series_id": next(row["id"] for row in multi_series if row["code"] == "OPENING"),
        "csv": "source_id,customer_code,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\ninvoice-17,company_010,OLD-INV-17,2026-05-15,2026-06-30,5000,2500,1100,company,invoice,invoice,source-system,MYR,unknown\ninvoice-18,company_010,OLD-INV-18,2026-04-15,2026-05-31,1000,500,1110,company,invoice,invoice,source-system,MYR,unknown\n",
        "opening_account_classifications": opening_classifications(multi_ar_context),
    },
    "payables": {
        "source_reference": "MULTI-AP",
        "currency_code": "MYR",
        "csv": "source_id,vendor_code,vendor_name,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\nbill-29,company_010,Conversion vendor,OLD-BILL-29,2026-05-01,2026-06-15,2500,1200,2000,company,bill,bill,source-system,MYR,unknown\nbill-30,company_010,Conversion vendor,OLD-BILL-30,2026-04-01,2026-05-15,900,300,2010,company,bill,bill,source-system,MYR,unknown\n",
        "opening_account_classifications": opening_classifications(multi_ap_context),
    },
}
for module, expected, omitted_code, omitted_amount in [
    ("receivables", {"1100", "1110"}, "1110", "500.00"),
    ("payables", {"2000", "2010"}, "2010", "-300.00"),
]:
    key = f"finance.{module}.opening.import"
    review = plan(channel, [operation(key, multi_company, **multi_inputs[module])], "approver")
    require(review.get("valid") is True, f"{module}: multi-control preview refused")
    facts = review["results"][0]["preview"]["result"]["changes"]
    require({row["account_code"] for row in facts["reconciliation"]} == expected, f"{module}: multi-control review is incomplete")
    require(all(Decimal(row["difference"]) == 0 for row in facts["reconciliation"]), f"{module}: multi-control review does not reconcile")
    submitted = {str(row["account_id"]): row["classification"] for row in multi_inputs[module]["opening_account_classifications"]}
    require(facts["opening_account_classifications"] == submitted, f"{module}: reviewed classifications changed")
    omitted_id = next(account["id"] for account in (multi_ar_context if module == "receivables" else multi_ap_context)["opening_accounts"] if account["code"] == omitted_code)
    incomplete = multi_inputs[module] | {"opening_account_classifications": [dict(row) for row in multi_inputs[module]["opening_account_classifications"] if row["account_id"] != omitted_id]}
    refusal = plan(channel, [operation(key, multi_company, **incomplete)], "approver")
    refusal_text = json.dumps(refusal)
    require(refusal.get("valid") is False and omitted_code in refusal_text and omitted_amount in refusal_text, f"{module}: omitted historical control was not named")
trial=read(channel,"finance.ledger.trial_balance.get",company,"approver",as_of="2026-07-01")
require(Decimal(trial["total_debit"])==Decimal("27000"),"source trial balance")
accounts={r["code"]:r["id"] for r in trial["rows"]}
series=read(channel,"finance.receivables.series.list",company,"approver")["records"]
invoice_series=next(r["id"] for r in series if r["code"]=="OPENING")
payment_series=next(r["id"] for r in series if r["code"]=="RECEIPT")
customer=next(r for r in read(channel,"finance.receivables.customer.list",company,"approver")["records"] if r["code"]=="company_010")
vendor=next(r for r in read(channel,"finance.payables.vendor.list",company,"approver")["records"] if r["code"]=="company_010")
asset_context=read(channel,"finance.fixed_assets.opening.context",company,"approver")
revision=next(r["id"] for r in asset_context["category_revisions"] if r["category_name"]=="Conversion assets")
ar_csv="source_id,customer_code,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\ninvoice-17,company_010,OLD-INV-17,2026-05-15,2026-06-30,5000,3000,1100,company,invoice,invoice,source-system,MYR,unknown\n"
ap_csv="source_id,vendor_code,vendor_name,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\nbill-29,company_010,Conversion vendor,OLD-BILL-29,2026-05-01,2026-06-15,2500,1500,2000,company,bill,bill,source-system,MYR,unknown\n"
asset_csv=f"source_id,description,category_revision_id,acquisition_date,in_service_date,cost,accumulated_depreciation,remaining_life_months,residual_value\nasset-3,Source register asset,{revision},2024-06-01,2024-07-01,12000,1800,96,0\n"
before_import={module:read(channel,f"finance.{module}.aging.get",company,"approver",as_of="2026-07-01") for module in ["receivables","payables"]}
ar_context=read(channel,"finance.receivables.opening.context",company,"approver")
ap_context=read(channel,"finance.payables.opening.context",company,"approver")
inputs={"receivables":{"source_reference":"AR-SOURCE","currency_code":"MYR","series_id":invoice_series,"csv":ar_csv,"opening_account_classifications":opening_classifications(ar_context)},"payables":{"source_reference":"AP-SOURCE","currency_code":"MYR","csv":ap_csv,"opening_account_classifications":opening_classifications(ap_context)},"fixed_assets":{"source_reference":"ASSET-SOURCE","currency_code":"MYR","csv":asset_csv}}
for module,old,new in [("receivables",",3000,1100",",3001,1100"),("payables",",1500,2000",",1501,2000"),("fixed_assets",",1800,96",",1801,96")]:
    changed=inputs[module]|{"csv":inputs[module]["csv"].replace(old,new)}
    refused(channel,f"finance.{module}.opening.import",company,"difference",who="approver",**changed)
    require(sql(f"SELECT count(*) FROM finance_{module}_opening_batches WHERE company_id=2")=="0","mismatch wrote records")
refused(channel,"finance.receivables.opening.import",company,"separation",who="manager",**inputs["receivables"])
ops=[operation("finance.receivables.opening.import",company,**inputs["receivables"])]
review=plan(channel,ops+ops,"approver")
require(review.get("valid") is True,"double-plan initial review")
require(commit(channel,ops+ops,review,"approver").get("error")=="record_changed","dependent import plan must roll back")
require(sql("SELECT count(*) FROM finance_receivables_opening_batches WHERE company_id=2")=="0","dependent plan left import")
print("MCP: schemas and guides discovered; two-account opening reviews and omissions proved; all three mismatches and same-preparer approval refused; dependent import plan rolled back")
for module,values in inputs.items():
    key=f"finance.{module}.opening.import"
    record=write(channel,key,company,**values)
    require(write(channel,key,company,**values)["id"]==record["id"],"identical import retry duplicated batch")
    refused(channel,key,company,"current register",who="approver",**(values|{"source_reference":"RENAMED"}))
require(read(channel,"finance.ledger.trial_balance.get",company,"approver",as_of="2026-07-01")==trial,"imports changed ledger balances")
for module,expected in [("receivables","3000"),("payables","1500")]:
    report=read(channel,f"finance.{module}.aging.get",company,"approver",as_of="2026-07-01")
    require(Decimal(report["total"])==Decimal(expected),"opening aging does not reconcile")
    before=read(channel,f"finance.{module}.aging.get",company,"approver",as_of="2026-06-30")
    require(Decimal(before["total"])==0,"opening item visible before conversion")
for module,report in before_import.items():
    reproduced=read(channel,f"finance.{module}.aging.get",company,"approver",as_of="2026-07-01",through_entry_number=report["through_entry_number"],through_opening_batch_id=report["through_opening_batch_id"],through_settlement_number=report["through_settlement_number"])
    require(reproduced==report,"opening import changed a reproduced aging report")
ar_context=read(channel,"finance.receivables.opening.context",company,"approver")
ap_context=read(channel,"finance.payables.opening.context",company,"approver")
invoice=ar_context["items"][0]["document_id"]
bill=ap_context["items"][0]["document_id"]
source_asset=read(channel,"finance.fixed_assets.opening.context",company,"approver")["items"][0]["source_facts"]
require(Decimal(source_asset["cost"])==12000 and Decimal(source_asset["accumulated_depreciation"])==1800 and Decimal(source_asset["carrying_value"])==10200,"asset source state changed")
receipt={"series_id":payment_series,"customer_id":customer["id"],"payment_date":"2026-07-15","amount":"1000","deposit_account_id":accounts["1000"],"allocations":[{"document_id":invoice,"amount":"1000"}]}
refused(channel,"finance.receivables.payment.record",company,"conversion",who="approver",**(receipt|{"payment_date":"2026-06-30"}))
write(channel,"finance.receivables.payment.record",company,**receipt)
payment_request=write(channel,"finance.payables.payment.submit",company,"manager",payment_date="2026-07-15",vendor_id=vendor["id"],paid_from_account_id=accounts["1000"],allocations=[{"document_id":bill,"amount":"500"}])
write(channel,"finance.payables.payment.approve",company,request_id=payment_request["id"])
final=read(channel,"finance.ledger.trial_balance.get",company,"approver",as_of="2026-07-31")
actual={r["code"]:(Decimal(r["debit_balance"]),Decimal(r["credit_balance"])) for r in final["rows"]}
for code,expected in {"1000":("10500","0"),"1100":("2000","0"),"2000":("0","1000"),"1200":("12000","0"),"1290":("0","1800"),"4000":("0","5500"),"5000":("2000","0")}.items():
    require(actual[code]==tuple(map(Decimal,expected)),"final account differs: "+code)
require(Decimal(read(channel,"finance.receivables.aging.get",company,"approver",as_of="2026-07-31")["total"])==2000,"AR settlement aging")
require(Decimal(read(channel,"finance.payables.aging.get",company,"approver",as_of="2026-07-31")["total"])==1000,"AP settlement aging")
require(sql("SELECT count(*) FROM finance_ledger_entries WHERE company_id=2")=="3","duplicate or unintended entries")
require(sql("SELECT count(*) FROM finance_receivables_document_tax_lines WHERE company_id=2")=="0","AR source tax was fabricated")
require(sql("SELECT count(*) FROM finance_payables_document_tax_lines WHERE company_id=2")=="0","AP source tax was fabricated")
# Compare values with the earlier browser company, whose record IDs differ.
ui=read(channel,"finance.ledger.trial_balance.get",1,"approver",as_of="2026-07-31")
ui_values={r["code"]:(Decimal(r["debit_balance"]),Decimal(r["credit_balance"])) for r in ui["rows"]}
require(ui_values==actual,"UI and MCP final trial balances differ")
print("MCP: three independently approved opening-register imports and identical retries; no additional opening entry or source tax; original aging dates and cutoff boundary retained; 1000 customer receipt and 500 independently approved supplier payment reconcile to bank 10500, receivables 2000 and payables 1000; UI and MCP final trial balances equal")
print("Reviews: 3 opening imports + 3 settlement writes; extra reviews exercised mismatches, retries and rollback. Asset cost/accumulation/carrying state matches; future depreciation run is proved separately through the facade/UI, because general asset lifecycle MCP operations are not yet available. Bank reconciliation items, opening credits, committed import replacement and historical transactions remain outside this proof.")
