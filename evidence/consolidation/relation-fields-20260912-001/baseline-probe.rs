use wamn_schema_generator::{DataAccessOverlay, DataAccessRelationInventory, derive_data_access_overlay_from_inventory};
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 3);
    let manifest = std::fs::read(&args[0]).unwrap();
    let bytes = std::fs::read(&args[1]).unwrap();
    let overlay = DataAccessOverlay::from_slice(&bytes).unwrap();
    let relation_fields = overlay.relations().iter().map(|relation| DataAccessRelationInventory::new(relation.schema(), relation.table(), relation.all_fields().to_vec())).collect::<Vec<_>>();
    let regenerated = derive_data_access_overlay_from_inventory(&relation_fields, &manifest).unwrap().canonical_bytes();
    std::fs::write(&args[2], &regenerated).unwrap();
    assert_eq!(bytes, regenerated);
}
