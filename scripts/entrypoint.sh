#!/usr/bin/env sh
# Thin entrypoint wrapper. Dockge's upstream image already defines its own
# start command; we exec it so this wrapper is transparent. Kept as a seam for
# future orca-managed pre-start checks (e.g. asserting the stacks dir is the
# same absolute path inside + outside the container).
set -e

: "${DOCKGE_STACKS_DIR:=/opt/stacks}"
export DOCKGE_STACKS_DIR

# Hand off to Dockge's own entrypoint/command.
exec node ./frontend-dist/server/server.js "$@"
