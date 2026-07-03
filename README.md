<p align="center">
  <img src="assets/icon-256.png" width="120" alt="dockge" />
</p>

# dockge

[Dockge](https://github.com/louislam/dockge) is a self-hosted manager for Docker Compose stacks with a clean web UI. Unlike the local-only `docker` plugin, **dockge exposes a network surface** — so orca reaches many dockge instances **over the network** and manages their credentials, the same way the `proxmox` plugin handles PVE endpoints.

A first-party [orca](https://github.com/argyle-labs/orca) plugin. Dockge speaks **no REST API** — it is **Socket.IO v4**, auth = username/password → a JWT returned in the login ack, authorized per-connection. This plugin wraps orca's generic Socket.IO transport; it owns only dockge's event vocabulary.

Everything here works **two ways, both supported and documented**:

- **With orca** — register your instances, then drive stacks through orca's generic surfaces.
- **Without orca (standalone)** — run dockge straight from the shipped [`compose.yml`](compose.yml).

---

## Run it without orca

### Docker Compose

```sh
docker compose up -d
```

See [`compose.yml`](compose.yml) for the image (`louislam/dockge`), ports (**5001**), and the two persistent paths — its app data (`/app/data`) and the managed-stack directory (`/opt/stacks`).

### Other runtimes

| target | how |
| --- | --- |
| Podman | `podman compose -f compose.yml up -d` |
| LXC / VM | run the same image via Docker/Podman on the guest |
| Unraid | *Docker → Add Container* with the image/ports/volumes from `compose.yml` |

Upstream install docs: <https://github.com/louislam/dockge>.

### Backup & restore

Back up the two volumes above — that is the whole service state (stop the container first for a clean copy). Restore by putting them back and starting it. The shipped `scripts/backup.sh` / `scripts/restore.sh` do exactly this.

---

## With orca

orca reaches dockge instances over the network. There are **two surfaces**, both generic — the plugin adds no bespoke stack verbs.

### 1. Register instances — the endpoint registry

`dockge.*` is the registry of dockge **instances** (each with a network address + login). The password is stored via orca's **secrets domain**, never plaintext in the row.

| command | what it does |
| --- | --- |
| `dockge.create` | register a dockge instance (`base_url`, `username`, `password`) |
| `dockge.list` | list registered instances (secret excluded) |
| `dockge.detail` | show one instance (secret excluded) |
| `dockge.update` | edit an instance's address / credentials |
| `dockge.delete` | remove an instance |

```jsonc
// dockge.create — register an instance (password flows to the secrets domain)
{ "name": "baldur", "base_url": "wss://baldur.example:5001", "username": "admin", "password": "…", "enabled": true }
```

### 2. Manage stacks — the generic unit surface

Every compose **stack** on every registered instance is surfaced as a `stack` **unit**. Its manager is `dockge@<instance>`, so a call routes to the right instance over Socket.IO. Drive it through orca's five-verb `unit` surface — no dockge-specific tools:

| verb | action(s) | what it does |
| --- | --- | --- |
| `list` | — | every stack on every enabled instance |
| `detail` | — | one stack's compose YAML / env / status |
| `update` | `start` | start the stack |
| `update` | `stop` | stop the stack |
| `update` | `restart` | restart the stack |
| `update` | `down` | tear the stack down |
| `update` | `update` | pull + redeploy the stack |
| `delete` | — | remove the stack |

> Stack **create** (`deployStack`) needs a multi-argument Socket.IO emit and is a toolkit follow-up; every single-arg lifecycle action above works today.

### Follow-ups

Deploy/backup/restore of the **dockge app itself** and stack **create** land as a `service` domain backend + a multi-arg emit — surfaced through orca's generic `service.*` / `unit.create`, still with no bespoke verbs.

---

## Layout

- `src/lib.rs` — the Socket.IO [`Client`] (dockge's event vocabulary over orca's transport).
- `src/tools.rs` — the `dockge.*` endpoint registry (`endpoint_resource!`).
- `src/unit_provider.rs` — stacks-as-units on the five-verb surface.
- `src/registration.rs` — the `unit` domain-backend descriptor + FFI dispatch.
- `compose.yml` — standalone deployment.
- `scripts/` — provisioning / backup / restore helpers (the standalone path).
- `assets/` — plugin icon.
