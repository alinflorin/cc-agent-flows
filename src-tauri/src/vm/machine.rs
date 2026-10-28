//! The QEMU command line of a VM, shared by the image builder and the agent runs: the machine
//! (accelerator, CPU, memory, kernel booted directly), its disk, and `qemu-img`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tokio::process::Command;

pub fn arch() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(format!("unsupported architecture {other}")),
    }
}

pub fn resources(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().resource_dir().map_err(|e| e.to_string())
}

/// The host's logical cores: "no CPU limit" for a VM.
fn host_cpus() -> u32 {
    std::thread::available_parallelism().map_or(2, |n| n.get() as u32)
}

/// The arguments that boot the kernel and initramfs shipped beside the plain image directly (no
/// bootloader, no firmware): `dir` is where `alpine-<arch>.vmlinuz` and `.initramfs` are. The root
/// is the first virtio disk. A builder (`agent` false) gets the kernel's output and the stock init
/// with a login on the serial port; an agent VM is silent and starts `ccaf-init`, which brings up
/// the little it needs and runs agentd (no OpenRC, no DHCP).
pub fn boot_args(dir: &Path, arch: &str, agent: bool) -> Vec<std::ffi::OsString> {
    let tty = if arch == "aarch64" { "ttyAMA0" } else { "ttyS0" };
    let cmdline = if agent {
        "quiet loglevel=0 8250.nr_uarts=0 noresume root=/dev/vda rootfstype=ext4 rootflags=noatime rw init=/sbin/ccaf-init".to_string()
    } else {
        format!("console={tty} root=/dev/vda rootfstype=ext4 rw")
    };
    vec![
        "-kernel".into(),
        dir.join(format!("alpine-{arch}.vmlinuz")).into(),
        "-initrd".into(),
        dir.join(format!("alpine-{arch}.initramfs")).into(),
        "-append".into(),
        cmdline.into(),
    ]
}

/// Most vCPUs a VM gets when its profile does not say: more only slow the boot and oversubscribe a
/// host that runs several VMs.
const DEFAULT_VCPUS: u32 = 4;

/// A QEMU command for a headless guest of this machine's architecture: hardware acceleration with
/// a software fallback, `-cpu max` (Claude Code needs SSE4.2), no reboot, no default devices, booting
/// the shipped kernel directly. The caller adds the network card (`-nic`): open for builders,
/// sealed for agents. `cpus` is the number of vCPUs (`None`: the host's, at most `DEFAULT_VCPUS`).
/// The caller adds the disk (`drive_arg`) and the channels. `agent`: see `boot_args`.
pub fn machine(app: &AppHandle, mem_mb: u32, cpus: Option<u32>, agent: bool) -> Result<std::process::Command, String> {
    let arch = arch()?;
    let mut cmd = super::qemu::command(app, &format!("qemu-system-{arch}"));
    let accel = if cfg!(target_os = "linux") {
        "kvm"
    } else if cfg!(target_os = "macos") {
        "hvf"
    } else {
        "whpx"
    };
    // pauth-impdef: without it the software fallback spends minutes on pointer authentication
    let cpu = if arch == "aarch64" { "max,pauth-impdef=on" } else { "max" };
    let vcpus = cpus.unwrap_or_else(|| host_cpus().min(DEFAULT_VCPUS));
    cmd.args(["-accel", accel, "-accel", "tcg", "-cpu", cpu, "-m", &mem_mb.to_string(), "-smp", &vcpus.to_string()]);
    cmd.args(if arch == "aarch64" { ["-M", "virt"] } else { ["-M", "pc,usb=off,vmport=off"] });
    // no VGA, CD-ROM, floppy, parallel port or default NIC to probe: the caller adds what the VM has
    cmd.args(["-nodefaults", "-no-user-config", "-display", "none", "-monitor", "none", "-no-reboot"]);
    cmd.args(boot_args(&resources(app)?.join("images"), arch, agent));
    Ok(cmd)
}

/// The disk of a VM that boots a throwaway overlay: no flushes (the overlay is deleted afterwards).
pub fn drive_arg(overlay: &Path) -> String {
    format!("file={},if=virtio,discard=unmap,cache=unsafe", overlay.display())
}

pub async fn qemu_img(app: &AppHandle, args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::from(super::qemu::command(app, "qemu-img"));
    let out = tokio::time::timeout(Duration::from_secs(600), cmd.args(args).output())
        .await
        .map_err(|_| "qemu-img timed out".to_string())?
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("qemu-img: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_get_a_console_and_agents_get_ccaf_init() {
        let line = |agent| boot_args(Path::new("/x"), "x86_64", agent).last().unwrap().to_string_lossy().into_owned();
        let (builder, agent) = (line(false), line(true));
        assert!(builder.contains("console=ttyS0") && !builder.contains("init="), "{builder}");
        assert!(agent.contains("init=/sbin/ccaf-init") && !agent.contains("console="), "{agent}");
        assert!(boot_args(Path::new("/x"), "aarch64", false).last().unwrap().to_string_lossy().contains("console=ttyAMA0"));
    }
}
