//! What keeps an agent VM inside its sandbox: the egress proxy, the TLS inspection that injects
//! credentials, and the SSH tunnel that carries both.

pub mod egress;
pub mod inspect;
pub mod tunnel;
