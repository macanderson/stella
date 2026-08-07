# ArenaBench trial runner.
#
# One container = one trial slot on AWS Batch: it boots its own dockerd
# (privileged docker-in-docker, so harbor task containers are fully isolated
# per trial), stages a prebuilt SUT binary the way sut_build.py would, and
# runs `arenabench run` against a match TOML fetched from S3.
#
# Build context is the repository root. PKG_PATH locates the ArenaBench
# package inside it: "arenabench" while ArenaBench lives in the stella
# monorepo, "." once it is ejected into its own repository.
ARG PKG_PATH=arenabench

# ECR Public mirror of the official image: Docker Hub rate-limits anonymous
# pulls from CodeBuild's shared egress IPs (429s), ECR Public does not.
FROM public.ecr.aws/docker/library/ubuntu:24.04
ARG PKG_PATH
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git jq unzip gnupg procps \
        python3 python3-pip ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# Docker engine from the official repository — the trial hosts its own
# dockerd, so harbor's `docker compose` task stacks work unmodified.
RUN install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc \
    && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu noble stable" \
        > /etc/apt/sources.list.d/docker.list \
    && apt-get update && apt-get install -y --no-install-recommends \
        docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

# Node 22 + Claude Code: the required day-1 comparison agent.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g @anthropic-ai/claude-code \
    && rm -rf /var/lib/apt/lists/*

# AWS CLI v2 for S3 artifact sync, SSM credential fetch, DynamoDB status.
RUN curl -fsSL https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip -o /tmp/awscliv2.zip \
    && unzip -q /tmp/awscliv2.zip -d /tmp \
    && /tmp/aws/install \
    && rm -rf /tmp/aws /tmp/awscliv2.zip

# ArenaBench itself. Stdlib-only by contract, so this install is dependency
# free; the pre-built web client ships inside the package.
COPY ${PKG_PATH}/pyproject.toml /opt/arenabench/pyproject.toml
COPY ${PKG_PATH}/README.md /opt/arenabench/README.md
COPY ${PKG_PATH}/LICENSE /opt/arenabench/LICENSE
COPY ${PKG_PATH}/arenabench /opt/arenabench/arenabench
RUN python3 -m pip install --break-system-packages /opt/arenabench

COPY ${PKG_PATH}/infra/runner/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["smoke"]
