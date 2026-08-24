#!/usr/bin/env bash
# Mirror the local experiments store into the durable benchmark database.
#
# The database host is reached over SSM only: the SQL travels as gzip+base64
# through RunShellScript chunks, so no database port opens and no credentials
# leave the host. The flow is emit -> ship -> apply -> verify -> mark:
# arenabench.mirror emits idempotent SQL, the host applies it inside the
# Postgres container, every emitted content hash is confirmed present, and
# only then are the local rows stamped as migrated. A failure anywhere stops
# before the stamp, so a failed push can never read as a finished one.
set -euo pipefail

INSTANCE_ID="${MIRROR_INSTANCE_ID:-i-023d002d6e44f8f84}"
CONTAINER="${MIRROR_PG_CONTAINER:-oxagen-data-postgres-1}"
PG_USER="${MIRROR_PG_USER:-oxagen}"
PG_DB="${MIRROR_PG_DB:-benchmarks}"
SOURCE="${MIRROR_SOURCE:-$(hostname -s)@local}"
# Base64 is chunked to stay under the SSM command-size limit.
CHUNK_BYTES=60000

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

remote() {
    # Run one command on the host and print its stdout; fail on its failure.
    # The command travels as a JSON parameters file — json.dumps, not shell
    # interpolation, is what keeps quotes inside SQL intact on the wire.
    local command_id
    printf '%s' "$1" | python3 -c \
        'import json, sys; print(json.dumps({"commands": [sys.stdin.read()]}))' \
        > "$WORK/params.json"
    command_id="$(aws ssm send-command \
        --instance-ids "$INSTANCE_ID" \
        --document-name AWS-RunShellScript \
        --parameters "file://$WORK/params.json" \
        --query 'Command.CommandId' --output text)"
    aws ssm wait command-executed \
        --command-id "$command_id" --instance-id "$INSTANCE_ID" || true
    local status
    status="$(aws ssm get-command-invocation \
        --command-id "$command_id" --instance-id "$INSTANCE_ID" \
        --query 'Status' --output text)"
    aws ssm get-command-invocation \
        --command-id "$command_id" --instance-id "$INSTANCE_ID" \
        --query 'StandardOutputContent' --output text
    if [ "$status" != "Success" ]; then
        aws ssm get-command-invocation \
            --command-id "$command_id" --instance-id "$INSTANCE_ID" \
            --query 'StandardErrorContent' --output text >&2
        echo "remote command failed ($status): $1" >&2
        return 1
    fi
}

echo "== emit"
uv run --project "$ROOT" python -m arenabench.mirror emit \
    --source "$SOURCE" --out "$WORK/mirror.sql"
row_count="$(grep -c '^INSERT INTO experiment_results' "$WORK/mirror.sql" || true)"
if [ "$row_count" -eq 0 ]; then
    echo "nothing to mirror"
    exit 0
fi

echo "== ship ($row_count rows)"
gzip -9 -c "$WORK/mirror.sql" | base64 | tr -d '\n' > "$WORK/payload.b64"
split -b "$CHUNK_BYTES" "$WORK/payload.b64" "$WORK/chunk."
remote "rm -f /tmp/bench-mirror.b64" > /dev/null
for chunk in "$WORK"/chunk.*; do
    remote "printf %s '$(cat "$chunk")' >> /tmp/bench-mirror.b64" > /dev/null
done

echo "== apply"
remote "base64 -d /tmp/bench-mirror.b64 | gunzip > /tmp/bench-mirror.sql \
&& docker exec $CONTAINER psql -U $PG_USER -tAc \
\"SELECT 1 FROM pg_database WHERE datname = '$PG_DB'\" | grep -q 1 \
|| docker exec $CONTAINER psql -U $PG_USER -c 'CREATE DATABASE $PG_DB'" > /dev/null
remote "docker exec -i $CONTAINER psql -U $PG_USER -d $PG_DB \
--set ON_ERROR_STOP=1 -q -f - < /tmp/bench-mirror.sql \
&& rm -f /tmp/bench-mirror.b64 /tmp/bench-mirror.sql" > /dev/null

echo "== verify"
sha_list="$(grep -oE "'[0-9a-f]{64}'" "$WORK/mirror.sql" | paste -sd, -)"
landed="$(remote "docker exec $CONTAINER psql -U $PG_USER -d $PG_DB -tAc \
\"SELECT COUNT(*) FROM experiment_results WHERE doc_sha256 IN ($sha_list)\"" \
    | tr -d '[:space:]')"
if [ "$landed" != "$row_count" ]; then
    echo "verification failed: emitted $row_count rows, found $landed" >&2
    echo "local rows were NOT marked migrated" >&2
    exit 1
fi

echo "== mark"
uv run --project "$ROOT" python -m arenabench.mirror mark --source "$SOURCE"
echo "mirrored $row_count rows to $PG_DB on $INSTANCE_ID as $SOURCE"
