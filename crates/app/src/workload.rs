//! Admission rules and drain state for application workloads.

use std::collections::BTreeSet;

/// Verify component references before admitting a workload.
///
/// # Errors
/// Returns the OCI reference refusal for the first invalid image.
pub fn validate_workload_images<'a>(
    images: impl IntoIterator<Item = &'a str>,
) -> Result<(), wamn_oci_reference::ComponentArtifactReferenceError> {
    for image in images {
        wamn_oci_reference::validate_component_start_reference(image)?;
    }
    Ok(())
}

/// How a workload participates in draining.
#[derive(Debug, Clone, Copy)]
pub enum WorkloadClass {
    Service,
    Component,
}

/// Tracks admission and the operator stops still needed during drain.
#[derive(Debug, Default)]
pub struct WorkloadAdmission {
    draining: bool,
    pending_stops: BTreeSet<String>,
    services: BTreeSet<String>,
    components: BTreeSet<String>,
}

impl WorkloadAdmission {
    /// Record a workload before starting it.
    ///
    /// # Errors
    /// Refuses new starts after draining begins.
    pub fn admit(&mut self, workload: &str, class: WorkloadClass) -> anyhow::Result<()> {
        anyhow::ensure!(!self.draining, "host is draining");
        self.pending_stops.insert(workload.to_owned());
        match class {
            WorkloadClass::Service => &mut self.services,
            WorkloadClass::Component => &mut self.components,
        }
        .insert(workload.to_owned());
        Ok(())
    }

    /// Forget a stopped workload or a failed start.
    pub fn stopped(&mut self, workload: &str) {
        self.pending_stops.remove(workload);
        self.services.remove(workload);
        self.components.remove(workload);
    }

    /// Acknowledge the operator stop while deferring component teardown.
    pub fn defer_component_stop(&mut self, workload: &str) -> bool {
        if self.draining && self.components.contains(workload) {
            self.pending_stops.remove(workload);
            true
        } else {
            false
        }
    }

    /// Refuse subsequent starts.
    pub fn begin_drain(&mut self) {
        self.draining = true;
    }

    /// Services that must stop before HTTP drains.
    pub fn services(&self) -> &BTreeSet<String> {
        &self.services
    }

    /// Whether the host has started draining.
    pub fn is_draining(&self) -> bool {
        self.draining
    }

    /// Whether every workload has received its operator stop.
    pub fn operator_stops_complete(&self) -> bool {
        self.pending_stops.is_empty()
    }

    /// Components retained until HTTP requests finish.
    pub fn components(&self) -> &BTreeSet<String> {
        &self.components
    }
}
