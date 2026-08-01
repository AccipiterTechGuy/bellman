#!/usr/bin/env bash
# Round-trip smoke test for the Bellman CLI (AI-skill surface).
#
# Acceptance (card C4):
#   add all 7 occurrence kinds, list, edit time, next 5, pause/resume,
#   run-now, rm — asserting on --json output.
#
# Usage (from repo root):
#   ./tests/cli_roundtrip.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -z "${BELLMAN_BIN:-}" ]]; then
  echo "==> building bellman-cli"
  cargo build -p bellman-cli -q
  BELLMAN_BIN="$ROOT/target/debug/bellman"
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "error: python3 is required for JSON assertions" >&2
  exit 2
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/bellman-cli-rt.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

export BELLMAN_DB="$TMP/timers.db"
BELLMAN=("$BELLMAN_BIN" --json)

pass=0

ok() {
  pass=$((pass + 1))
  echo "  PASS  $1"
}

die() {
  echo "  FAIL  $1" >&2
  echo "        $2" >&2
  exit 1
}

# Extract a JSON field with python3. Usage: jget '<json>' 'python expr on obj'
# The expression is evaluated as: obj = json.loads(...); print(<expr>)
jget() {
  local json="$1"
  local expr="$2"
  python3 -c '
import json, sys
obj = json.loads(sys.argv[1])
print(eval(sys.argv[2], {"obj": obj}))
' "$json" "$expr"
}

run_json() {
  local out rc
  set +e
  out="$("${BELLMAN[@]}" "$@" 2>/dev/null)"
  rc=$?
  set -e
  if [[ $rc -ne 0 ]]; then
    echo "$out"
    return 1
  fi
  echo "$out"
}

assert_ok() {
  local label="$1"
  local json="$2"
  local ok_flag
  ok_flag="$(jget "$json" 'obj.get("ok")')"
  if [[ "$ok_flag" != "True" ]]; then
    die "$label" "expected ok=true, got: $json"
  fi
}

assert_cmd() {
  local label="$1"
  local json="$2"
  local want="$3"
  local got
  got="$(jget "$json" 'obj.get("command")')"
  if [[ "$got" != "$want" ]]; then
    die "$label" "expected command=$want, got=$got ($json)"
  fi
}

echo "==> parse-time error honors --json (invalid_args envelope)"
# Auditor REPRO: missing required --occurrence must not leave stdout empty.
set +e
PARSE_OUT="$("${BELLMAN[@]}" add --name broken 2>"$TMP/parse.err")"
PARSE_RC=$?
set -e
if [[ "$PARSE_RC" -eq 0 ]]; then
  die "parse error exit" "expected non-zero rc for missing --occurrence, got 0"
fi
if [[ -z "$PARSE_OUT" ]]; then
  die "parse error stdout" "expected JSON error on stdout, got empty (stderr=$(cat "$TMP/parse.err"))"
fi
assert_ok_false() {
  local label="$1"
  local json="$2"
  local ok_flag
  ok_flag="$(jget "$json" 'obj.get("ok")')"
  if [[ "$ok_flag" != "False" ]]; then
    die "$label" "expected ok=false, got: $json"
  fi
}
assert_ok_false "parse error ok" "$PARSE_OUT"
assert_cmd "parse error command" "$PARSE_OUT" "add"
CODE="$(jget "$PARSE_OUT" 'obj.get("error",{}).get("code")')"
if [[ "$CODE" != "invalid_args" ]]; then
  die "parse error code" "expected invalid_args, got $CODE: $PARSE_OUT"
fi
MSG="$(jget "$PARSE_OUT" 'obj.get("error",{}).get("message") or ""')"
if [[ -z "$MSG" || "$MSG" == "None" ]]; then
  die "parse error message" "expected non-empty message: $PARSE_OUT"
fi
# stderr must not be the only signal when --json is set (may be empty or unused)
ok "parse-time --json error envelope (rc=$PARSE_RC code=$CODE)"

echo "==> parse-time --db path 'list' does not steal command=add"
# Auditor REPRO #2: relative --db value equal to a subcommand name must not
# mislabel the JSON envelope command field.
set +e
# Invoke with explicit argv order (do not use BELLMAN array which already has --json).
PARSE2_OUT="$("$BELLMAN_BIN" --json --db list add --name broken 2>"$TMP/parse2.err")"
PARSE2_RC=$?
set -e
if [[ "$PARSE2_RC" -eq 0 ]]; then
  die "db-list steal exit" "expected non-zero rc, got 0"
