#!/usr/bin/env bash
# ArenaBench Batch runner entrypoint.
#
# Modes (first argument, default "smoke"):
#   smoke  — prove the trial substrate works end to end: dockerd boots,
#            a container runs, S3 and DynamoDB are reachable. Cheap enough
#            to run after every infra change.
#   run    — execute one match: fetch MATCH_S3_URI (an arenabench.toml),
#            optionally stage a prebuilt SUT binary from SUT_S3_URI in the
#            layout sut_build.py produces, run `arenabench run`, then sync
#            all artifacts (events, results, recordings) to S3 and record
#            the outcome in DynamoDB.
#
# Required environment (from the Batch job definition / submit-time overrides):
#   ARTIFACTS_BUCKET, ARENABENCH_TABLE, AWS_DEFAULT_REGION   (job definition)
#   run mode: RUN_ID, MATCH_S3_URI; optionally SUT_S3_URI + SUT_COMMIT.
#
# Provider credentials are read from SSM Parameter Store: every parameter
# under /arenabench/ is exported, upper-cased, into the environment
# (e.g. /arenabench/anthropic_api_key -> ANTHROPIC_API_KEY).
set -euo pipefail

MODE="${1:-smoke}"
JOB_ID="${AWS_BATCH_JOB_ID:-local-$$}"

log() { printf '[runner] %s\n' "$*"; }

cleanup_dockerd() {
    kill "${DOCKERD_PID:-0}" 2>/dev/null || true
    wait "${DOCKERD_PID:-0}" 2>/dev/null || true
    [ -n "${JOB_SLUG:-}" ] && rm -rf "/scratch/$JOB_SLUG" 2>/dev/null || true
}

assert_sole_dockerd() {
    # Batch runs EC2 jobs with ECS `networkMode: host`, so a second trial
    # placed on this instance would share our network namespace — and
    # Docker's filter chains are named globally within a namespace. The
    # second dockerd then dies on `DOCKER-BRIDGE: Chain already exists`
    # after a 60-second wait, reported as fifty lines of daemon log that
    # name neither the neighbour nor the placement that created it.
    #
    # The job definition and both compute environments size a trial to the
    # whole instance so this cannot happen; this is the backstop for the
    # ways that guarantee can be bypassed anyway — a submit-time
    # `--vcpus` override, a hand-written job definition, a compute
    # environment edited to allow a larger instance type. Refusing here
    # costs one iptables read and turns a mystifying crash into its cause.
    if iptables -w -t filter -n -L DOCKER-BRIDGE >/dev/null 2>&1; then
        log "FATAL: another dockerd already owns this network namespace."
        log "  Batch uses host networking, so two trials on one instance"
        log "  share it and only the first dockerd can start. Give this"
        log "  trial the whole instance (4 vCPU on m6i.xlarge) or give each"
        log "  dockerd its own netns. See TrialTopology in infra/core.yaml."
        return 1
    fi
}

start_dockerd() {
    assert_sole_dockerd || return 1
    # Overlayfs cannot nest: with /var/lib/docker on the container's own
    # overlay root, the inner dockerd's mounts fail with EINVAL. The job
    # definition mounts a host-path volume at /scratch; a per-job
    # subdirectory there gives this trial storage on a real filesystem,
    # cleaned up on exit so a reused instance starts clean.
    local data_root=/var/lib/docker
    if [ -d /scratch ]; then
        JOB_SLUG=$(printf '%s' "$JOB_ID" | tr -c 'A-Za-z0-9_-' '_')
        data_root="/scratch/$JOB_SLUG/docker"
        mkdir -p "$data_root"
        trap 'cleanup_dockerd' EXIT
    fi
    log "starting dockerd (data-root $data_root)"
    dockerd --data-root "$data_root" >/var/log/dockerd.log 2>&1 &
    DOCKERD_PID=$!
    for _ in $(seq 1 60); do
        if docker info >/dev/null 2>&1; then
            log "dockerd is up"
            return 0
        fi
        sleep 1
    done
    log "dockerd failed to start; tail of log:"
    tail -50 /var/log/dockerd.log || true
    return 1
}

