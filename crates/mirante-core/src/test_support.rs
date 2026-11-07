use std::path::{Path, PathBuf};

/// A unique directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mirante-core-{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_is_created_and_removed() {
        let kept;
        {
            let d = TempDir::new("smoke");
            kept = d.path().to_path_buf();
            assert!(kept.is_dir());
        }
        assert!(!kept.exists());
    }
}
