#![doc = "A deterministic reservation, idempotency, and fair-waitlist engine."]

use std::sync::{Arc, Mutex, MutexGuard};
use thiserror::Error;

pub mod fixture;
pub mod model;
pub mod report;
pub mod server;
pub mod store;

pub use model::{
    Actor, AuditEntry, CancellationReceipt, CheckInReceipt, Engine, EngineConfig, EngineMetrics,
    EngineSnapshot, Event, EventPatch, EventUpdateReceipt, PersistedEngineState, Reservation,
    ReservationReceipt, ReservationStatus, Role, SlotForgeError, WaitlistPolicy,
};
pub use store::{Delivery, Notification, SqliteEngine, StoreError};

#[derive(Debug, Error)]
pub enum SharedEngineError {
    #[error(transparent)]
    Domain(#[from] SlotForgeError),
    #[error("shared engine lock is poisoned")]
    Poisoned,
}

/// A small synchronization boundary for callers that accept concurrent retries.
/// The domain engine remains deterministic; this wrapper only serializes mutations.
#[derive(Clone)]
pub struct SharedEngine {
    inner: Arc<Mutex<Engine>>,
}

impl SharedEngine {
    pub fn new(engine: Engine) -> Self {
        Self {
            inner: Arc::new(Mutex::new(engine)),
        }
    }

    pub fn reserve(
        &self,
        actor: &Actor,
        request_id: &str,
        event_id: &str,
        cohort: &str,
        at: i64,
    ) -> Result<ReservationReceipt, SharedEngineError> {
        let mut engine = self.lock()?;
        Ok(engine.reserve(actor, request_id, event_id, cohort, at)?)
    }

    pub fn cancel(
        &self,
        actor: &Actor,
        request_id: &str,
        reservation_id: u64,
        at: i64,
    ) -> Result<CancellationReceipt, SharedEngineError> {
        let mut engine = self.lock()?;
        Ok(engine.cancel(actor, request_id, reservation_id, at)?)
    }

    pub fn check_in(
        &self,
        actor: &Actor,
        request_id: &str,
        reservation_id: u64,
        at: i64,
    ) -> Result<CheckInReceipt, SharedEngineError> {
        let mut engine = self.lock()?;
        Ok(engine.check_in(actor, request_id, reservation_id, at)?)
    }

    pub fn snapshot(&self) -> Result<EngineSnapshot, SharedEngineError> {
        Ok(self.lock()?.snapshot())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Engine>, SharedEngineError> {
        self.inner.lock().map_err(|_| SharedEngineError::Poisoned)
    }
}
