//! Crate-wide error type. Later tasks add variants as new pipeline stages
//! (ways, relations, admin hierarchy, ...) grow their own failure modes.

use std::fmt;

#[derive(Debug)]
pub enum ExtractError {
    /// Raised when a node table load would exceed the caller-supplied
    /// `max_nodes` capacity guard. v1 targets metro/regional extracts only;
    /// planet-scale inputs need a different (disk-backed) node store.
    TooManyNodes { seen: u64, max: u64 },
    Pbf(osmpbf::Error),
    Io(std::io::Error),
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExtractError::TooManyNodes { seen, max } => write!(
                f,
                "too many nodes: seen {seen}, max {max} — planet-scale extracts are not supported in v1"
            ),
            ExtractError::Pbf(e) => write!(f, "pbf error: {e}"),
            ExtractError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for ExtractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExtractError::TooManyNodes { .. } => None,
            ExtractError::Pbf(e) => Some(e),
            ExtractError::Io(e) => Some(e),
        }
    }
}

impl From<osmpbf::Error> for ExtractError {
    fn from(e: osmpbf::Error) -> Self {
        ExtractError::Pbf(e)
    }
}

impl From<std::io::Error> for ExtractError {
    fn from(e: std::io::Error) -> Self {
        ExtractError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_many_nodes_message_mentions_v1_limitation() {
        let e = ExtractError::TooManyNodes { seen: 11_000_000, max: 10_000_000 };
        let msg = e.to_string();
        assert!(
            msg.contains("planet-scale extracts are not supported in v1"),
            "message was: {msg}"
        );
        assert!(msg.contains("11000000"));
        assert!(msg.contains("10000000"));
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let e: ExtractError = io_err.into();
        assert!(matches!(e, ExtractError::Io(_)));
    }
}