export_ssm_credentials() {
    # Missing credentials are not fatal here: `arenabench run` itself
    # refuses a seat whose declared credentials are absent, which is the
    # better error surface.
    local pairs
    if ! pairs=$(aws ssm get-parameters-by-path \
        --path /arenabench --with-decryption \
        --query 'Parameters[].[Name,Value]' --output text 2>/dev/null); then
        log "no SSM credentials readable; continuing"
        return 0
    fi
    while IFS=$'\t' read -r name value; do
        [ -n "$name" ] || continue
        local key
        key=$(basename "$name" | tr '[:lower:]' '[:upper:]' | tr -c 'A-Z0-9_\n' '_')
        export "$key=$value"
        log "exported credential $key"
    done <<<"$pairs"
}

ddb_put_status() {
    local status="$1" detail="$2"
    aws dynamodb put-item --table-name "$ARENABENCH_TABLE" --item "{
        \"PK\": {\"S\": \"RUN#${RUN_ID:-smoke}\"},
        \"SK\": {\"S\": \"JOB#${JOB_ID}\"},
        \"GSI1PK\": {\"S\": \"JOB\"},
        \"GSI1SK\": {\"S\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"},
        \"status\": {\"S\": \"${status}\"},
        \"detail\": {\"S\": \"${detail}\"},
        \"mode\": {\"S\": \"${MODE}\"}
    }" >/dev/null 2>&1 || log "dynamodb status write failed (non-fatal)"
}

stage_sut() {
    # Reconstruct the finished-build layout sut_build.py stages, so
    # arenabench's binary_for() accepts the prebuilt binary as done.
    : "${SUT_COMMIT:?SUT_S3_URI requires SUT_COMMIT}"
    local dest="$HOME/.arenabench/sut/$SUT_COMMIT"
    mkdir -p "$dest"
    aws s3 cp "$SUT_S3_URI" "$dest/stella"
    chmod 0755 "$dest/stella"
    printf '%s\n' "$SUT_COMMIT" >"$dest/sut_commit.txt"
    sha256sum "$dest/stella" | cut -d' ' -f1 >"$dest/binary_sha256.txt"
    log "staged SUT $SUT_COMMIT ($(cut -c1-12 "$dest/binary_sha256.txt"))"
}

case "$MODE" in
smoke)
    start_dockerd
    docker run --rm public.ecr.aws/docker/library/hello-world:latest
    log "inner container ran"
    printf '{"job":"%s","at":"%s"}\n' "$JOB_ID" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >/tmp/heartbeat.json
    aws s3 cp /tmp/heartbeat.json "s3://$ARTIFACTS_BUCKET/runs/_smoke/$JOB_ID.json"
    log "s3 write ok"
    RUN_ID="smoke" ddb_put_status "done" "smoke passed"
    log "SMOKE OK"
    ;;
run)
    : "${RUN_ID:?}" "${MATCH_S3_URI:?}"
    start_dockerd
    export_ssm_credentials
    [ -n "${SUT_S3_URI:-}" ] && stage_sut
    ddb_put_status "running" "$MATCH_S3_URI"

    mkdir -p /work/ws
    aws s3 cp "$MATCH_S3_URI" /work/match.toml
    rc=0
    arenabench run /work/match.toml \
        --workspace /work/ws \
        --results /work/results.json \
        --progress || rc=$?

    log "arenabench run exited $rc; syncing artifacts"
    aws s3 sync /work/ws "s3://$ARTIFACTS_BUCKET/runs/$RUN_ID/$JOB_ID/ws" --only-show-errors || true
    [ -f /work/results.json ] &&
        aws s3 cp /work/results.json "s3://$ARTIFACTS_BUCKET/runs/$RUN_ID/$JOB_ID/results.json"
    [ -d "$HOME/.arenabench/matches" ] &&
        aws s3 sync "$HOME/.arenabench/matches" "s3://$ARTIFACTS_BUCKET/runs/$RUN_ID/$JOB_ID/matches" --only-show-errors || true

    if [ "$rc" -eq 0 ]; then
        ddb_put_status "done" "exit 0"
    else
        ddb_put_status "failed" "exit $rc"
    fi
    exit "$rc"
    ;;
*)
    log "unknown mode: $MODE (expected smoke|run)"
    exit 2
    ;;
esac
