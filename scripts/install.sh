#!/usr/bin/env bash
# Bring up the Dockge web app via Docker Compose. This is the curl-bootstrap
# payload that `dockge.install` orchestrates; run it directly for a manual
# deploy.
#
#   ./scripts/install.sh [PROJECT_DIR]
#
# PROJECT_DIR defaults to the repo root (where compose.yml lives).
set -euo pipefail

PROJECT_DIR="${1:-.}"

mkdir -p "${PROJECT_DIR}/data" /opt/stacks
docker compose --project-directory "${PROJECT_DIR}" up -d
echo "Dockge is starting; open http://<host>.local:5001"
