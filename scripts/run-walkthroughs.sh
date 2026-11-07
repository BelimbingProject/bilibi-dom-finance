#!/usr/bin/env bash
# Runs this Domain's walkthrough fixtures (fixtures/walkthrough) so they cannot
# rot unnoticed: CI used to build them (clippy --all-targets) and never run one.
#
# Run it from a Platform checkout with this repository mounted at
# apps/domains/finance and the walkthrough binaries already built
# (`cargo build --locked -p bilibi-finance-walkthrough-fixture --bins`).
# DATABASE_URL names a PostgreSQL 18 role that may create databases; every
# walkthrough gets its own empty database, dropped afterwards, because each one
# expects a fresh one. Nothing here is a secret or a product seed.
#
#   scripts/run-walkthroughs.sh               every walkthrough in WALKTHROUGHS
#   scripts/run-walkthroughs.sh sst vendors   only the named ones
#
# A walkthrough passes when it exits 0, or, for one that ends by serving the
# app for a browser, when it has finished its proofs and accepts connections
# (it is then stopped by its own PID). Failing ones are all reported, then the
# script exits 1.
#
# To add one: build its binary, add a line to WALKTHROUGHS (name, binary,
# arguments) and, if it needs more environment, a case below. A walkthrough
# that needs a person at a browser, OAuth credentials from another run or a
# mounted sibling Domain does not belong here; say why in
# fixtures/walkthrough/README.md.
set -euo pipefail

root=$(pwd -P)
: "${DATABASE_URL:?DATABASE_URL must name a PostgreSQL 18 role that may create databases}"
target=${CARGO_TARGET_DIR:-$root/target}
profile=${WALKTHROUGH_PROFILE:-debug}
timeout_seconds=${WALKTHROUGH_TIMEOUT:-900}
admin_url="${DATABASE_URL%/*}/postgres"
work=$(mktemp -d)
logs=${WALKTHROUGH_LOGS:-$work/logs}
mkdir -p "$logs"

# name|binary|arguments|prepare|speed. prepare "seed" migrates and applies the
# production seeds first, for a proof that starts from Core's seeded currency
# list. speed "slow" runs only with WALKTHROUGH_SLOW=1 (the nightly and manual runs, not a pull request).
WALKTHROUGHS=(
  'fixture|bilibi-finance-walkthrough-fixture|'
  'approval-journals|approval-journals|'
  'auditor-pack|auditor-pack|'
  'bad-debt|bad-debt|'
  'chart-templates|chart-templates|'
  'conversion|conversion|||slow'
  'engagement-access|engagement-access|'
  'fixed-assets|fixed-assets|'
  'fixed-assets-runs|fixed-assets-runs|'
  'fixed-assets-ui|fixed-assets-ui|'
  'opening-balances|opening-balances|'
  'order-invoices|order-invoices|absent'
  'party-code|party-code|'
  'receivables-companies|receivables-companies|'
  'report-controls|report-controls|'
  'reports-ui|reports-ui|'
  'sst|sst|'
  'vendors|vendors|'
  'einvoice-ui|einvoice-ui|'
  'einvoice-pages|einvoice-pages|'
  'bills-ui|bills-ui|'
  'ledger-ui|ledger-ui|'
  'ledger-ui-opening-proof|ledger-ui|opening-proof'
  'ledger-ui-book-proof|ledger-ui|book-proof|seed'
  'finance-ui|finance-ui|'
  'companies-ui|companies-ui|'
  'adopter-ui|adopter-ui|'
  'reconciliation-ui|bilibi-finance-reconciliation-ui|'
  'reconciliation-pages|bilibi-finance-reconciliation-pages|'
  'attachments-ui|bilibi-finance-walkthrough-ui|'
)

# Walkthroughs that fail today for a reason that needs a product decision, not
# a fixture edit. They still run and are reported (a GitHub warning), but do not
# fail the run; one that passes is reported so its line can be removed.
KNOWN_BROKEN=(
  # Issuing an invoice without Companies view returns a raw Forbidden from
  # Core's relationship role read (receivables/src/companies.rs `customer_role`),
  # which the live transport treats as fatal; the field refusal "needs
  # Companies view permission to check the customer relationship" was dropped
  # by #136. Stops the combined walkthrough at Receivables NR2.
  fixture
  # Mapping a historical supplier key now ends in a bare Invalid
  # (finance_payables_vendor_keys); this section builds old-system key
  # evidence, which the no-backfill rule says the product no longer supports.
  vendors
)

