//! Menu system for route and navigation management.
//!
//! Menus are collected from plugins via the `tap_menu` tap and provide:
//! - Route definitions for the HTTP router
//! - Navigation structure for admin/frontend
//! - Permission requirements per route

pub mod navigation;
mod registry;

pub use navigation::{MenuTreeLink, SiteMenus, clear_local_config_menus, invalidate_config_menus};
pub use registry::{MenuDefinition, MenuRegistry, RouteMatch, nav_entry_for};
