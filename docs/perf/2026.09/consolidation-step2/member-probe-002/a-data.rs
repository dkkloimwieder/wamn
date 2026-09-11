use std::hash::{Hash,Hasher};
struct Marker;
#[inline(never)]
pub fn type_key()->u64 {let mut hasher=std::collections::hash_map::DefaultHasher::new(); std::any::TypeId::of::<Marker>().hash(&mut hasher); hasher.finish()}
