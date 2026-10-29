//! How the app starts virtual machines on this computer (Settings > General > Virtual machines),
//! global, in `vm.json` of the app data dir: the defaults for agent VMs, how many may run, the
//! accelerator, the time limits around a run and an image build, and the builder VM's size. The
//! network, the CPU model and the machine type are not here on purpose: they are what keeps the
//! guest sealed and bootable. Missing or damaged file = the defaults.

use serde::{Deserialize, Serialize};

use crate::util::{read_json, write_json};

/// Which accelerator the VMs use.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Accel {
    /// Hardware (KVM, HVF, WHPX) when the computer has it, else software emulation.
    #[default]
    Auto,
    /// Hardware only: a VM that cannot get it fails instead of crawling.
    Hardware,
    /// Software emulation (TCG) only: slow, for debugging or a broken hypervisor.
    Software,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VmSettings {
    /// vCPUs of an agent VM whose profile gives none; `None`: the host's cores, at most 4.
    pub default_cpus: Option<u32>,
    /// Memory a new agent profile starts with (MB). Existing profiles keep theirs.
    pub default_memory_mb: u32,
    /// VMs running at once in the whole app (applied when the app starts).
    pub max_vms: u32,
    /// VMs running at once for one session.
    pub max_vms_per_session: u32,
    pub accel: Accel,
    /// Seconds on top of an agent's timeout for its VM to boot and shut down.
    pub run_grace_secs: u64,
    /// Minutes an image build script may run.
    pub image_script_mins: u32,
    pub builder_cpus: u32,
    pub builder_memory_mb: u32,
}

impl Default for VmSettings {
    fn default() -> Self {
        VmSettings {
            default_cpus: None,
            default_memory_mb: 2048,
            max_vms: 8,
            max_vms_per_session: 4,
            accel: Accel::Auto,
            run_grace_secs: 300,
            image_script_mins: 25,
            builder_cpus: 2,
            builder_memory_mb: 1024,
        }
    }
}

/// The fields that mean the same on another computer, so the only ones the export carries (the
/// others depend on this machine's cores, memory and hypervisor).
pub const PORTABLE: [&str; 3] = ["default_memory_mb", "run_grace_secs", "image_script_mins"];

impl VmSettings {
    pub fn validate(&self) -> Result<(), String> {
        let range = |name: &str, v: u64, lo: u64, hi: u64| {
            if (lo..=hi).contains(&v) { Ok(()) } else { Err(format!("{name} must be between {lo} and {hi}")) }
        };
        if let Some(c) = self.default_cpus {
            range("the default CPU count", c.into(), 1, 64)?;
        }
        range("the default memory", self.default_memory_mb.into(), 1024, 65536)?;
        range("the VM limit", self.max_vms.into(), 1, 32)?;
        range("the VM limit per session", self.max_vms_per_session.into(), 1, 16)?;
        range("the run grace time", self.run_grace_secs, 60, 3600)?;
        range("the image script time limit", self.image_script_mins.into(), 5, 240)?;
        range("the builder CPU count", self.builder_cpus.into(), 1, 16)?;
        range("the builder memory", self.builder_memory_mb.into(), 1024, 32768)?;
        Ok(())
    }
}

fn path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    crate::util::data_dir(app).ok().map(|d| d.join("vm.json"))
}

/// The settings in force (the defaults when nothing valid is saved).
pub fn current(app: &tauri::AppHandle) -> VmSettings {
    path(app).and_then(|p| read_json::<VmSettings>(&p)).filter(|s| s.validate().is_ok()).unwrap_or_default()
}

#[tauri::command]
pub fn vm_settings_get(app: tauri::AppHandle) -> VmSettings {
    current(&app)
}

#[tauri::command]
pub fn vm_settings_set(app: tauri::AppHandle, settings: VmSettings) -> Result<VmSettings, String> {
    settings.validate()?;
    write_json(&path(&app).ok_or("no data directory")?, &settings)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_limits_are_checked() {
        assert!(VmSettings::default().validate().is_ok());
        assert!(VmSettings { max_vms: 0, ..Default::default() }.validate().is_err());
        assert!(VmSettings { default_memory_mb: 512, ..Default::default() }.validate().is_err());
        assert!(VmSettings { default_cpus: Some(0), ..Default::default() }.validate().is_err());
    }

    #[test]
    fn a_partial_file_takes_the_defaults_for_the_rest() {
        let s: VmSettings = serde_json::from_str(r#"{"accel":"hardware","max_vms":3}"#).unwrap();
        assert_eq!(s.accel, Accel::Hardware);
        assert_eq!(s.max_vms, 3);
        assert_eq!(s.run_grace_secs, 300);
    }
}
