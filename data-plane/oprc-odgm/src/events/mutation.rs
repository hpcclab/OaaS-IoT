/// Action performed on a data key during a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutAction {
    Create,
    Update,
    Delete,
}

/// Distinguishes the origin of a state mutation for event consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationSource {
    /// Client-initiated operation (API call, invocation)
    Local,
    /// State arrived via replication sync (MST anti-entropy, Raft log apply)
    Sync,
}

#[derive(Debug, Clone)]
pub struct ChangedKey {
    pub key_canonical: String,
    pub action: MutAction,
    /// Raw entry value bytes (i.e. `ObjectVal.data`).
    /// Populated only when the collection option `ws_event_include_values` is
    /// `"true"` and the mutation is a create/update (never for deletes).
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct MutationContext {
    pub object_id: String,
    pub cls_id: String,
    pub partition_id: u16,
    pub version_before: u64,
    pub version_after: u64,
    pub changed: Vec<ChangedKey>,
    pub source: MutationSource,
    pub event_config: Option<std::sync::Arc<oprc_grpc::ObjectEvent>>,
}

impl MutationContext {
    /// Create a new context for a **local** (client-initiated) mutation.
    pub fn new(
        object_id: String,
        cls_id: String,
        partition_id: u16,
        version_before: u64,
        version_after: u64,
        changed: Vec<ChangedKey>,
    ) -> Self {
        Self {
            object_id,
            cls_id,
            partition_id,
            version_before,
            version_after,
            changed,
            source: MutationSource::Local,
            event_config: None,
        }
    }

    /// Create a new context for a **sync** (replication-originated) mutation.
    pub fn new_sync(
        object_id: String,
        cls_id: String,
        partition_id: u16,
        version_before: u64,
        version_after: u64,
        changed: Vec<ChangedKey>,
    ) -> Self {
        Self {
            object_id,
            cls_id,
            partition_id,
            version_before,
            version_after,
            changed,
            source: MutationSource::Sync,
            event_config: None,
        }
    }

    pub fn with_event_config(
        mut self,
        cfg: Option<std::sync::Arc<oprc_grpc::ObjectEvent>>,
    ) -> Self {
        self.event_config = cfg;
        self
    }
}
