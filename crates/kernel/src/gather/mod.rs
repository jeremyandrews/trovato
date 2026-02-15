//! Gather query engine module.
//!
//! This module provides:
//! - CategoryService: Manages categories, tags, and hierarchies
//! - GatherService: Executes declarative gather queries
//! - GatherQueryBuilder: SeaQuery-based SQL generation
//! - Types: QueryDefinition, QueryDisplay, FilterOperator, etc.

mod category_service;
mod gather_service;
mod query_builder;
pub mod types;

#[allow(unused_imports)]
pub use category_service::CategoryService;
#[allow(unused_imports)]
pub use gather_service::GatherService;
#[allow(unused_imports)]
pub use query_builder::{CategoryHierarchyQuery, GatherQueryBuilder};
#[allow(unused_imports)]
pub use types::{
    DisplayFormat, FilterOperator, FilterValue, GatherQuery, GatherResult, IncludeDefinition,
    JoinType, NullsOrder, PagerConfig, PagerStyle, QueryContext, QueryDefinition, QueryDisplay,
    QueryField, QueryFilter, QueryRelationship, QuerySort, SortDirection,
};