fi
if [[ -z "$PARSE2_OUT" ]]; then
  die "db-list steal stdout" "expected JSON on stdout, empty (stderr=$(cat "$TMP/parse2.err"))"
fi
assert_ok_false "db-list steal ok" "$PARSE2_OUT"
assert_cmd "db-list steal command" "$PARSE2_OUT" "add"
CODE2="$(jget "$PARSE2_OUT" 'obj.get("error",{}).get("code")')"
if [[ "$CODE2" != "invalid_args" ]]; then
  die "db-list steal code" "expected invalid_args, got $CODE2: $PARSE2_OUT"
fi
ok "parse-time --db list add → command=add (not list)"

echo "==> add all 7 occurrence kinds"

JSON="$(run_json add --name once-job --occurrence once --time 2030-06-15T12:00:00 --tz UTC)" \
  || die "add once" "$JSON"
assert_ok "add once" "$JSON"
assert_cmd "add once" "$JSON" "add"
ONCE_ID="$(jget "$JSON" 'obj["timer"]["id"]')"
[[ -n "$ONCE_ID" && "$ONCE_ID" != "None" ]] || die "add once id" "$JSON"
ok "add once ($ONCE_ID)"

JSON="$(run_json add --name tick --occurrence interval --every-secs 60 --tz UTC)" \
  || die "add interval" "$JSON"
assert_ok "add interval" "$JSON"
INTERVAL_ID="$(jget "$JSON" 'obj["timer"]["id"]')"
ok "add interval ($INTERVAL_ID)"

JSON="$(run_json add --name daily-job --occurrence daily --time 09:30 --tz UTC)" \
  || die "add daily" "$JSON"
assert_ok "add daily" "$JSON"
DAILY_ID="$(jget "$JSON" 'obj["timer"]["id"]')"
ok "add daily ($DAILY_ID)"

JSON="$(run_json add --name weekly-job --occurrence weekly --days mon,wed,fri --time 10:00 --tz UTC)" \
  || die "add weekly" "$JSON"
assert_ok "add weekly" "$JSON"
WEEKLY_ID="$(jget "$JSON" 'obj["timer"]["id"]')"
ok "add weekly ($WEEKLY_ID)"

JSON="$(run_json add --name monthly-job --occurrence monthly --day 15 --time 08:00 --tz UTC)" \
  || die "add monthly" "$JSON"
assert_ok "add monthly" "$JSON"
MONTHLY_ID="$(jget "$JSON" 'obj["timer"]["id"]')"
ok "add monthly ($MONTHLY_ID)"

JSON="$(run_json add --name yearly-job --occurrence yearly --month 7 --day 4 --time 12:00 --tz UTC)" \
  || die "add yearly" "$JSON"
assert_ok "add yearly" "$JSON"
YEARLY_ID="$(jget "$JSON" 'obj["timer"]["id"]')"
ok "add yearly ($YEARLY_ID)"

JSON="$(run_json add --name cron-job --occurrence cron --cron '0 0 12 * * *' --tz UTC)" \
  || die "add cron" "$JSON"
assert_ok "add cron" "$JSON"
CRON_ID="$(jget "$JSON" 'obj["timer"]["id"]')"
ok "add cron ($CRON_ID)"

echo "==> list"
JSON="$(run_json list)" || die "list" "$JSON"
assert_ok "list" "$JSON"
assert_cmd "list" "$JSON" "list"
COUNT="$(jget "$JSON" 'obj.get("count")')"
if [[ "$COUNT" != "7" ]]; then
  die "list count" "expected 7 timers, got $COUNT: $JSON"
fi
ok "list count=7"

echo "==> edit time (daily 09:30 -> 11:45)"
JSON="$(run_json edit daily-job --time 11:45)" || die "edit time" "$JSON"
assert_ok "edit time" "$JSON"
assert_cmd "edit time" "$JSON" "edit"
# OccurrenceKind is #[serde(tag = "occ")]; daily has "at": "HH:MM:SS"
AT="$(jget "$JSON" 'obj["timer"]["occurrence"]["kind"].get("at","")')"
NEXT="$(jget "$JSON" 'obj["timer"].get("next_fire_utc") or ""')"
if [[ "$AT" != "11:45:00" ]]; then
  if [[ -z "$NEXT" || "$NEXT" == "None" ]]; then
    die "edit time" "expected daily at=11:45:00 or a next_fire; AT=$AT full=$JSON"
  fi
  if ! echo "$NEXT" | grep -q 'T11:45'; then
    die "edit time" "expected next_fire around 11:45, got AT=$AT NEXT=$NEXT full=$JSON"
  fi
