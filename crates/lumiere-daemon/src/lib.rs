//! Actor-based discovery, connection, and light state management.

pub mod api;
pub mod config;
pub mod light;
pub mod registry;
pub mod store;

pub use light::LightOp;
pub use registry::{RegistryCmd, RegistryConfig, RegistryHandle, spawn_registry};
