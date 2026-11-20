//! Read the platform's sealed palette for release and development resolution.
use wamn_component_policy::AdmittedComponentFacts;

#[derive(Debug)]
pub struct PaletteComponent {
    pub facts: AdmittedComponentFacts,
}
