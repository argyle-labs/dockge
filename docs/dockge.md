# Dockge

Docker Compose stack manager with multi-host agent support.

**Status:** running — primary host (`:5001`) + agent host (`:5001`)

> **Host IPs**: see your network map / inventory.

## Architecture

```
┌──────────────────────────────────────────┐
│             Dockge Setup                 │
│                                          │
│  ┌─────────────┐   ┌─────────────┐       │
│  │  primary    │   │    agent    │       │
│  │  Docker VM  │   │  Docker VM  │       │
│  │             │   │             │       │
│  │ ┌─────────┐ │   │ ┌─────────┐ │       │
│  │ │ Dockge  │ │   │ │ Dockge  │ │       │
│  │ │ Primary │ │   │ │  Agent  │ │       │
│  │ └─────────┘ │   │ └─────────┘ │       │
│  └─────────────┘   └─────────────┘       │
└──────────────────────────────────────────┘
```

- **Primary**: unified UI for all hosts (port 5001)
- **Agent**: connected from the primary's UI via Socket.IO (port 5001)

Every Dockge instance uses the **same image** (`louislam/dockge:1`). There is no separate agent binary. One instance is designated primary; others are added as agents from the primary's UI.

## Replaces Portainer

Dockge replaces a Portainer manager + agent setup. Key differences:

- No GitHub auto-pull — sync compose files to hosts yourself (e.g. `git pull` on each host)
- Stacks are local compose files in `/opt/stacks`, symlinked to the repo
- Each host runs a full Dockge instance (not a lightweight agent)
- Primary connects to agents via Socket.IO (WebSocket) using agent credentials

## Repo Integration

Each Docker host clones the compose repo and symlinks service compose dirs into `/opt/stacks`:

```
/opt/<repo>/               ← git clone of your compose repo
/opt/stacks/<service>      ← symlink to /opt/<repo>/compose/<service>
/opt/dockge/data/          ← Dockge internal data (SQLite DB, etc.)
```

Update workflow:
1. Push changes to your git remote
2. Pull latest on each host
3. Restart changed stacks from Dockge UI

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DOCKGE_STACKS_DIR` | `/opt/stacks` | Directory where compose stacks are stored |
| `TZ` | `America/Denver` | Timezone |

### Volumes

| Host Path | Container Path | Purpose |
|-----------|----------------|---------|
| `/var/run/docker.sock` | `/var/run/docker.sock` | Docker engine access |
| `/opt/dockge/data` | `/app/data` | Dockge database and config |
| `/opt/stacks` | `/opt/stacks` | Compose stack directory |
| `/opt/<repo>` | `/opt/<repo>` (ro) | Repo mount — required for symlink resolution |

> The `/opt/<repo>` mount is required because stacks in `/opt/stacks` are symlinks to `/opt/<repo>/compose/<service>`. Without this mount, Dockge cannot read the compose files inside the container.

## Deployment

### Step 1: Deploy Dockge on the agent host

```bash
ssh <agent-host>
mkdir -p /opt/dockge/data /opt/stacks
cd /opt/<repo>/compose/dockge && docker compose up -d
```

Visit **http://\<agent-host\>:5001** — create admin account on first visit.

### Step 2: Symlink agent-host services

```bash
for svc in sabnzbd qbittorrent prowlarr sonarr radarr radarr-4k bazarr lidarr kapowarr mylar3 lazylibrarian; do
  ln -s /opt/<repo>/compose/$svc /opt/stacks/$svc
done
```

### Step 3: Deploy Dockge on the primary host

```bash
ssh <primary-host>
mkdir -p /opt/dockge/data /opt/stacks
cd /opt/<repo>/compose/dockge && docker compose up -d
```

Visit **http://\<primary-host\>:5001** — create admin account.

### Step 4: Symlink primary-host services

```bash
for svc in audiobookshelf calibre-web kavita komga libation navidrome immich ntfy uptime-kuma; do
  ln -s /opt/<repo>/compose/$svc /opt/stacks/$svc
done
```

### Step 5: Add the agent host from the primary

1. Open **http://\<primary-host\>:5001**
2. Click **Add Agent**
3. Enter URL: `http://<agent-host-ip>:5001`
4. Enter the username/password created on the agent host in Step 1

All agent-host stacks now appear in the primary's unified view.

### Step 6: Start stacks from Dockge UI

For each service, click into the stack in Dockge and click **Compose Up**. Dockge must start the containers itself to track them.

## Access

- **Primary**: `http://<primary-host>:5001`
- **Agent**: `http://<agent-host>:5001`

## Prerequisites

If a host routes through a VPN, it may need a firewall bypass rule to reach GitHub / GHCR for `git clone`/`pull` and image pulls. See your OPNsense (or router) setup for a `github_hosts` alias + floating bypass rule.

## Troubleshooting

### Stacks not appearing in Dockge

Dockge cannot follow symlinks unless the symlink target is mounted inside the container. Ensure `/opt/<repo>` is mounted as a volume in the Dockge compose file:

```yaml
volumes:
  - /opt/<repo>:/opt/<repo>:ro
```

Restart Dockge after adding the mount:
```bash
cd /opt/stacks/dockge && docker compose down && docker compose up -d
```

### Agent not connecting

1. Verify the agent is running: `docker ps | grep dockge` (on the agent host)
2. Check port reachability from the primary: `nc -zv <agent-host> 5001`
3. Ensure credentials match the account created on the agent instance
