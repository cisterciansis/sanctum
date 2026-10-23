#!/usr/bin/env bash
# Dead-drop experiment against Hippius S3 (SPEC-V2-DEADDROP.md §4).
# Decides: can a sender WRITE into a bucket it cannot READ or LIST?
# Usage: ./test-deaddrop.sh          (reads hippius.env next to this script)
set -u

DIR="$(cd "$(dirname "$0")" && pwd)"
source "$DIR/hippius.env"

EP="https://s3.hippius.com"
export AWS_DEFAULT_REGION=decentralized
export AWS_EC2_METADATA_DISABLED=true

# Path-style addressing, per Hippius docs.
aws configure set default.s3.addressing_style path 2>/dev/null

owner()  { AWS_ACCESS_KEY_ID=$OWNER_KEY  AWS_SECRET_ACCESS_KEY=$OWNER_SECRET  aws --endpoint-url "$EP" "$@"; }
sender() { AWS_ACCESS_KEY_ID=${SENDER_KEY:-} AWS_SECRET_ACCESS_KEY=${SENDER_SECRET:-} aws --endpoint-url "$EP" "$@"; }

PASS=0; FAIL=0
check() { # check <expect:0|nonzero> <label> <actual_rc>
  local exp=$1 label=$2 rc=$3
  if { [ "$exp" = 0 ] && [ "$rc" -eq 0 ]; } || { [ "$exp" != 0 ] && [ "$rc" -ne 0 ]; }; then
    echo "PASS  $label"; PASS=$((PASS+1))
  else
    echo "FAIL  $label (rc=$rc, expected ${exp/0/success}${exp/#[1-9]*/failure})"; FAIL=$((FAIL+1))
  fi
}

STATE="$DIR/.state"
if [ -f "$STATE" ]; then source "$STATE"; fi

# ---- Phase 1: owner-only setup (run with just OWNER_* filled) ----
if [ -z "${ANCHOR:-}" ]; then
  SUF=$(LC_ALL=C tr -dc a-z0-9 </dev/urandom | head -c 10)
  ANCHOR="anchor-$SUF"; DROP="drop-$SUF"
  echo "Creating buckets: $ANCHOR (sub-token scope) and $DROP (dead drop)..."
  owner s3api create-bucket --bucket "$ANCHOR"; check 0 "create anchor bucket" $?
  owner s3api create-bucket --bucket "$DROP";   check 0 "create drop bucket"   $?
  printf 'ANCHOR=%s\nDROP=%s\n' "$ANCHOR" "$DROP" > "$STATE"
  echo "hello" > /tmp/dd-hello.txt
  owner s3api put-object --bucket "$DROP" --key sanity.txt --body /tmp/dd-hello.txt >/dev/null
  check 0 "owner sanity PUT" $?
  owner s3api get-object --bucket "$DROP" --key sanity.txt /tmp/dd-hello-back.txt >/dev/null
  check 0 "owner sanity GET" $?
fi

if [ -z "${SENDER_KEY:-}" ] || [ "${SENDER_KEY}" = "hip_..." ]; then
  cat <<EOF

== Phase 1 done. Next manual step ==
In the Hippius console/app: create a SUB TOKEN with permission "Object Read & Write",
scoped ONLY to bucket: $ANCHOR
Fill SENDER_KEY / SENDER_SECRET in hippius.env, then re-run this script.
EOF
  exit 0
fi

# ---- Phase 2: the actual experiment ----
source "$STATE"
echo "== Using drop bucket: $DROP =="
echo "msg-$(date +%s)" > /tmp/dd-msg.bin

echo "-- Baseline: sender has no grant yet --"
sender s3api put-object --bucket "$DROP" --key pre-grant.bin --body /tmp/dd-msg.bin >/dev/null 2>&1
check 1 "sender PUT before grant is denied" $?

echo "-- Grant WRITE (not READ) to sender key --"
OWNER_ID=$(owner s3api get-bucket-acl --bucket "$DROP" --query 'Owner.ID' --output text)
echo "  owner canonical id: $OWNER_ID"
# Hippius grant-header syntax (hippius_s3/api/s3/acl_endpoints.py): accessKey="hip_..."
owner s3api put-bucket-acl --bucket "$DROP" --grant-write "accessKey=\"$SENDER_KEY\"" \
  --grant-full-control "id=\"$OWNER_ID\"" 2>/tmp/dd-acl-err.txt
rc=$?
[ $rc -ne 0 ] && cat /tmp/dd-acl-err.txt
check 0 "owner grants WRITE to sender" $rc

echo "-- Core claims --"
sender s3api put-object --bucket "$DROP" --key msg-001.bin --body /tmp/dd-msg.bin >/dev/null 2>&1
check 0 "sender PUT after grant succeeds" $?
sender s3api get-object --bucket "$DROP" --key msg-001.bin /tmp/dd-stolen.bin >/dev/null 2>&1
check 1 "sender GET is denied (WRITE !=> READ)" $?
sender s3api list-objects-v2 --bucket "$DROP" >/dev/null 2>&1
check 1 "sender LIST is denied" $?
sender s3api get-object --bucket "$DROP" --key sanity.txt /tmp/dd-stolen2.bin >/dev/null 2>&1
check 1 "sender GET of owner's object is denied" $?
sender s3api put-bucket-acl --bucket "$DROP" --grant-full-control "id=$SENDER_KEY" >/dev/null 2>&1
check 1 "sender ACL escalation is denied" $?
owner s3api get-object --bucket "$DROP" --key msg-001.bin /tmp/dd-recv.bin >/dev/null 2>&1
check 0 "owner GET of sender's message succeeds" $?

echo "-- Anonymous PUT (the onboarding-wart question) --"
owner s3api put-bucket-acl --bucket "$DROP" \
  --grant-write 'uri="http://acs.amazonaws.com/groups/global/AllUsers"' \
  --grant-write "accessKey=\"$SENDER_KEY\"" \
  --grant-full-control "id=\"$OWNER_ID\"" >/dev/null 2>&1
if [ $? -eq 0 ]; then
  http=$(curl -s -o /tmp/dd-anon-resp.txt -w '%{http_code}' -X PUT \
    --data-binary @/tmp/dd-msg.bin "$EP/$DROP/anon-001.bin")
  if [ "$http" = 200 ]; then echo "PASS  anonymous PUT works (HTTP 200) — senders need NO account"; PASS=$((PASS+1))
  else echo "INFO  anonymous PUT returned HTTP $http — fall back to credential broker"; fi
else
  echo "INFO  AllUsers WRITE grant rejected — fall back to credential broker"
fi

echo "-- Latency: PUT -> owner-visible (10 rounds) --"
for i in $(seq 1 10); do
  t0=$(python3 -c 'import time;print(time.time())')
  sender s3api put-object --bucket "$DROP" --key "lat-$i.bin" --body /tmp/dd-msg.bin >/dev/null 2>&1
  tries=0
  until owner s3api head-object --bucket "$DROP" --key "lat-$i.bin" >/dev/null 2>&1; do
    sleep 0.2; tries=$((tries+1)); [ $tries -gt 150 ] && break
  done
  t1=$(python3 -c 'import time;print(time.time())')
  [ $tries -gt 150 ] && echo "  round $i: TIMEOUT (>30s)" || echo "  round $i: $(python3 -c "print(f'{$t1-$t0:.2f}s')")"
done

echo
echo "== RESULT: $PASS passed, $FAIL failed =="
[ $FAIL -eq 0 ] && echo "Dead-drop design CONFIRMED on Hippius." || echo "See failures above — design gate not met."
