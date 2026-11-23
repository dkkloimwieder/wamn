//! Host-owned authority used by the application.

use std::any::Any;
use std::sync::Arc;

use crate::ingress::RouteProvider;

/// Host-owned authority exposes routing facts without coupling service types.
pub trait InvocationAuthority: Any + Send + Sync {
    fn routes(&self) -> Option<Arc<dyn RouteProvider>> {
        None
    }
}
