//! What keeps an agent VM inside its sandbox: the egress proxy, the TLS inspection that injects
//! credentials, and the yamux tunnel over a virtio-serial port that carries both.

pub mod egress;
pub mod inspect;
pub mod leaks;
pub mod tunnel;
