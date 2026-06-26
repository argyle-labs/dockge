#!/usr/bin/env bash
# Register a running Dockge endpoint with orca so the `dockge.*` tools can
# reach it. Wraps `orca dockge create`.
#
#   ./scripts/configure.sh NAME BASE_URL TOKEN
#
# Example:
#   ./scripts/configure.sh home http://<host>.local:5001 "$DOCKGE_TOKEN"
set -euo pipefail

NAME="${1:?usage: configure.sh NAME BASE_URL TOKEN}"
BASE_URL="${2:?missing BASE_URL}"
TOKEN="${3:?missing TOKEN}"

orca dockge create \
  --name "${NAME}" \
  --base-url "${BASE_URL}" \
  --token "${TOKEN}" \
  --enabled true

echo "Registered Dockge endpoint '${NAME}'."
