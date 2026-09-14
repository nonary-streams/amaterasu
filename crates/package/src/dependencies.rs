use std::collections::BTreeSet;

use crate::{ComponentManifest, PackageResult};

/// Return a deterministic initialization order, preserving manifest order whenever
/// dependencies permit it. Destruction uses the reverse of this order.
pub fn ordered_components(
    components: &[ComponentManifest],
) -> PackageResult<Vec<&ComponentManifest>> {
    let mut remaining: Vec<_> = components.iter().collect();
    let mut initialized = BTreeSet::new();
    let mut ordered = Vec::with_capacity(components.len());

    while !remaining.is_empty() {
        let ready_index = remaining.iter().position(|component| {
            component
                .requires
                .keys()
                .all(|dependency| initialized.contains(dependency))
        });
        let Some(ready_index) = ready_index else {
            return Err(
                "component dependency graph contains a cycle or an absent dependency".into(),
            );
        };
        let component = remaining.remove(ready_index);
        initialized.insert(component.id.clone());
        ordered.push(component);
    }

    Ok(ordered)
}
