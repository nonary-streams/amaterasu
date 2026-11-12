//! In-process access to the real state store for dependent module tests.
//!
//! These are the state provider's exact domain modules, compiled without its
//! native plugin entry points so another provider's test binary can link them.
#[path = "../../src/collection.rs"]
mod collection;
#[path = "../../src/draft.rs"]
mod draft;
#[path = "../../src/protocol.rs"]
mod protocol;
#[path = "../../src/snapshot.rs"]
mod snapshot;
#[path = "../../src/store.rs"]
mod store;

pub mod test_support {
    use serde_json::Value;
    use std::path::PathBuf;

    pub struct TestStateStore(crate::store::StateStore);

    impl TestStateStore {
        pub fn open(directory: PathBuf) -> Result<Self, String> {
            crate::store::StateStore::open(directory).map(Self)
        }

        pub fn handle(&self, caller: &str, payload: Value) -> Value {
            self.0.handle(caller, payload)
        }
    }
}
