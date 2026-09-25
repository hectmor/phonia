//! Which output the daemon plays on, and how to move it to another.
//!
//! The ids are the ones `phonia-core`'s catalog hands out (`exclusive:hw:DS2,0`, `shared:default`,
//! `shared:<output>`); how a sink is built for an id, and how outputs are listed, are functions
//! given in, so a test can use fakes.

use futures_util::future::BoxFuture;
use phonia_core::config::OutputSpec;
use phonia_core::output::SinkFactory;
use phonia_core::output::catalog::{self, Entry};
use phonia_ipc as ipc;
use std::sync::{Arc, Mutex};

/// Builds the sink factory for an output.
pub type Build = Arc<dyn Fn(&OutputSpec) -> Arc<dyn SinkFactory> + Send + Sync>;
/// Lists the outputs there are now.
pub type Lister = Arc<dyn Fn() -> BoxFuture<'static, Vec<Entry>> + Send + Sync>;

pub struct Outputs {
    current: Mutex<(OutputSpec, String)>,
    build: Build,
    lister: Lister,
}

impl Outputs {
    /// Playing on `initial`, which is what the daemon was started with.
    pub fn new(initial: OutputSpec, build: Build) -> Self {
        let description = initial.describe();
        Self {
            current: Mutex::new((initial, description)),
            build,
            lister: Arc::new(|| Box::pin(catalog::list())),
        }
    }

    /// Lists outputs with `lister` instead of asking the system.
    pub fn with_lister(mut self, lister: Lister) -> Self {
        self.lister = lister;
        self
    }

    pub async fn list(&self) -> Vec<Entry> {
        (self.lister)().await
    }

    /// Where the sound is going now.
    pub fn route(&self) -> ipc::Route {
        let (spec, description) = &*self.current.lock().unwrap();
        ipc::Route {
            id: spec.id(),
            mode: match spec {
                OutputSpec::Exclusive { .. } => ipc::OutputMode::Exclusive,
                OutputSpec::Shared { .. } => ipc::OutputMode::Shared,
            },
            description: description.clone(),
        }
    }

    /// The output an id names, and a factory for it.
    pub fn prepare(&self, id: &str) -> anyhow::Result<(OutputSpec, Arc<dyn SinkFactory>)> {
        let spec = OutputSpec::from_id(id)?;
        let factory = (self.build)(&spec);
        Ok((spec, factory))
    }

    /// Names the current output the way the list does (`Fosi Audio DS2`, not `device hw:DS2,0`).
    pub async fn refresh(&self) {
        let entries = self.list().await;
        let spec = self.current.lock().unwrap().0.clone();
        self.switched_to(spec, &entries);
    }

    /// The daemon plays on `spec` now. `entries` name it for people.
    pub fn switched_to(&self, spec: OutputSpec, entries: &[Entry]) {
        let description = catalog::describe(&spec, entries);
        *self.current.lock().unwrap() = (spec, description);
    }
}
