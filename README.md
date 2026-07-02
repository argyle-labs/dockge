<p align="center">
  <img src="assets/icon-256.png" width="120" alt="dockge" />
</p>

# dockge

Dockge is a self-hosted manager for Docker Compose stacks with a clean web UI.

A first-party [orca](https://github.com/argyle-labs/orca) plugin (service-backend).

This repo **ships a `compose.yml`** — run dockge **by hand, without orca** straight from it:

---

## Run it without orca

### Docker Compose

```sh
docker compose up -d
```

See [`compose.yml`](compose.yml) for the image, ports, volumes, and hardware/device mappings and `scripts/` for provisioning helpers.

### Other runtimes

**Podman**: `podman compose -f compose.yml up -d`. **LXC / VM**: run the same image via Docker/Podman on the guest, or install upstream. **Unraid**: *Docker → Add Container* with the image/ports/volumes from `compose.yml`. Upstream: <https://github.com/louislam/dockge>.


### Backup & restore

Back up the config/data volume(s) above — that's the whole service state (stop the container first for a clean copy). Restore by putting them back and starting it.

> With orca this is **`service.backup` / `service.restore`** — location-agnostic (docker / podman / lxc / vm), one command regardless of where dockge runs. No per-service backup script.

## With orca

orca drives this plugin through the single generic `service.*` surface — no per-plugin tools:

```sh
orca service.deploy dockge      # render + launch on any supported runtime
orca service.status dockge      # health + rich diagnostics (typed payload)
orca service.backup dockge      # location-agnostic backup (tar; PBS on Proxmox)
orca service.configure dockge   # apply config via the upstream API
```

## Layout

- `src/` — the plugin (pure Rust): the `ServiceBackend` descriptor + `configure` / `status`.
- `compose.yml` — standalone deployment.
- `scripts/` — provisioning / lifecycle helpers.
- `assets/` — plugin icon.
