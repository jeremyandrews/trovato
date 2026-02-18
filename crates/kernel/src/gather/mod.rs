//! Gather query engine module.
//!
//! This module provides:
//! - CategoryService: Manages categories, tags, and hierarchies
//! - GatherService: Executes declarative gather queries
//! - GatherQueryBuilder: SeaQuery-based SQL generation
//! - GatherExtensionRegistry: Plugin-provided filter/relationship/sort extensions
//! - Types: QueryDefinition, QueryDisplay, FilterOperator, etc.

mod category_service;
pub mod extension;
mod gather_service;
mod handlers;
mod query_builder;
pub mod types;

#[allow(unused_imports)]
pub use category_service::CategoryService;
#[allow(unused_imports)]
pub use extension::{
    FilterContext, FilterExtension, FilterHandler, GatherExtensionDeclaration,
    GatherExtensionRegistry, JoinSpec, RelationshipContext, RelationshipExtension,
    RelationshipHandler, SortContext, SortExtension, SortHandler,
};
#[allow(unused_imports)]
pub use gather_service::{GatherService, MAX_ITEMS_PER_PAGE};
#[allow(unused_imports)]
pub use handlers::{HierarchicalInFilterHandler, JsonbArrayContainsFilterHandler};
#[allow(unused_imports)]
pub use query_builder::{CategoryHierarchyQuery, GatherQueryBuilder};
#[allow(unused_imports)]
pub use types::{
    DisplayFormat, FilterOperator, FilterValue, GatherQuery, GatherResult, IncludeDefinition,
    JoinType, NullsOrder, PagerConfig, PagerStyle, QueryContext, QueryDefinition, QueryDisplay,
    QueryField, QueryFilter, QueryRelationship, QuerySort, SortDirection,
};
