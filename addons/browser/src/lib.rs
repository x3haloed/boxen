//! Host-local browser primitives. Browser discovery and process ownership are separate
//! from protocol clients so they can vary independently by host and browser.

pub mod browser;
pub mod cdp;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
