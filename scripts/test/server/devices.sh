#!/usr/bin/env bash
# Server device endpoints: register -> list -> get -> delete
# Requires: curl, jq
#
# Usage:
#   SERVER_HOST=192.168.1.10 ./devices.sh
#   ./devices.sh 192.168.1.10

set -euo pipefail

SERVER_HOST="${1:-${SERVER_HOST:-127.0.0.1}}"
SERVER_PORT="${SERVER_PORT:-8000}"
BASE="http://${SERVER_HOST}:${SERVER_PORT}/api"

# A fixed id so a re-run updates the same row instead of piling up test devices.
DEVICE_ID="${DEVICE_ID:-99999999-9999-4999-8999-999999999999}"
DEVICE_NAME="${DEVICE_NAME:-TV-SCRIPT-TEST}"
DEVICE_IP="${DEVICE_IP:-192.0.2.99}"

pass()    { printf "[PASS] %s\n" "$1"; }
fail()    { printf "[FAIL] %s\n" "$1"; exit 1; }
section() { printf "\n=== %s ===\n" "$1"; }

echo "server device endpoints -> $BASE"
echo "test device:               $DEVICE_NAME ($DEVICE_ID)"

section "register"
curl -sf -X POST "$BASE/devices/register" \
  -H "Content-Type: application/json" \
  -d "{\"id\":\"$DEVICE_ID\",\"name\":\"$DEVICE_NAME\",\"ip\":\"$DEVICE_IP\"}" | jq .
pass "registered"

section "register again (must be idempotent)"
curl -sf -X POST "$BASE/devices/register" \
  -H "Content-Type: application/json" \
  -d "{\"id\":\"$DEVICE_ID\",\"name\":\"$DEVICE_NAME\",\"ip\":\"$DEVICE_IP\"}" > /dev/null
count=$(curl -sf "$BASE/devices" | jq "[.[] | select(.id==\"$DEVICE_ID\")] | length")
[[ "$count" == "1" ]] || fail "expected exactly 1 row for the test device, got $count"
pass "re-register did not duplicate the row"

section "list"
curl -sf "$BASE/devices" | jq .
pass "list returned"

section "get by id"
name=$(curl -sf "$BASE/devices/$DEVICE_ID" | jq -r '.name')
[[ "$name" == "$DEVICE_NAME" ]] || fail "expected $DEVICE_NAME, got $name"
pass "get returned the right device"

section "get unknown id (expect 404)"
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/devices/00000000-0000-4000-8000-000000000000")
[[ "$code" == "404" ]] || fail "expected 404 for unknown id, got $code"
pass "unknown id -> 404"

section "register with blank name (expect 400)"
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/devices/register" \
  -H "Content-Type: application/json" \
  -d "{\"id\":\"$DEVICE_ID\",\"name\":\"\",\"ip\":\"$DEVICE_IP\"}")
[[ "$code" == "400" ]] || fail "expected 400 for blank name, got $code"
pass "blank name -> 400"

section "delete"
curl -sf -X DELETE "$BASE/devices/$DEVICE_ID" | jq .
code=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/devices/$DEVICE_ID")
[[ "$code" == "404" ]] || fail "expected 404 deleting twice, got $code"
pass "delete removed the device and is not repeatable"

printf "\nAll tests passed.\n"
