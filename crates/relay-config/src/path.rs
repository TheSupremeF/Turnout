use serde::{Deserialize, Serialize};

/// A configuration key path. Prefer [`ConfigPath::new`] for keys that may
/// contain dots; [`ConfigPath::dotted`] splits on `.`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigPath(Vec<String>);

impl ConfigPath {
    pub fn new<S: Into<String>>(segments: impl IntoIterator<Item = S>) -> Self {
        ConfigPath(segments.into_iter().map(Into::into).collect())
    }

    pub fn dotted(path: &str) -> Self {
        ConfigPath(path.split('.').map(str::to_string).collect())
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn child(&self, segment: impl Into<String>) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment.into());
        ConfigPath(segments)
    }

    pub fn parent(&self) -> Option<ConfigPath> {
        if self.0.is_empty() {
            return None;
        }
        Some(ConfigPath(self.0[..self.0.len() - 1].to_vec()))
    }

    pub fn last(&self) -> Option<&String> {
        self.0.last()
    }

    pub fn join(&self) -> String {
        self.0.join(".")
    }
}

impl From<&str> for ConfigPath {
    fn from(value: &str) -> Self {
        ConfigPath::dotted(value)
    }
}

impl From<Vec<String>> for ConfigPath {
    fn from(value: Vec<String>) -> Self {
        ConfigPath(value)
    }
}

impl std::fmt::Display for ConfigPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.join())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_paths_split() {
        assert_eq!(
            ConfigPath::dotted("a.b.c").segments(),
            &["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn new_keeps_dots_in_keys() {
        let path = ConfigPath::new(["a.b", "c"]);
        assert_eq!(path.segments(), &["a.b".to_string(), "c".to_string()]);
    }

    #[test]
    fn child_and_parent_round_trip() {
        let path = ConfigPath::dotted("a.b").child("c");
        assert_eq!(path.join(), "a.b.c");
        assert_eq!(path.parent().unwrap().join(), "a.b");
    }
}
