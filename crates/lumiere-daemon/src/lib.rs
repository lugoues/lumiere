//! Actor-based discovery, connection, and light state management.

pub mod light;
pub mod registry;

pub use light::LightOp;
pub use registry::{RegistryCmd, RegistryConfig, RegistryHandle, spawn_registry};
