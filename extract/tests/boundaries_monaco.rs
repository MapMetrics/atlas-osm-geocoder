use atlas_extract::{boundaries::AdminSet, hierarchy::HierarchyIndex, nodes::NodeTable};

#[test]
fn monaco_admin_areas_assemble() {
    let nodes = NodeTable::load("tests/fixtures/monaco.osm.pbf".as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load("tests/fixtures/monaco.osm.pbf".as_ref(), &nodes).unwrap();

    assert!(
        admin
            .areas()
            .iter()
            .any(|a| a.admin_level == 2 && a.name == "Monaco"),
        "expected an admin_level=2 area named Monaco, got: {:?}",
        admin
            .areas()
            .iter()
            .map(|a| (a.name.as_str(), a.admin_level))
            .collect::<Vec<_>>()
    );

    assert!(
        admin.place_nodes().len() >= 5,
        "expected >=5 place nodes, got {}",
        admin.place_nodes().len()
    );

    for a in admin.areas() {
        for p in &a.rings {
            use geo::CoordsIter;
            assert!(
                p.exterior().0.len() >= 4,
                "ring for {} has too few points: {}",
                a.name,
                p.exterior().coords_count()
            );
            let first = p.exterior().0.first().unwrap();
            let last = p.exterior().0.last().unwrap();
            assert_eq!(
                (first.x, first.y),
                (last.x, last.y),
                "ring for {} is not closed",
                a.name
            );
        }
    }
}

#[test]
fn monaco_hierarchy_resolves_casino_de_monte_carlo() {
    let nodes = NodeTable::load("tests/fixtures/monaco.osm.pbf".as_ref(), 10_000_000).unwrap();
    let admin = AdminSet::load("tests/fixtures/monaco.osm.pbf".as_ref(), &nodes).unwrap();
    let idx = HierarchyIndex::build(&admin);

    // Casino de Monte-Carlo.
    let parents = idx.resolve(7.4247, 43.7394);
    assert_eq!(
        parents.country.as_deref(),
        Some("Monaco"),
        "expected Casino de Monte-Carlo to resolve to country Monaco, got {parents:?}"
    );
}