fi
ok "edit daily time -> 11:45 (at=$AT next=$NEXT)"

echo "==> next 5 (daily-job)"
JSON="$(run_json next daily-job 5)" || die "next 5" "$JSON"
assert_ok "next 5" "$JSON"
assert_cmd "next 5" "$JSON" "next"
NFIRES="$(jget "$JSON" 'len(obj.get("fires") or [])')"
if [[ "$NFIRES" != "5" ]]; then
  die "next 5" "expected exactly 5 fires for daily, got $NFIRES: $JSON"
fi
ok "next 5 ($NFIRES fires)"

echo "==> pause / resume"
JSON="$(run_json pause daily-job)" || die "pause" "$JSON"
assert_ok "pause" "$JSON"
EN="$(jget "$JSON" 'obj["timer"]["enabled"]')"
if [[ "$EN" != "False" ]]; then
  die "pause" "expected enabled=false, got $EN: $JSON"
fi
ok "pause"

JSON="$(run_json resume daily-job)" || die "resume" "$JSON"
assert_ok "resume" "$JSON"
EN="$(jget "$JSON" 'obj["timer"]["enabled"]')"
if [[ "$EN" != "True" ]]; then
  die "resume" "expected enabled=true, got $EN: $JSON"
fi
ok "resume"

echo "==> run-now (interval tick)"
JSON="$(run_json run-now tick)" || die "run-now" "$JSON"
assert_ok "run-now" "$JSON"
assert_cmd "run-now" "$JSON" "run-now"
RUN_ID="$(jget "$JSON" 'obj.get("run_id")')"
MSG="$(jget "$JSON" 'obj.get("message") or ""')"
[[ -n "$RUN_ID" && "$RUN_ID" != "None" ]] || die "run-now run_id" "$JSON"
if ! echo "$MSG" | grep -qE '(action=none|notify stub)'; then
  die "run-now message" "expected 'action=none' or 'notify stub', got: $MSG"
fi
ok "run-now (run_id=$RUN_ID)"

echo "==> wake actions (SHIP1-E): launch / notify / edit / validation"
JSON="$(run_json add --name launcher --occurrence interval --every-secs 60 \
  --action launch --command /usr/bin/true --tz UTC)" || die "add launch" "$JSON"
assert_ok "add launch" "$JSON"
ATYPE="$(jget "$JSON" 'obj["timer"]["action"].get("type")')"
ACMD="$(jget "$JSON" 'obj["timer"]["action"].get("command")')"
if [[ "$ATYPE" != "launch" || "$ACMD" != "/usr/bin/true" ]]; then
  die "add launch action" "expected launch /usr/bin/true, got: $JSON"
fi
ok "add --action launch --command /usr/bin/true"

JSON="$(run_json run-now launcher)" || die "run-now launch" "$JSON"
assert_ok "run-now launch" "$JSON"
MSG="$(jget "$JSON" 'obj.get("message") or ""')"
if ! echo "$MSG" | grep -q 'launch ok exit=0'; then
  die "run-now launch message" "expected 'launch ok exit=0', got: $MSG"
fi
ok "run-now launch ran (exit=0)"

JSON="$(run_json add --name hyadd --occurrence interval --every-secs 60 \
  --action launch --command /usr/local/bin/backup.sh --args --full --tz UTC)" \
  || die "add launch hyphen arg" "$JSON"
assert_ok "add launch hyphen arg" "$JSON"
AARGS="$(jget "$JSON" 'obj["timer"]["action"].get("args")')"
if [[ "$AARGS" != "['--full']" ]]; then
  die "add launch hyphen arg" "expected ['--full'], got: $AARGS"
fi
ok "add --args --full (hyphen-leading, space form)"
JSON="$(run_json rm hyadd)" || die "rm hyadd" "$JSON"
assert_ok "rm hyadd" "$JSON"
ok "rm hyadd"

JSON="$(run_json edit launcher --action launch --command /bin/echo --args hello --args "two words")" \
  || die "edit launch args" "$JSON"
