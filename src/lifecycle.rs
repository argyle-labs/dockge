//! Dockge deployment lifecycle tool surface.
//!
//! Net-new over the stack-management surface: these `#[orca_tool]`s own the
//! deploy lifecycle of the Dockge web app *itself* — the container that hosts
//! the compose-stack UI. Dockge ships as a single Docker image
//! (`louislam/dockge`) with two persistent paths: its app data (`/app/data`)
//! and the directory of managed compose stacks (`/opt/stacks`). The lifecycle
//! tools drive `docker` and `tar` through `tokio::process::Command`; the shell
//! scripts in `scripts/` are the curl-bootstrap payload these tools
//! orchestrate, so every capability is reachable as an orca tool.
//!
//! Imports flow through `plugin_toolkit::prelude::*` only — the toolkit is the
//! single gateway. Process exec uses the toolkit's re-exported `tokio`.
#![allow(clippy::disallowed_types)]

use std::process::Output;

use plugin_toolkit::prelude::*;
use plugin_toolkit::tokio::process::Command;

/// Run a command, capturing output, and map a non-zero exit to an error that
/// carries stderr — the lifecycle tools surface the runtime's own message
/// rather than a bare exit code.
async fn run(cmd: &mut Command) -> Result<Output> {
    let output = cmd
        .output()
        .await
        .with_context(|| "failed to spawn command".to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("command failed ({}): {}", output.status, stderr.trim());
    }
    Ok(output)
}

// ═══════════════════════════════════════════════════════════════════════════
// dockge.install — bring up the Dockge Compose stack
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct DockgeInstallArgs {
    /// Directory holding the Dockge `compose.yml` to bring up. Defaults to the
    /// repo-relative asset; override for a non-standard layout.
    #[arg(long, default_value = ".")]
    #[serde(default = "default_project_dir")]
    pub project_dir: String,
}

fn default_project_dir() -> String {
    ".".to_string()
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
#[derive(Debug)]
pub struct DockgeLifecycleOutput {
    /// True when the command completed successfully.
    pub ok: bool,
    /// Combined stdout from the step.
    pub log: String,
}

/// **Provision the Dockge web app.** Runs `docker compose up -d` in the
/// project directory containing Dockge's `compose.yml`, bringing the stack
/// manager online with its `/app/data` and `/opt/stacks` volumes.
#[orca_tool(domain = "dockge", verb = "install")]
async fn dockge_install(args: DockgeInstallArgs, _ctx: &ToolCtx) -> Result<DockgeLifecycleOutput> {
    let mut cmd = Command::new("docker");
    cmd.arg("compose")
        .arg("--project-directory")
        .arg(&args.project_dir)
        .arg("up")
        .arg("-d");
    let out = run(&mut cmd).await?;
    Ok(DockgeLifecycleOutput {
        ok: true,
        log: String::from_utf8_lossy(&out.stdout).into_owned(),
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// dockge.update — pull the latest image and recreate the stack
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct DockgeUpdateArgs {
    /// Project directory holding the Dockge `compose.yml`.
    #[arg(long, default_value = ".")]
    #[serde(default = "default_project_dir")]
    pub project_dir: String,
}

/// **Update the Dockge web app.** Pulls the newest `louislam/dockge` image and
/// recreates the container in place (`docker compose pull && up -d`).
#[orca_tool(domain = "dockge", verb = "update")]
async fn dockge_update(args: DockgeUpdateArgs, _ctx: &ToolCtx) -> Result<DockgeLifecycleOutput> {
    let mut pull = Command::new("docker");
    pull.arg("compose")
        .arg("--project-directory")
        .arg(&args.project_dir)
        .arg("pull");
    let pull_out = run(&mut pull).await?;

    let mut up = Command::new("docker");
    up.arg("compose")
        .arg("--project-directory")
        .arg(&args.project_dir)
        .arg("up")
        .arg("-d");
    let up_out = run(&mut up).await?;

    let mut log = String::from_utf8_lossy(&pull_out.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&up_out.stdout));
    Ok(DockgeLifecycleOutput { ok: true, log })
}

// ═══════════════════════════════════════════════════════════════════════════
// dockge.backup — tar the Dockge data + managed-stacks directories
// ═══════════════════════════════════════════════════════════════════════════

#[derive(
    plugin_toolkit::clap::Args,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
pub struct DockgeBackupArgs {
    /// Host path of Dockge's app data volume (mounts `/app/data`).
    #[arg(long, default_value = "/opt/dockge/data")]
    #[serde(default = "default_data_path")]
    pub data_path: String,
    /// Host path of the managed compose-stacks directory (mounts `/opt/stacks`).
    #[arg(long, default_value = "/opt/stacks")]
    #[serde(default = "default_stacks_path")]
    pub stacks_path: String,
    /// Destination `.tar.gz` archive path.
    #[arg(long)]
    pub out: String,
}

fn default_data_path() -> String {
    "/opt/dockge/data".to_string()
}

fn default_stacks_path() -> String {
    "/opt/stacks".to_string()
}

#[derive(
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
#[derive(Debug)]
pub struct DockgeBackupOutput {
    /// True when the archive was written.
    pub ok: bool,
    /// Path to the archive that was written.
    pub archive: String,
}

/// **Back up a Dockge deployment.** Archives the app-data and managed-stacks
/// directories into a single `.tar.gz`. Restore by extracting over the same
/// paths and running `dockge.install`.
#[orca_tool(domain = "dockge", verb = "backup")]
async fn dockge_backup(args: DockgeBackupArgs, _ctx: &ToolCtx) -> Result<DockgeBackupOutput> {
    let mut cmd = Command::new("tar");
    cmd.arg("czf")
        .arg(&args.out)
        .arg(&args.data_path)
        .arg(&args.stacks_path);
    run(&mut cmd).await?;
    Ok(DockgeBackupOutput {
        ok: true,
        archive: args.out,
    })
}
