//! Catalog-derived, transport-neutral PostgreSQL entity access.

mod error;
mod planner;
mod shape;

pub use error::EntityAccessError;
pub use planner::{
    CompareOp, EntityOperation, EntityPlan, EntityRequest, Expansion, ExpansionDirection, Filter,
    ListOptions, PlanKind, Planner, Sort, SortDirection, UpdateMode,
};
pub use shape::{attach_expansion, shape_row, shape_rows};
