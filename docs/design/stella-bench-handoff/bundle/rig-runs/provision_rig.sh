#!/bin/bash
# Provision the standing Stella-vs-Claude-Code rig.
#
# Faithful clone of the live benchmark host, with one deliberate difference:
# sized for parallelism (32 vCPU) so a head-to-head is an iteration loop
# rather than an overnight job. The SUT binary is copied in, not rebuilt --
# a rebuild could produce a different binary and silently change what is
# being measured.
set -uo pipefail
exec > >(tee -a "$HOME/provision.log") 2>&1
echo "=== provision start $(date -u +%FT%TZ) ==="

SUT_SHA="18599fea0c3b2d13c3a185189318c9eaba2f9057"

# --- 1. instance NVMe for docker (60GB+ of task images land here) ----------
if ! mountpoint -q /var/lib/docker; then
  sudo mkfs.ext4 -F -q /dev/nvme1n1
  sudo mkdir -p /var/lib/docker
  sudo mount /dev/nvme1n1 /var/lib/docker
  echo '/dev/nvme1n1 /var/lib/docker ext4 defaults,nofail 0 2' | sudo tee -a /etc/fstab >/dev/null
fi
echo "nvme mounted: $(df -h /var/lib/docker | tail -1)"

# --- 2. docker -------------------------------------------------------------
if ! command -v docker >/dev/null; then
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ca-certificates curl gnupg git jq >/dev/null
  sudo install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg | sudo gpg --dearmor -o /etc/apt/keyrings/docker.gpg
  sudo chmod a+r /etc/apt/keyrings/docker.gpg
  echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo $VERSION_CODENAME) stable" \
    | sudo tee /etc/apt/sources.list.d/docker.list >/dev/null
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin >/dev/null
  sudo usermod -aG docker ubuntu
fi
# 32 vCPU box running tens of concurrent trials: raise daemon file limits
sudo mkdir -p /etc/docker
echo '{"log-driver":"json-file","log-opts":{"max-size":"10m","max-file":"3"},"max-concurrent-downloads":12}' \
  | sudo tee /etc/docker/daemon.json >/dev/null
sudo systemctl restart docker
sudo docker --version

# --- 3. uv + repo at the exact SUT commit ---------------------------------
command -v uv >/dev/null || curl -LsSf https://astral.sh/uv/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
if [ ! -d "$HOME/stella" ]; then
  git clone -q https://github.com/macanderson/stella.git "$HOME/stella"
fi
cd "$HOME/stella"
git fetch -q origin
git checkout -q "$SUT_SHA"
echo "repo HEAD: $(git rev-parse HEAD)"

# --- 4. harbor 0.6.1, pinned (version mismatch refuses to launch) ---------
cd "$HOME/stella/bench/harbor_adapter"
[ -d .venv ] || uv venv -q .venv
VENV_PY="$HOME/stella/bench/harbor_adapter/.venv/bin/python"
uv pip install -q --python "$VENV_PY" -r requirements.txt 2>/dev/null || \
  uv pip install -q --python "$VENV_PY" "harbor==0.6.1"
"$HOME/stella/bench/harbor_adapter/.venv/bin/harbor" --version

mkdir -p "$HOME/tb21"
echo "$SUT_SHA" > "$HOME/tb21/sut_commit.txt"
echo "=== provision phase 1 done $(date -u +%FT%TZ) ==="
