#!/bin/bash
# Fetch the pinned dataset and classify tasks by the resources they request, so
# a task that wants more memory than the VM can share never runs beside another.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

harbor download "$TB_DATASET" --export -o "$DATASET_DIR"

python3 - "$DATASET_DIR" "$TB_ROOT" <<'PY'
import glob, json, re, sys
dataset_dir, tb_root = sys.argv[1], sys.argv[2]
a, b, images = [], [], set()
for path in sorted(glob.glob(f"{dataset_dir}/*/*/task.toml")):
    text = open(path).read()
    name = path.split("/")[-2]
    mem = int(re.search(r"memory_mb\s*=\s*(\d+)", text).group(1))
    cpus = int(re.search(r"cpus\s*=\s*(\d+)", text).group(1))
    img = re.search(r'docker_image\s*=\s*"([^"]+)"', text)
    if img:
        images.add(img.group(1))
    (b if (mem > 4096 or cpus > 1) else a).append(name)
json.dump({"phaseA": a, "phaseB": b}, open(f"{tb_root}/phases.json", "w"), indent=1)
open(f"{tb_root}/images.txt", "w").write("\n".join(sorted(images)) + "\n")
print(f"phaseA={len(a)} phaseB={len(b)} total={len(a)+len(b)} images={len(images)}")
PY
