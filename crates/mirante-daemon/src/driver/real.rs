//! The production driver factory. The Claude and Codex drivers are wired in by Tasks 14
//! and 16; until then starting a session is a typed `Unsupported` error.
use std::sync::Arc;

use mirante_core::model::AgentKind;

use super::{Driver, DriverError, DriverFactory, Sink, SpawnRequest};

#[derive(Debug, Default, Clone)]
pub struct RealFactory;

impl DriverFactory for RealFactory {
    fn spawn(&self, req: SpawnRequest, _sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        match req.task.agent {
            AgentKind::Claude => Err(DriverError::Unsupported("the Claude driver is not built yet")),
            AgentKind::Codex => Err(DriverError::Unsupported("the Codex driver is not built yet")),
        }
    }
}
