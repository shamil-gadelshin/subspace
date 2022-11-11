//! Service and ServiceFactory implementation. Specialized wrapper over substrate service.

mod core_domain;
mod rpc;
mod system_domain;

pub use self::system_domain::{new_full, NewFull};
