# Dockge — operator notes

[Dockge](https://github.com/louislam/dockge) is a self-hosted manager for
Docker Compose stacks with a clean web UI. These notes cover running and
operating a Dockge instance. For how this plugin drives Dockge through orca,
see the [README](../README.md).

Dockge serves its UI on port **5001** and manages compose stacks stored under a
stacks directory (default `/opt/stacks`). Every instance uses the same image
(`louislam/dockge`); there is no separate agent binary. One instance can be
designated the primary and others added as agents from the primary's UI, giving
a single pane over multiple hosts — but a single standalone instance is the
common case.

## Run it

```sh
docker compose up -d
```

The shipped [`compose.yml`](../compose.yml) pins the image, the port (**5001**),
and the two persistent paths — app data (`/app/data`) and the managed-stack
directory (`/opt/stacks`). On first visit to `http://<host>:5001` you create the
admin account.

Other runtimes work the same way — run the same image via Podman
(`podman compose -f compose.yml up -d`), on a VM/LXC guest, or as an Unraid
container using the image/ports/volumes from `compose.yml`. Upstream install
docs: <https://github.com/louislam/dockge>.

## Configuration

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DOCKGE_STACKS_DIR` | `/opt/stacks` | Directory where compose stacks are stored |
| `TZ` | (unset) | Timezone, e.g. `Etc/UTC` |

### Volumes

| Host Path | Container Path | Purpose |
|-----------|----------------|---------|
| `/var/run/docker.sock` | `/var/run/docker.sock` | Docker engine access |
| `<data-dir>` | `/app/data` | Dockge database and config |
| `<stacks-dir>` | `/opt/stacks` | Compose stack directory |

If your compose files live outside the stacks directory and are referenced by
symlink, the symlink **target** must also be mounted into the container —
otherwise Dockge cannot read the compose files. Mount the source tree read-only
and point the symlinks at it:

```bash
for svc in service-a service-b service-c; do
  ln -s /srv/compose/$svc /opt/stacks/$svc
done
```

```yaml
volumes:
  - /srv/compose:/srv/compose:ro
```

Restart Dockge after adding a mount:

```bash
docker compose down && docker compose up -d
```

## Multi-host (optional)

To manage several hosts from one UI:

1. Deploy Dockge on each host (same image/compose), creating an admin account on
   each on first visit.
2. From the primary instance, click **Add Agent** and enter the other host's
   URL (`http://<host>:5001`) plus the username/password created on that host.

All stacks across connected agents then appear in the primary's unified view.
Agents connect over Socket.IO (WebSocket), so the agent's port must be reachable
from the primary.

## Backup & restore

Back up the two persistent paths — app data (`/app/data`) and the stacks
directory — that is the whole service state. Stop the container first for a
clean copy, then restore by putting the paths back and starting the container.
The shipped `scripts/backup.sh` / `scripts/restore.sh` do exactly this.

## Troubleshooting

### Stacks not appearing

Dockge cannot follow a symlink unless the symlink target is mounted inside the
container. Confirm the source tree is mounted (see [Volumes](#volumes)) and
restart Dockge.

### Agent not connecting

1. Verify the agent is running on its host: `docker ps | grep dockge`
2. Check port reachability from the primary: `nc -zv <agent-host> 5001`
3. Ensure the credentials match the account created on the agent instance
