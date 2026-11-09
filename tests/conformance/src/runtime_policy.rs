//! Runtime instance policy checks.

#[test]
fn only_positive_pool_sizes_keep_instances_warm() {
    use wash_runtime::engine::InstancePolicy;
    use wash_runtime::types::Component;

    for pool_size in [i32::MIN, -1, 0, 1, i32::MAX] {
        let component = Component {
            pool_size,
            ..Default::default()
        };
        assert_eq!(
            InstancePolicy::from_component(&component).keeps_instances_warm(),
            pool_size > 0,
            "pool_size {pool_size} must preserve the native fresh-store boundary"
        );
    }
}
