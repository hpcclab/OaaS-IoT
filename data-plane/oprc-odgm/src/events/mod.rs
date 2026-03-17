pub mod config;
pub mod dispatcher;
pub mod manager;
pub mod mutation;
pub mod processor;
pub mod types;

pub use config::EventConfig;
pub use dispatcher::{EventDispatcher, EventDispatcherRef, QueuedEvent};
pub use manager::EventManagerImpl;
pub use mutation::*;
pub use processor::TriggerProcessor;
pub use types::*;

use crate::shard::ObjectData;

#[async_trait::async_trait]
pub trait EventManager {
    async fn trigger_event(&self, context: EventContext);
    async fn trigger_event_with_entry(
        &self,
        context: EventContext,
        object_entry: &ObjectData,
    );
}
