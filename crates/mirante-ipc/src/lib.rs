//! Wire protocol between `mirante-daemon` and its clients (`mirante`, `mirante-hook`):
//! newline-delimited JSON-RPC 2.0 over a Unix socket, the parameter and result types, and
//! the on-disk layout under `~/.mirante`. Synchronous std I/O only.
pub mod client;
pub mod paths;
pub mod rpc;
pub mod types;

pub use client::{Client, ClientError};
pub use paths::Paths;
pub use rpc::RpcError;

/// A short unique directory under `/tmp`, removed on drop. Unix socket paths are limited to
/// ~104 bytes on macOS, so tests must not use the long per-user temp dir.
#[doc(hidden)]
pub mod test_dir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new() -> TestDir {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let path = PathBuf::from(format!("/tmp/mr-{}-{n}-{nanos:x}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Default for TestDir {
        fn default() -> Self {
            TestDir::new()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
