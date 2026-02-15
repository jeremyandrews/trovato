//! Gather query engine module.
//!
//! This module provides:
//! - CategoryService: Manages categories, tags, and hierarchies
//! - GatherService: Executes declarative view queries
//! - ViewQueryBuilder: SeaQuery-based SQL generation
//! - Types: ViewDefinition, ViewDisplay, FilterOperator, etc.

mod category_service;
mod gather_service;
mod query_builder;
pub mod types;

pub use category_service::CategoryService;
pub use gather_service::GatherService;
pub use query_builder::{CategoryHierarchyQuery, ViewQueryBuilder};
pub use types::{
    DisplayFormat, FilterOperator, FilterValue, GatherResult, GatherView, IncludeDefinition,
    JoinType, NullsOrder, PagerConfig, PagerStyle, QueryContext, SortDirection, ViewDefinition,
    ViewDisplay, ViewField, ViewFilter, ViewRelationship, ViewSort,
};
