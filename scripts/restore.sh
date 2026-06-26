#!/usr/bin/env bash
# Restore a Dockge backup archive over the app-data + managed-stacks paths,
# then bring the stack back up.
#
#   ./scripts/restore.sh ARCHIVE.tar.gz [PROJECT_DIR]
set -euo pipefail

ARCHIVE="${1:?usage: restore.sh ARCHIVE.tar.gz [PROJECT_DIR]}"
PROJECT_DIR="${2:-.}"

# tar was created with absolute paths; extract from filesystem root.
tar xzf "${ARCHIVE}" -C /
docker compose --project-directory "${PROJECT_DIR}" up -d
echo "Restored from ${ARCHIVE}."
