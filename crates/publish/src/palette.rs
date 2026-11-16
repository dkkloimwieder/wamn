//! Read the platform's sealed palette for release and development resolution.
use wamn_catalog::AdmittedComponentFacts;

#[derive(Debug)]
pub struct PaletteComponent {
    pub facts: AdmittedComponentFacts,
}
