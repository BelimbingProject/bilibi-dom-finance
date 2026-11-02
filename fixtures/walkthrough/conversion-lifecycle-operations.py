#!/usr/bin/env python3
"""Live opening-register correction MCP walkthrough, not a unit-test runner. Use only the disposable
database and private credentials provisioned by `agent-operations conversion-credentials`.
That fixture mode uses the browser reviewer for equivalent Core fact access.
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


require(sql("SELECT count(*) FROM users WHERE id=1 AND email='manager@example.test'") == "1", "not the disposable fixture")


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
    require("results" in body, f"{key}: commit refused: {body.get('error')}; fields={body.get('fields')}")
    return body["results"][0]["result"]["result"]["record"]


def refused(channel, key, company, contains, who="manager", **values):
    body = plan(channel, [operation(key, company, **values)], who)
    require(body.get("valid") is False and contains in json.dumps(body), f"{key}: expected refusal {contains}")


def opening_classifications(context):
    roles = {"1100": "receivables_control", "2000": "payables_control"}
    return [
        {"account_id": account["id"], "classification": roles.get(account["code"], "not_a_control")}
        for account in context["opening_accounts"]
    ]


# Company 001 is exercised in the browser; Company 002 uses the MCP door.
channel, company = "mcp", 2
inputs = {}
original = {}
for module in ["receivables", "payables", "fixed_assets"]:
    context = read(channel, f"finance.{module}.opening.context", company, "approver")
    # An interrupted wire proof may already have committed its original register.
    # Repeating the exact original import exercises the owner’s safe retry path.
    if module == "receivables":
        series=read(channel,"finance.receivables.series.list",company,"approver")["records"]
        series_id=next(r["id"] for r in series if r["code"]=="OPENING")
        csv="source_id,customer_code,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\ninvoice-17,company_010,OLD-INV-17,2026-05-15,2026-06-30,5000,3000,1100,company,invoice,invoice,source-system,MYR,unknown\n"
        values={"source_reference":"AR-SOURCE","currency_code":"MYR","series_id":series_id,"csv":csv,"opening_account_classifications":opening_classifications(context)}
    elif module == "payables":
        csv="source_id,vendor_code,vendor_name,document_reference,document_date,due_date,original_total,outstanding,control_account_code,party_kind,item_kind,source_kind,source_system,currency_code,source_tax_facts\nbill-29,company_010,Conversion vendor,OLD-BILL-29,2026-05-01,2026-06-15,2500,1500,2000,company,bill,bill,source-system,MYR,unknown\n"
        values={"source_reference":"AP-SOURCE","currency_code":"MYR","csv":csv,"opening_account_classifications":opening_classifications(context)}
    else:
        revision=next(r["id"] for r in context["category_revisions"] if r["category_name"]=="Conversion assets")
        csv=f"source_id,description,category_revision_id,acquisition_date,in_service_date,cost,accumulated_depreciation,remaining_life_months,residual_value\nasset-3,Source register asset,{revision},2024-06-01,2024-07-01,12000,1800,96,0\n"
        values={"source_reference":"ASSET-SOURCE","currency_code":"MYR","csv":csv}
    original[module]=write(channel,f"finance.{module}.opening.import",company,**values)
    inputs[module]=values
before_trial=read(channel,"finance.ledger.trial_balance.get",company,"approver",as_of="2026-08-01")
for module,values in inputs.items():
    key=f"finance.{module}.opening"
    changed_csv=values["csv"].replace(",5000,3000",",5100,3000").replace(",2500,1500",",2600,1500").replace(",1800,96,",",1800,120,")
    staged=values|{"source_reference":values["source_reference"]+"-CORRECTED","csv":changed_csv,"replaces_batch_id":original[module]["id"],"reason":"Correct source register detail","part":1,"part_count":1}
    preview=plan(channel,[operation(key+".stage",company,**staged)],"manager")
    require(preview.get("valid") is True,"source staging review refused")
    facts=preview["results"][0]["preview"]["result"]["changes"]
    require(facts["state"]=="previewed" and facts["posts"] is False,"staging review state")
    committed=commit(channel,[operation(key+".stage",company,**staged)],preview,"manager")
    require(committed["results"][0]["result"]["result"]["record"]["state"]=="committed","staging commit state")
    refused(channel,key+".stage",company,"different source bytes",who="manager",**(staged|{"csv":changed_csv+"extra"}))
    finalize=staged|{"staged":True,"csv":""}
    refused(channel,key+".import",company,"separation",who="manager",**finalize)
    ops=[operation(key+".import",company,**finalize)]
    preview=plan(channel,ops,"approver")
    require(preview.get("valid") is True,"replacement preview refused")
    facts=preview["results"][0]["preview"]["result"]["changes"]
    require(facts["state"]=="previewed" and facts["posts"] is False,"replacement review state")
    record=commit(channel,ops,preview,"approver")["results"][0]["result"]["result"]["record"]
    require(record["state"]=="reconciled","final reconciliation state")
    require(write(channel,key+".import",company,**finalize)["id"]==record["id"],"replacement retry duplicated")
    context=read(channel,key+".context",company,"approver")
    require(len(context["decisions"])==2 and len(context["stages"])==1,"source or approvals missing")
    require(all("payload" not in stage and stage["part_count"]==1 for stage in context["stages"]),"context returned staged source payloads")
    require(all("rows" not in decision["review"] and decision["review"]["row_count"]>=1 for decision in context["decisions"] if decision["replaces_batch_id"]),"decision review kept every source row")
    page=read(channel,key+".context",company,"approver",limit=1)
    rest=read(channel,key+".context",company,"approver",limit=1,items_after=page["items_next"])
    require(len(page["items"])==1 and page["item_count"]==len(context["items"])>=2 and rest["items"] and rest["items"][0]["id"]>page["items"][0]["id"],"context item pages differ")
    require(write(channel,key+".import",company,**values)==original[module],"historical retry changed")
    ui=read(channel,key+".context",1,"approver")
    require(ui["batch"]["source_reference"]==record["source_reference"],"UI/MCP source references differ")
    ui_facts=ui["decisions"][-1]["review"]
    for field in ["state","reason","currency_code","minor_units","total","opening_balance","difference","posts","depreciation"]:
        require(ui_facts.get(field)==facts.get(field),f"UI/MCP reviewed facts differ: {module}/{field}")
    if module in ("receivables", "payables"):
        def without_ids(rows):
            return [{k:v for k,v in row.items() if k != "account_id"} for row in rows]
        require(without_ids(ui_facts["reconciliation"]) == without_ids(facts["reconciliation"]),f"UI/MCP account reconciliations differ: {module}")
require(read(channel,"finance.ledger.trial_balance.get",company,"approver",as_of="2026-08-01")==before_trial,"corrections posted entries")
for module in ["receivables","payables"]:
    live=read(channel,f"finance.{module}.aging.get",company,"approver",as_of="2026-08-01")
    old=read(channel,f"finance.{module}.aging.get",company,"approver",as_of="2026-07-31")
    ui=read(channel,f"finance.{module}.aging.get",1,"approver",as_of="2026-08-01")
    require(live["total"]==old["total"]==ui["total"],"UI/MCP totals or dated boundary differ")
print("MCP lifecycle: real reviewed staging and reconciliation states, changed-source and independent-approval refusals, append-only corrections in all modules, safe current and historical retries, unchanged ledger postings, and equivalent UI/MCP review facts and totals passed.")
