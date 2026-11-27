//! Gather query engine module.
//!
//! This module provides:
//! - CategoryService: Manages categories, tags, and hierarchies
//! - GatherService: Executes declarative gather queries
//! - GatherQueryBuilder: SeaQuery-based SQL generation
//! - GatherExtensionRegistry: Plugin-provided filter/relationship/sort extensions
//! - Types: QueryDefinition, QueryDisplay, FilterOperator, etc.

mod access;
mod category_service;
pub mod extension;
mod gather_service;
mod handlers;
mod query_builder;
pub mod types;

#[allow(unused_imports)]
pub use access::{GatherAccessConfig, GatherDenied, authorize_gather};
#[allow(unused_imports)]
pub use category_service::CategoryService;
#[allow(unused_imports)]
pub use extension::{
    FilterContext, FilterExtension, FilterHandler, GatherExtensionDeclaration,
    GatherExtensionRegistry, JoinSpec, RelationshipContext, RelationshipExtension,
    RelationshipHandler, SortContext, SortExtension, SortHandler,
};
pub(crate) use gather_service::GATHER_JOIN_TABLES;
#[allow(unused_imports)]
pub use gather_service::{GatherConfig, GatherService, MAX_ITEMS_PER_PAGE, PluginScope};
#[allow(unused_imports)]
pub use handlers::{HierarchicalInFilterHandler, JsonbArrayContainsFilterHandler};
#[allow(unused_imports)]
pub use query_builder::{CategoryHierarchyQuery, GatherQueryBuilder};
#[allow(unused_imports)]
pub use types::{
    AggregateFunction, Aggregation, Conjunction, ContextualValue, DisplayFormat, ExposedWidget,
    FilterGroup, FilterOperator, FilterValue, GatherAccess, GatherQuery, GatherResult,
    GatherRoute, GatherRouteParam, IncludeDefinition, JoinType, MAX_GROUP_DEPTH, NullsOrder,
    PagerConfig, PagerStyle, QueryAggregate, QueryContext, QueryDefinition, QueryDisplay,
    QueryField, QueryFilter, QueryRelationship, QuerySort, SortDirection,
};
