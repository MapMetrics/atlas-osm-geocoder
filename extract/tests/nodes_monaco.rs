use atlas_extract::nodes::NodeTable;

#[test]
fn monaco_nodes_load_and_resolve() {
    let t = NodeTable::load("tests/fixtures/monaco.osm.pbf".as_ref(), 10_000_000).unwrap();
    assert!(t.len() > 10_000, "monaco has tens of thousands of nodes, got {}", t.len());
    // every stored location is a sane coordinate
    // (probe: iterate first ways in Task 4's test instead; here just len + spot API shape)
}
