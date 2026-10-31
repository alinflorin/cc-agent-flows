//! The QEMU command line of a VM, shared by the image builder and the agent runs: the machine
//! (accelerator, CPU, memory, kernel booted directly), its disk, and `qemu-img`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri::AppHandle;
use tokio::process::Command;

pub fn arch() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(format!("unsupported architecture {other}")),
    }
}

pub fn resources(app: &AppHandle) -> Result<PathBuf, String> {
    crate::util::resource_dir(app)
}

/// The host's logical cores: "no CPU limit" for a VM.
fn host_cpus() -> u32 {
    std::thread::available_parallelism().map_or(2, |n| n.get() as u32)
}

/// Whether the host offers the hardware accelerator `machine` asks for (KVM, HVF; WHPX is not
/// checked). When it does, QEMU still falls back to TCG if the accelerator fails to start.
fn hw_accel() -> bool {
    static HW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HW.get_or_init(|| {
        if cfg!(target_os = "linux") {
            std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok()
        } else if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "kern.hv_support"])
                .output()
                .is_ok_and(|o| o.stdout.starts_with(b"1"))
        } else {
            true
        }
    })
}

/// The arguments that boot the kernel shipped beside the plain image directly (no initramfs, no
/// bootloader, no firmware): `dir` is where `alpine-<arch>.vmlinuz` is. The root
/// is the first virtio disk. A builder (`agent` false) gets the kernel's output and the stock init
/// with a login on the serial port; an agent VM is silent and starts `ccaf-init`, which brings up
/// the little it needs and runs agentd (no OpenRC, no DHCP).
pub fn boot_args(dir: &Path, arch: &str, agent: bool) -> Vec<std::ffi::OsString> {
    let tty = if arch == "aarch64" { "ttyAMA0" } else { "ttyS0" };
    let mut cmdline = if agent {
        "quiet loglevel=0 8250.nr_uarts=0 random.trust_cpu=on mitigations=off noresume raid=noautodetect root=/dev/vda rootfstype=ext4 rw init=/sbin/ccaf-init".to_string()
    } else {
        format!("console={tty} root=/dev/vda rootfstype=ext4 rw")
    };
    if agent && arch == "x86_64" {
        // no keyboard controller probe, no timer checks, a trusted TSC: all time off the boot
        cmdline.push_str(" i8042.noaux i8042.nokbd tsc=reliable no_timer_check");
    }
    // WHPX: the guest kernel panics while setting up XSAVE state (fpu__init_system_xstate), so it
    // must not use XSAVE (no AVX in the guest; Claude Code's SSE4.2 baseline is enough)
    if cfg!(windows) && arch == "x86_64" {
        cmdline.push_str(" noxsave");
    }
    vec![
        "-kernel".into(),
        dir.join(format!("alpine-{arch}.vmlinuz")).into(),
        "-append".into(),
        cmdline.into(),
    ]
}

/// Most vCPUs a VM gets when neither its profile nor the settings say: more only slow the boot and
/// oversubscribe a host that runs several VMs.
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
    let settings = crate::config::vm_settings::current(app);
    use crate::config::vm_settings::Accel;
    let hw = match settings.accel {
        Accel::Software => false,
        Accel::Auto => hw_accel(),
        Accel::Hardware if hw_accel() => true,
        Accel::Hardware => return Err(format!("hardware acceleration is not available on this computer ({accel} cannot be used): change the accelerator in Settings > General > Virtual machines")),
    };
    // pauth-impdef: without it software emulation spends minutes on pointer authentication. Only
    // TCG has the option: under hvf/kvm `max` is the host CPU and QEMU refuses to start with it.
    let cpu = if arch == "aarch64" && !hw { "max,pauth-impdef=on" } else { "max" };
    let vcpus = cpus.or(settings.default_cpus).unwrap_or_else(|| host_cpus().min(DEFAULT_VCPUS));
    if hw {
        cmd.args(["-accel", accel]);
    }
    // hardware only: no software fallback, QEMU refuses to start when the accelerator fails
    if settings.accel != Accel::Hardware {
        cmd.args(["-accel", "tcg"]);
    }
    cmd.args(["-cpu", cpu, "-m", &mem_mb.to_string(), "-smp", &vcpus.to_string()]);
    cmd.args(if arch == "aarch64" { ["-M", "virt"] } else { ["-M", "pc,usb=off,vmport=off"] });
    // no VGA, CD-ROM, floppy, parallel port or default NIC to probe: the caller adds what the VM has
    cmd.args(["-nodefaults", "-no-user-config", "-display", "none", "-monitor", "none", "-no-reboot"]);
    cmd.args(boot_args(&resources(app)?.join("images"), arch, agent));
    Ok(cmd)
}

/// Boxes in the QEMU process that `cmd` will start (see `confine`), unless the setting is off.
/// `disks` are the disks it opens (their backing chains are found), `write` the VM's own folder.
pub fn confine(app: &AppHandle, cmd: &mut std::process::Command, disks: &[&Path], write: &[&Path]) {
    if !crate::config::vm_settings::current(app).confine {
        return;
    }
    let plan = super::confine::Plan {
        disks: disks.iter().map(|p| p.to_path_buf()).collect(),
        // the shipped kernel (and the plain image beside it)
        read: resources(app).map(|r| vec![r.join("images")]).unwrap_or_default(),
        write: write.iter().map(|p| p.to_path_buf()).collect(),
        exec: Vec::new(),
    };
    super::confine::apply(cmd, &plan);
}

/// Windows: puts the started VM process into a restricted job object (see `confine`). Elsewhere a no-op.
pub fn confine_started(app: &AppHandle, child: &tokio::process::Child) {
    if crate::config::vm_settings::current(app).confine {
        super::confine::after_spawn(child);
    }
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
        .map_err(|e| super::qemu::spawn_error(app, "qemu-img", &e))?;
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
