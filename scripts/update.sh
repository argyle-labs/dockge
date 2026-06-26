#!/usr/bin/env bash
# Pull the newest Dockge image and recreate the container in place. Mirrors
# `dockge.update`.
#
#   ./scripts/update.sh [PROJECT_DIR]
set -euo pipefail

PROJECT_DIR="${1:-.}"

docker compose --project-directory "${PROJECT_DIR}" pull
docker compose --project-directory "${PROJECT_DIR}" up -d
echo "Dockge updated."
