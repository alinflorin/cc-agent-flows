//! Everything that boots a QEMU VM: the bundled QEMU, images and their builder VM, the agent
//! channel and its protocol, and the run of one agent.

pub mod agent;
#[cfg(test)]
mod agentd_e2e;
pub mod builder;
pub mod images;
pub mod machine;
pub mod protocol;
pub mod qemu;
pub mod runs;