step() { printf '\n== %s\n' "$*"; }

free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
PY
}

failed=()
selected=("$@")
for entry in "${WALKTHROUGHS[@]}"; do
  IFS='|' read -r name binary args prepare speed <<<"$entry"
  if [[ ${#selected[@]} -gt 0 ]] && [[ ! " ${selected[*]} " == *" $name "* ]]; then
    continue
  fi
  if [[ $speed == slow && ${WALKTHROUGH_SLOW:-} != 1 && ${#selected[@]} -eq 0 ]]; then
    echo "SKIP $name (slow; set WALKTHROUGH_SLOW=1)"
    continue
  fi
  step "Walkthrough: $name"
  database="walkthrough_${name//[^a-z0-9]/_}_$$"
  psql "$admin_url" -v ON_ERROR_STOP=1 -qc "CREATE DATABASE $database"
  if [[ $prepare == seed ]]; then
    app="$target/$profile/bilibi-web"
    DATABASE_URL="${DATABASE_URL%/*}/$database" "$app" migrate >"$logs/$name.seed.log" 2>&1
    DATABASE_URL="${DATABASE_URL%/*}/$database" BILIBI_ENV=development "$app" seed-production >>"$logs/$name.seed.log" 2>&1
  fi
  port=$(free_port)
  dir="$work/$name"
  # shellcheck disable=SC2174 # $work is already private; only $dir needs the mode
  mkdir -m 700 -p "$dir"
  log="$logs/$name.log"
  started=$(date +%s)
  (
    export DATABASE_URL="${DATABASE_URL%/*}/$database"
    export APP_ORIGIN="http://127.0.0.1:$port" BIND_ADDR="127.0.0.1:$port"
    export BILIBI_SETTINGS_KEY
    BILIBI_SETTINGS_KEY=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
    export BILIBI_ENV=development BILIBI_AGENT_PROOF=disposable
    export BILIBI_DISPOSABLE_APPROVALS=yes
    export FINANCE_UI_STORAGE="$dir/storage" NC3_STORAGE_ROOT="$dir/nc3" NC4_STORAGE_ROOT="$dir/nc4"
    export AUDITOR_PACK_PROOF_DIR="$dir/pack" BAD_DEBT_STORAGE="$dir/bad-debt"
    export PARTY_CODE_STORAGE_ROOT="$dir/party" BILIBI_ATTACHMENTS_DEMO_ROOT="$dir/attachments"
    # Private storage roots refuse a folder others can read.
    # shellcheck disable=SC2174 # the mode is wanted on the leaf folders only
    mkdir -p -m 700 "$dir"/{storage,nc3,nc4,pack,bad-debt,party,attachments}
    # shellcheck disable=SC2086
    exec "$target/$profile/$binary" $args
  ) >"$log" 2>&1 &
  pid=$!
  verdict=''
  for ((i = 0; i < timeout_seconds; i++)); do
    if ! kill -0 "$pid" 2>/dev/null; then
      if wait "$pid"; then verdict=pass; else verdict=fail; fi
      break
    fi
    # Still running: either busy proving, or serving after its proofs passed.
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
      verdict=pass
      break
    fi
    sleep 1
  done
  if [[ -z $verdict ]]; then verdict=timeout; fi
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  psql "$admin_url" -qc "DROP DATABASE IF EXISTS $database WITH (FORCE)" || true
  elapsed=$(($(date +%s) - started))
  broken=0
  [[ " ${KNOWN_BROKEN[*]} " == *" $name "* ]] && broken=1
  if [[ $verdict == pass && $broken == 1 ]]; then
    echo "PASS $name (${elapsed}s) but it is listed in KNOWN_BROKEN: remove it from the list"
  elif [[ $verdict == pass ]]; then
    echo "PASS $name (${elapsed}s)"
  elif [[ $broken == 1 ]]; then
    echo "::warning::walkthrough $name is known to fail and still does ($verdict)"
    echo "KNOWN-BROKEN $name ($verdict, ${elapsed}s); last output:"
    tail -n 6 "$log"
  else
    echo "FAIL $name ($verdict, ${elapsed}s); last output:"
    tail -n 40 "$log"
    failed+=("$name")
  fi
done

if [[ ${#failed[@]} -gt 0 ]]; then
  printf '\nFailed walkthroughs: %s\n' "${failed[*]}" >&2
  exit 1
fi
printf '\nAll walkthroughs passed\n'
rm -rf "$work"
