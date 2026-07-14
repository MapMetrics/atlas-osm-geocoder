use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::fs::File;
use serde_json::{json, Map, Value};

pub struct LayerWriter {
    writer: BufWriter<File>,
    count: u64,
}

impl LayerWriter {
    pub fn new(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(LayerWriter {
            writer: BufWriter::new(file),
            count: 0,
        })
    }

    pub fn feature(&mut self, id: u64, props: &Map<String, Value>, geometry: Value) -> io::Result<()> {
        let feature = json!({
            "type": "Feature",
            "id": id,
            "properties": props,
            "geometry": geometry,
        });
        serde_json::to_writer(&mut self.writer, &feature)?;
        self.writer.write_all(b"\n")?;
        self.count += 1;
        Ok(())
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_one_feature_per_line() {
        let dir = std::env::temp_dir().join("ae_emit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("poi.geojsonl");
        let mut w = LayerWriter::new(&p).unwrap();
        let mut props = serde_json::Map::new();
        props.insert("carmen:text".into(), "Cafe X".into());
        w.feature(42, &props, serde_json::json!({"type":"Point","coordinates":[4.9,52.3]})).unwrap();
        drop(w);
        let line = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["type"], "Feature");
        assert_eq!(v["id"], 42);
        assert_eq!(v["properties"]["carmen:text"], "Cafe X");
        assert_eq!(v["geometry"]["type"], "Point");
    }
}
