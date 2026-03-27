//! Transport-level TCP proxy — delegates to `oprc_netsim::proxy`.
//!
//! Re-exports the shared proxy from `oprc_netsim`, keeping the dev-server's
//! public interface unchanged.

pub use oprc_netsim::proxy::{TransportProxy, find_free_port};
