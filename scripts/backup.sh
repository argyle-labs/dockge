#!/usr/bin/env bash
# Archive Dockge's app data + managed-stacks directories. Mirrors
# `dockge.backup`.
#
#   ./scripts/backup.sh OUT.tar.gz [DATA_PATH] [STACKS_PATH]
set -euo pipefail

OUT="${1:?usage: backup.sh OUT.tar.gz [DATA_PATH] [STACKS_PATH]}"
DATA_PATH="${2:-/opt/dockge/data}"
STACKS_PATH="${3:-/opt/stacks}"

tar czf "${OUT}" "${DATA_PATH}" "${STACKS_PATH}"
echo "Wrote ${OUT}"