assert_ok "edit launch args" "$JSON"
AARGS="$(jget "$JSON" 'obj["timer"]["action"].get("args")')"
if [[ "$AARGS" != "['hello', 'two words']" ]]; then
  die "edit launch args" "expected ['hello', 'two words'], got: $AARGS"
fi
ok "edit --action launch --args (repeatable)"

# Hyphen-leading args (audit REPRO): flags are what launch commands almost
# always take; clap must accept them in the documented space form.
JSON="$(run_json edit launcher --action launch --command /bin/echo --args -m --args "two words")" \
  || die "edit launch hyphen args" "$JSON"
assert_ok "edit launch hyphen args" "$JSON"
AARGS="$(jget "$JSON" 'obj["timer"]["action"].get("args")')"
if [[ "$AARGS" != "['-m', 'two words']" ]]; then
  die "edit launch hyphen args" "expected ['-m', 'two words'], got: $AARGS"
fi
ok "edit --args -m (hyphen-leading, space form)"

JSON="$(run_json edit launcher --action launch --command /bin/echo --args --full)" \
  || die "edit launch double-dash arg" "$JSON"
AARGS="$(jget "$JSON" 'obj["timer"]["action"].get("args")')"
if [[ "$AARGS" != "['--full']" ]]; then
  die "edit launch double-dash arg" "expected ['--full'], got: $AARGS"
fi
ok "edit --args --full (double-dash value)"

JSON="$(run_json edit launcher --action launch --command /bin/echo --args=--full)" \
  || die "edit launch equals arg" "$JSON"
AARGS="$(jget "$JSON" 'obj["timer"]["action"].get("args")')"
if [[ "$AARGS" != "['--full']" ]]; then
  die "edit launch equals arg" "expected ['--full'], got: $AARGS"
fi
ok "edit --args=--full (equals form)"

JSON="$(run_json edit launcher --action notify --title "Hi" --body "there")" \
  || die "edit notify" "$JSON"
ATYPE="$(jget "$JSON" 'obj["timer"]["action"].get("type")')"
ATITLE="$(jget "$JSON" 'obj["timer"]["action"].get("title")')"
if [[ "$ATYPE" != "notify" || "$ATITLE" != "Hi" ]]; then
  die "edit notify" "expected notify/Hi, got: $JSON"
fi
ok "edit --action notify --title"

JSON="$(run_json edit launcher --action none)" || die "edit none" "$JSON"
ATYPE="$(jget "$JSON" 'obj["timer"]["action"].get("type")')"
if [[ "$ATYPE" != "none" ]]; then
  die "edit none" "expected action none, got: $JSON"
fi
ok "edit --action none (clears)"

set +e
BAD="$("${BELLMAN[@]}" add --name bad-act --occurrence interval --every-secs 60 --command /usr/bin/true 2>/dev/null)"
BAD_RC=$?
set -e
if [[ $BAD_RC -eq 0 ]]; then
  die "--command without --action" "expected non-zero rc"
fi
assert_ok_false "--command without --action ok" "$BAD"
ok "--command without --action rejected (invalid_args)"

set +e
BAD="$("${BELLMAN[@]}" add --name bad-act2 --occurrence interval --every-secs 60 --action launch 2>/dev/null)"
BAD_RC=$?
set -e
if [[ $BAD_RC -eq 0 ]]; then
  die "--action launch without --command" "expected non-zero rc"
fi
assert_ok_false "--action launch without --command ok" "$BAD"
ok "--action launch without --command rejected"

JSON="$(run_json rm launcher)" || die "rm launcher" "$JSON"
assert_ok "rm launcher" "$JSON"
ok "rm launcher"

echo "==> rm all 7 by name"
for name in once-job tick daily-job weekly-job monthly-job yearly-job cron-job; do
  JSON="$(run_json rm "$name")" || die "rm $name" "$JSON"
  assert_ok "rm $name" "$JSON"
  DEL="$(jget "$JSON" 'obj.get("deleted")')"
  if [[ "$DEL" != "True" ]]; then
    die "rm $name" "expected deleted=true: $JSON"
  fi
  ok "rm $name"
done

JSON="$(run_json list)" || die "list empty" "$JSON"
COUNT="$(jget "$JSON" 'obj.get("count")')"
if [[ "$COUNT" != "0" ]]; then
  die "list empty" "expected 0 timers after rm, got $COUNT: $JSON"
fi
ok "list empty after rm"

echo
echo "OK  $pass assertions passed (db=$BELLMAN_DB)"
exit 0
