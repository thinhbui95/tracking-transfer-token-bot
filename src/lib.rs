pub mod get_event;
pub mod get_config;
pub mod send_message_to_telegram;
pub mod solana_adapter;

pub use get_event::*;
pub use get_config::*;
pub use send_message_to_telegram::*;
pub use solana_adapter::*;

use std::sync::atomic::{AtomicUsize, Ordering};

/// Generic round-robin selector for load balancing across multiple items
pub struct RoundRobin<T> {
    items: Vec<T>,
    counter: AtomicUsize,
}

impl<T: Clone> RoundRobin<T> {
    /// Create a new round-robin selector with the given items
    pub fn new(items: Vec<T>) -> Self {
        Self {
            items,
            counter: AtomicUsize::new(0),
        }
    }
    
    /// Get the next item using round-robin selection
    pub fn next(&self) -> Option<T> {
        if self.items.is_empty() {
            return None;
        }
        let index = self.counter.fetch_add(1, Ordering::Relaxed) % self.items.len();
        Some(self.items[index].clone())
    }
    
    /// Get the number of items
    pub fn len(&self) -> usize {
        self.items.len()
    }
    
    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
