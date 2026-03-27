//! Network partition simulation — delegates to `oprc_netsim`.
//!
//! Re-exports the shared pairwise state and HTTP debug API from
//! `oprc_netsim::state`, keeping the dev-server's public interface unchanged.

pub use oprc_netsim::state::{NetworkSimState, build_debug_api, build_v1_api};
pub use oprc_netsim::types::LinkState;
