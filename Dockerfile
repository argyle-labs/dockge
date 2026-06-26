# Dockge ships as a prebuilt image; this thin wrapper pins a version via build
# arg and layers in the repo's entrypoint so the orca lifecycle tools and the
# bare container share one start path. Override DOCKGE_VERSION to pin a release.
ARG DOCKGE_VERSION=latest
FROM louislam/dockge:${DOCKGE_VERSION}

# Dockge persists its app state here and manages compose stacks here.
VOLUME ["/app/data", "/opt/stacks"]

# Dockge's web UI.
EXPOSE 5001

COPY scripts/entrypoint.sh /usr/local/bin/orca-dockge-entrypoint.sh
RUN chmod +x /usr/local/bin/orca-dockge-entrypoint.sh

ENTRYPOINT ["/usr/local/bin/orca-dockge-entrypoint.sh"]
