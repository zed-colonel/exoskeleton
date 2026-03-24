//! Inbox implementations for the vessel's Perceive step.
//!
//! Two implementations:
//! - `FileInbox`: production use, watches a directory for JSON envelope files
//! - `InMemoryInbox`: testing, in-memory push/receive

pub mod file_inbox;
pub mod memory_inbox;
pub mod stream_handler;

pub use file_inbox::FileInbox;
pub use memory_inbox::InMemoryInbox;
pub use stream_handler::InboxStreamHandler;
