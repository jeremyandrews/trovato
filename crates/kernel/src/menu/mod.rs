//! Menu system for route and navigation management.
//!
//! Menus are collected from plugins via the `tap_menu` tap and provide:
//! - Route definitions for the HTTP router
//! - Navigation structure for admin/frontend
//! - Permission requirements per route

mod registry;

pub use registry::{MenuDefinition, MenuRegistry, RouteMatch};
