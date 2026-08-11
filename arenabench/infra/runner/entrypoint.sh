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

discard_dead_docker_chains() {
    # Only ever reached while THIS trial holds the instance lock, so every
    # DOCKER* chain in the shared namespace is residue from a trial that has
    # already exited: `cleanup_dockerd` kills the daemon, and the daemon's
    # iptables rules are not its to take with it. dockerd rebuilds what it
    # needs at startup, so removing them restores the namespace to the state a
    # first-on-this-instance trial finds.
    for table in filter nat; do
        iptables -w -t "$table" -S 2>/dev/null \
            | awk '/^-N DOCKER/ { print $2 }' \
            | while read -r chain; do
                iptables -w -t "$table" -F "$chain" 2>/dev/null || true
                iptables -w -t "$table" -X "$chain" 2>/dev/null || true
            done
    done
}

claim_instance_netns() {
    # Batch runs EC2 jobs with ECS `networkMode: host`, so every trial placed
    # on this instance shares one network namespace — and Docker's chains are
    # named globally within a namespace. A second *concurrent* dockerd dies on
    # `DOCKER-BRIDGE: Chain already exists` after a 60-second wait, reported as
    # fifty lines of daemon log naming neither the neighbour nor the placement.
    #
    # This used to refuse whenever those chains existed, reasoning that the job
    # definition sizes a trial to the whole instance, so only a bypass could
    # produce them. That reasoning holds across SPACE and fails across TIME:
    # Batch reuses a warm instance for the next trial, and the previous trial's
    # chains are still there. On 2026-08-11 that took out 157 of 178 trials in
    # one panel — the 16 placed first, each on a fresh instance, ran; every
    # trial placed on a reused instance died in about five seconds. The guard
    # was reading a dead neighbour's residue as a live neighbour.
    #
    # An exclusive lock on the instance-wide scratch volume answers the
    # question the chains only hinted at, because the kernel drops it when the
    # holder dies: held means a trial is running here NOW and the original
    # refusal stands; free means whatever left those chains is gone.
    if [ ! -d /scratch ]; then
        # No shared volume, so no liveness signal — keep the conservative
        # refusal rather than flushing chains that might belong to somebody.
        if iptables -w -t filter -n -L DOCKER-BRIDGE >/dev/null 2>&1; then
            log "FATAL: another dockerd already owns this network namespace,"
            log "  and without /scratch there is no way to tell a live trial"
            log "  from a finished one's leftovers. Mount the scratch volume"
            log "  (see TrialTopology in infra/core.yaml), or give each dockerd"
            log "  its own netns."
            return 1
        fi
        return 0
    fi
    # fd 9 stays open for the life of this shell: the lock belongs to the
    # trial, not to this function, and releasing it early would admit a
    # neighbour while our dockerd is still running.
    exec 9>>/scratch/.instance-netns.lock
    if ! flock -n 9; then
        log "FATAL: another trial is running on this instance right now."
        log "  Batch uses host networking, so two concurrent trials share one"
        log "  network namespace and only the first dockerd can start. Give"
        log "  this trial the whole instance (4 vCPU on m6i.xlarge), or give"
        log "  each dockerd its own netns. See TrialTopology in infra/core.yaml."
        return 1
    fi
    if iptables -w -t filter -n -L DOCKER-BRIDGE >/dev/null 2>&1; then
        log "reclaiming the network namespace from a finished trial"
        discard_dead_docker_chains
    fi
}

prepare_cgroup2() {
    # cgroup v2 enforces "no internal processes": a cgroup may hold processes
    # or delegate controllers to children, never both. Batch runs this
    # container in the host's cgroup namespace, so the inner dockerd creates
    # /sys/fs/cgroup/docker under a root that already holds our own
    # processes, and every task container dies at start with
    #   cannot enter cgroupv2 "/sys/fs/cgroup/docker" with domain
    #   controllers -- it is in an invalid state
    # which surfaces as a Harbor setup traceback while the job still exits 0.
    #
    # The remedy is the one the official docker:dind entrypoint uses: move
    # this container's processes into a leaf cgroup so the root holds none,
    # then delegate the available controllers downward so dockerd may place
    # children. Both steps are best-effort — on a host already giving us a
    # private cgroup namespace there is nothing to fix, and failing here
    # would be worse than letting dockerd try.
    [ -f /sys/fs/cgroup/cgroup.controllers ] || return 0
    mkdir -p /sys/fs/cgroup/init 2>/dev/null || return 0
    xargs -rn1 </sys/fs/cgroup/cgroup.procs >/sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
    sed -e 's/ / +/g' -e 's/^/+/' </sys/fs/cgroup/cgroup.controllers \
        >/sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
    log "cgroup v2 prepared for nested containers"
}

start_dockerd() {
    claim_instance_netns || return 1
    prepare_cgroup2
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
    # refuses a seat whose declared credentials are absent, and that refusal
    # names the seat and the variable, which is the better error surface.
    #
    # Why the credentials could not be read is reported, though. Discarding
    # this stderr once turned a one-line IAM gap — the role could list the
    # SecureStrings but had no kms:Decrypt to read them, so
    # `--with-decryption` failed wholesale — into "no SSM credentials
    # readable; continuing", and the AccessDeniedException underneath it
    # reached nobody. A whole 40-trial match died of it twice.
    local pairs problem
    if ! pairs=$(aws ssm get-parameters-by-path \
        --path /arenabench --with-decryption \
        --query 'Parameters[].[Name,Value]' --output text 2>/tmp/ssm-error); then
        problem=$(tr '\n' ' ' </tmp/ssm-error 2>/dev/null)
        rm -f /tmp/ssm-error
        log "no SSM credentials readable: ${problem:0:400}"
        return 0
    fi
    rm -f /tmp/ssm-error
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
