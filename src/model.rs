use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const MAX_ID_LEN: usize = 64;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SlotForgeError {
    #[error("invalid {field}: must be 1..={max} safe identifier characters")]
    InvalidId { field: &'static str, max: usize },
    #[error("event {0} is unknown")]
    UnknownEvent(String),
    #[error("event {0} is inactive")]
    InactiveEvent(String),
    #[error("event {0} is closed at {1}")]
    EventClosed(String, i64),
    #[error("event {0} has invalid time window")]
    InvalidEventWindow(String),
    #[error("event {0} must have positive capacity")]
    InvalidCapacity(String),
    #[error("event {event_id} capacity {requested} is below occupied count {occupied}")]
    CapacityBelowOccupied {
        event_id: String,
        requested: u32,
        occupied: usize,
    },
    #[error("event {event_id} revision conflict: expected {expected}, current {current}")]
    RevisionConflict {
        event_id: String,
        expected: u64,
        current: u64,
    },
    #[error("attendee {0} already has an active reservation for event {1}")]
    DuplicateReservation(String, String),
    #[error("attendee {0} reached the active reservation limit of {1}")]
    AttendeeLimit(String, usize),
    #[error("reservation {0} is unknown")]
    UnknownReservation(u64),
    #[error("actor {actor} cannot perform {action}")]
    Forbidden { actor: String, action: &'static str },
    #[error("actor {actor} does not own reservation {reservation}")]
    NotOwner { actor: String, reservation: u64 },
    #[error("reservation {0} cannot transition from {1:?} to {2:?}")]
    InvalidTransition(u64, ReservationStatus, ReservationStatus),
    #[error("request id {0} was reused with different parameters")]
    IdempotencyConflict(String),
    #[error("request id must be a non-empty safe identifier")]
    InvalidRequestId,
    #[error("timestamp must be positive")]
    InvalidTimestamp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub event_id: String,
    pub title: String,
    pub capacity: u32,
    pub starts_at: i64,
    pub ends_at: i64,
    pub booking_close_at: i64,
    pub active: bool,
    /// Monotonic aggregate version used by HTTP If-Match edits.
    pub revision: u64,
}

impl Event {
    pub fn new(
        event_id: impl Into<String>,
        title: impl Into<String>,
        capacity: u32,
        starts_at: i64,
        ends_at: i64,
    ) -> Result<Self, SlotForgeError> {
        Self::with_booking_close(event_id, title, capacity, starts_at, ends_at, starts_at)
    }

    pub fn with_booking_close(
        event_id: impl Into<String>,
        title: impl Into<String>,
        capacity: u32,
        starts_at: i64,
        ends_at: i64,
        booking_close_at: i64,
    ) -> Result<Self, SlotForgeError> {
        let event_id = event_id.into();
        validate_id(&event_id, "event_id")?;
        let title = title.into();
        if title.trim().is_empty() {
            return Err(SlotForgeError::InvalidId {
                field: "title",
                max: MAX_ID_LEN,
            });
        }
        if capacity == 0 {
            return Err(SlotForgeError::InvalidCapacity(event_id));
        }
        if starts_at < 1
            || ends_at <= starts_at
            || booking_close_at < 1
            || booking_close_at > starts_at
        {
            return Err(SlotForgeError::InvalidEventWindow(event_id));
        }
        Ok(Self {
            event_id,
            title,
            capacity,
            starts_at,
            ends_at,
            booking_close_at,
            active: true,
            revision: 1,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    Confirmed,
    Waitlisted,
    CheckedIn,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Attendee,
    Coordinator,
    Observer,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actor {
    pub actor_id: String,
    pub role: Role,
}

impl Actor {
    pub fn attendee(actor_id: impl Into<String>) -> Result<Self, SlotForgeError> {
        Self::new(actor_id, Role::Attendee)
    }

    pub fn coordinator(actor_id: impl Into<String>) -> Result<Self, SlotForgeError> {
        Self::new(actor_id, Role::Coordinator)
    }

    pub fn observer(actor_id: impl Into<String>) -> Result<Self, SlotForgeError> {
        Self::new(actor_id, Role::Observer)
    }

    fn new(actor_id: impl Into<String>, role: Role) -> Result<Self, SlotForgeError> {
        let actor_id = actor_id.into();
        validate_id(&actor_id, "actor_id")?;
        Ok(Self { actor_id, role })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reservation {
    pub reservation_id: u64,
    pub event_id: String,
    pub attendee_id: String,
    pub cohort: String,
    pub request_id: String,
    pub queue_seq: u64,
    pub requested_at: i64,
    pub confirmed_at: Option<i64>,
    pub wait_seconds: Option<i64>,
    pub checked_in_at: Option<i64>,
    pub status: ReservationStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReservationReceipt {
    pub reservation_id: u64,
    pub event_id: String,
    pub status: ReservationStatus,
    pub waitlist_position: Option<usize>,
    pub replayed: bool,
    pub event_revision: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventPatch {
    pub title: Option<String>,
    pub capacity: Option<u32>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub booking_close_at: Option<i64>,
    pub active: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventUpdateReceipt {
    pub event: Event,
    pub promoted: Vec<u64>,
    pub replayed: bool,
}

impl ReservationReceipt {
    fn replayed(&self) -> Self {
        let mut receipt = self.clone();
        receipt.replayed = true;
        receipt
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancellationReceipt {
    pub reservation_id: u64,
    pub promoted: Vec<u64>,
    pub replayed: bool,
}

impl CancellationReceipt {
    fn replayed(&self) -> Self {
        let mut receipt = self.clone();
        receipt.replayed = true;
        receipt
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckInReceipt {
    pub reservation_id: u64,
    pub checked_in_at: i64,
    pub replayed: bool,
}

impl CheckInReceipt {
    fn replayed(&self) -> Self {
        let mut receipt = self.clone();
        receipt.replayed = true;
        receipt
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitlistPolicy {
    Fifo,
    Fair,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineConfig {
    pub max_active_per_attendee: usize,
    pub policy: WaitlistPolicy,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_active_per_attendee: 3,
            policy: WaitlistPolicy::Fair,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    pub sequence: u64,
    pub at: i64,
    pub action: String,
    pub actor_id: String,
    pub request_id: String,
    pub reservation_id: Option<u64>,
    pub event_id: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EngineMetrics {
    pub events: usize,
    pub reservations_total: usize,
    pub confirmed: usize,
    pub waitlisted: usize,
    pub checked_in: usize,
    pub cancelled: usize,
    pub promotions: usize,
    pub idempotent_replays: usize,
    pub capacity_slots: u64,
    pub occupied_slots: u64,
    pub utilization: f64,
    pub waitlist_rate: f64,
    pub fairness_index: f64,
    pub attendee_fairness_index: f64,
    pub max_wait_seconds: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EngineSnapshot {
    pub schema_version: u32,
    pub config: EngineConfig,
    pub events: Vec<Event>,
    pub reservations: Vec<Reservation>,
    pub audit: Vec<AuditEntry>,
    pub metrics: EngineMetrics,
}

/// The complete serialized state required to make an idempotent command after
/// a restart behave exactly as it did before. `EngineSnapshot` remains a
/// display/report artifact and deliberately does not substitute for this.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedEngineState {
    pub schema_version: u32,
    pub config: EngineConfig,
    pub events: BTreeMap<String, Event>,
    pub reservations: BTreeMap<u64, Reservation>,
    pub requests: BTreeMap<String, RequestRecord>,
    pub audit: Vec<AuditEntry>,
    pub next_reservation_id: u64,
    pub next_queue_seq: u64,
    pub next_audit_seq: u64,
    pub attendee_grants: BTreeMap<String, u64>,
    pub cohort_grants: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestRecord {
    fingerprint: String,
    outcome: RequestOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RequestOutcome {
    Reservation(ReservationReceipt),
    Cancellation(CancellationReceipt),
    CheckIn(CheckInReceipt),
}

#[derive(Clone, Debug)]
pub struct Engine {
    config: EngineConfig,
    events: BTreeMap<String, Event>,
    reservations: BTreeMap<u64, Reservation>,
    requests: BTreeMap<String, RequestRecord>,
    audit: Vec<AuditEntry>,
    next_reservation_id: u64,
    next_queue_seq: u64,
    next_audit_seq: u64,
    attendee_grants: BTreeMap<String, u64>,
    cohort_grants: BTreeMap<String, u64>,
}

impl Engine {
    pub fn new<I>(events: I, config: EngineConfig) -> Result<Self, SlotForgeError>
    where
        I: IntoIterator<Item = Event>,
    {
        if config.max_active_per_attendee == 0 {
            return Err(SlotForgeError::AttendeeLimit("config".to_string(), 0));
        }
        let mut event_map = BTreeMap::new();
        for event in events {
            if event.capacity == 0
                || event.starts_at < 1
                || event.ends_at <= event.starts_at
                || event.booking_close_at < 1
                || event.booking_close_at > event.starts_at
            {
                return Err(SlotForgeError::InvalidEventWindow(event.event_id));
            }
            validate_id(&event.event_id, "event_id")?;
            if event_map.insert(event.event_id.clone(), event).is_some() {
                return Err(SlotForgeError::InvalidId {
                    field: "duplicate event_id",
                    max: MAX_ID_LEN,
                });
            }
        }
        if event_map.is_empty() {
            return Err(SlotForgeError::InvalidId {
                field: "events",
                max: MAX_ID_LEN,
            });
        }
        Ok(Self {
            config,
            events: event_map,
            reservations: BTreeMap::new(),
            requests: BTreeMap::new(),
            audit: Vec::new(),
            next_reservation_id: 1,
            next_queue_seq: 1,
            next_audit_seq: 1,
            attendee_grants: BTreeMap::new(),
            cohort_grants: BTreeMap::new(),
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.events.values()
    }

    pub fn reservations(&self) -> impl Iterator<Item = &Reservation> {
        self.reservations.values()
    }

    pub fn audit(&self) -> &[AuditEntry] {
        &self.audit
    }

    pub fn reservation(&self, reservation_id: u64) -> Option<&Reservation> {
        self.reservations.get(&reservation_id)
    }

    pub fn add_event(
        &mut self,
        actor: &Actor,
        request_id: &str,
        event: Event,
        at: i64,
    ) -> Result<(), SlotForgeError> {
        self.require_role(actor, Role::Coordinator, "create_event")?;
        validate_request_id(request_id)?;
        validate_timestamp(at)?;
        if self.events.contains_key(&event.event_id) {
            return Err(SlotForgeError::InvalidId {
                field: "duplicate event_id",
                max: MAX_ID_LEN,
            });
        }
        let event_id = event.event_id.clone();
        self.events.insert(event_id.clone(), event);
        self.append_audit(
            at,
            "event_created",
            actor,
            request_id,
            None,
            Some(&event_id),
            "created",
        );
        Ok(())
    }

    pub fn persisted_state(&self) -> PersistedEngineState {
        PersistedEngineState {
            schema_version: 1,
            config: self.config.clone(),
            events: self.events.clone(),
            reservations: self.reservations.clone(),
            requests: self.requests.clone(),
            audit: self.audit.clone(),
            next_reservation_id: self.next_reservation_id,
            next_queue_seq: self.next_queue_seq,
            next_audit_seq: self.next_audit_seq,
            attendee_grants: self.attendee_grants.clone(),
            cohort_grants: self.cohort_grants.clone(),
        }
    }

    pub fn from_persisted_state(state: PersistedEngineState) -> Result<Self, SlotForgeError> {
        if state.schema_version != 1
            || state.next_reservation_id == 0
            || state.next_queue_seq == 0
            || state.next_audit_seq == 0
        {
            return Err(SlotForgeError::InvalidEventWindow(
                "persisted_state".to_string(),
            ));
        }
        let engine = Self {
            config: state.config,
            events: state.events,
            reservations: state.reservations,
            requests: state.requests,
            audit: state.audit,
            next_reservation_id: state.next_reservation_id,
            next_queue_seq: state.next_queue_seq,
            next_audit_seq: state.next_audit_seq,
            attendee_grants: state.attendee_grants,
            cohort_grants: state.cohort_grants,
        };
        // Reuse constructor-level structural checks without discarding state.
        if engine.config.max_active_per_attendee == 0 || engine.events.is_empty() {
            return Err(SlotForgeError::InvalidEventWindow(
                "persisted_state".to_string(),
            ));
        }
        for event in engine.events.values() {
            if event.capacity == 0
                || event.starts_at < 1
                || event.ends_at <= event.starts_at
                || event.booking_close_at > event.starts_at
            {
                return Err(SlotForgeError::InvalidEventWindow(event.event_id.clone()));
            }
            if engine.occupied_for(&event.event_id) > event.capacity as usize {
                return Err(SlotForgeError::InvalidCapacity(event.event_id.clone()));
            }
        }
        Ok(engine)
    }

    pub fn reserve(
        &mut self,
        actor: &Actor,
        request_id: &str,
        event_id: &str,
        cohort: &str,
        at: i64,
    ) -> Result<ReservationReceipt, SlotForgeError> {
        self.require_role(actor, Role::Attendee, "reserve")?;
        validate_request_id(request_id)?;
        validate_id(event_id, "event_id")?;
        validate_id(cohort, "cohort")?;
        validate_timestamp(at)?;
        let fingerprint = format!("reserve|{}|{}|{}", actor.actor_id, event_id, cohort);
        if let Some(record) = self.requests.get(request_id).cloned() {
            if record.fingerprint != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_id.to_string()));
            }
            if let RequestOutcome::Reservation(receipt) = record.outcome {
                self.append_audit(
                    at,
                    "idempotent_replay",
                    actor,
                    request_id,
                    Some(receipt.reservation_id),
                    Some(event_id),
                    "same_fingerprint",
                );
                return Ok(receipt.replayed());
            }
            return Err(SlotForgeError::IdempotencyConflict(request_id.to_string()));
        }
        let event = self
            .events
            .get(event_id)
            .ok_or_else(|| SlotForgeError::UnknownEvent(event_id.to_string()))?;
        if !event.active {
            return Err(SlotForgeError::InactiveEvent(event_id.to_string()));
        }
        if at >= event.booking_close_at {
            return Err(SlotForgeError::EventClosed(
                event_id.to_string(),
                event.booking_close_at,
            ));
        }
        let event_revision = event.revision;
        let event_capacity = event.capacity;
        if self.active_count_for(&actor.actor_id) >= self.config.max_active_per_attendee {
            return Err(SlotForgeError::AttendeeLimit(
                actor.actor_id.clone(),
                self.config.max_active_per_attendee,
            ));
        }
        if self.has_active_reservation(&actor.actor_id, event_id) {
            return Err(SlotForgeError::DuplicateReservation(
                actor.actor_id.clone(),
                event_id.to_string(),
            ));
        }

        let reservation_id = self.next_reservation_id;
        self.next_reservation_id += 1;
        let queue_seq = self.next_queue_seq;
        self.next_queue_seq += 1;
        let confirmed = self.occupied_for(event_id) < event_capacity as usize;
        let status = if confirmed {
            ReservationStatus::Confirmed
        } else {
            ReservationStatus::Waitlisted
        };
        let reservation = Reservation {
            reservation_id,
            event_id: event_id.to_string(),
            attendee_id: actor.actor_id.clone(),
            cohort: cohort.to_string(),
            request_id: request_id.to_string(),
            queue_seq,
            requested_at: at,
            confirmed_at: confirmed.then_some(at),
            wait_seconds: confirmed.then_some(0),
            checked_in_at: None,
            status,
        };
        self.reservations.insert(reservation_id, reservation);
        self.attendee_grants
            .entry(actor.actor_id.clone())
            .or_default();
        self.cohort_grants.entry(cohort.to_string()).or_default();
        if confirmed {
            self.record_grant(&actor.actor_id, cohort);
        }
        let receipt = ReservationReceipt {
            reservation_id,
            event_id: event_id.to_string(),
            status,
            waitlist_position: if confirmed {
                None
            } else {
                Some(self.waitlist_position(reservation_id).unwrap_or(0))
            },
            replayed: false,
            event_revision,
        };
        self.requests.insert(
            request_id.to_string(),
            RequestRecord {
                fingerprint,
                outcome: RequestOutcome::Reservation(receipt.clone()),
            },
        );
        self.append_audit(
            at,
            "reserve",
            actor,
            request_id,
            Some(reservation_id),
            Some(event_id),
            if confirmed { "confirmed" } else { "waitlisted" },
        );
        Ok(receipt)
    }

    pub fn cancel(
        &mut self,
        actor: &Actor,
        request_id: &str,
        reservation_id: u64,
        at: i64,
    ) -> Result<CancellationReceipt, SlotForgeError> {
        self.require_cancel_role(actor)?;
        validate_request_id(request_id)?;
        validate_timestamp(at)?;
        let fingerprint = format!("cancel|{}|{}", actor.actor_id, reservation_id);
        if let Some(record) = self.requests.get(request_id).cloned() {
            if record.fingerprint != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_id.to_string()));
            }
            if let RequestOutcome::Cancellation(receipt) = record.outcome {
                self.append_audit(
                    at,
                    "idempotent_replay",
                    actor,
                    request_id,
                    Some(receipt.reservation_id),
                    None,
                    "same_fingerprint",
                );
                return Ok(receipt.replayed());
            }
            return Err(SlotForgeError::IdempotencyConflict(request_id.to_string()));
        }
        let reservation = self
            .reservations
            .get(&reservation_id)
            .ok_or(SlotForgeError::UnknownReservation(reservation_id))?;
        if actor.role == Role::Attendee && reservation.attendee_id != actor.actor_id {
            return Err(SlotForgeError::NotOwner {
                actor: actor.actor_id.clone(),
                reservation: reservation_id,
            });
        }
        if reservation.status == ReservationStatus::Cancelled {
            return Err(SlotForgeError::InvalidTransition(
                reservation_id,
                ReservationStatus::Cancelled,
                ReservationStatus::Cancelled,
            ));
        }
        if reservation.status == ReservationStatus::CheckedIn {
            return Err(SlotForgeError::InvalidTransition(
                reservation_id,
                ReservationStatus::CheckedIn,
                ReservationStatus::Cancelled,
            ));
        }
        let event_id = reservation.event_id.clone();
        let was_confirmed = reservation.status == ReservationStatus::Confirmed;
        let event = self
            .events
            .get(&event_id)
            .ok_or_else(|| SlotForgeError::UnknownEvent(event_id.clone()))?;
        if at >= event.starts_at {
            return Err(SlotForgeError::EventClosed(event_id, event.starts_at));
        }
        if let Some(item) = self.reservations.get_mut(&reservation_id) {
            item.status = ReservationStatus::Cancelled;
        }
        let mut promoted = Vec::new();
        if was_confirmed && at < event.booking_close_at {
            promoted = self.promote_waitlist(&event_id, at, actor, request_id);
        }
        let receipt = CancellationReceipt {
            reservation_id,
            promoted,
            replayed: false,
        };
        self.requests.insert(
            request_id.to_string(),
            RequestRecord {
                fingerprint,
                outcome: RequestOutcome::Cancellation(receipt.clone()),
            },
        );
        self.append_audit(
            at,
            "cancel",
            actor,
            request_id,
            Some(reservation_id),
            Some(&event_id),
            if was_confirmed {
                "released_and_promoted"
            } else {
                "removed_from_waitlist"
            },
        );
        Ok(receipt)
    }

    pub fn check_in(
        &mut self,
        actor: &Actor,
        request_id: &str,
        reservation_id: u64,
        at: i64,
    ) -> Result<CheckInReceipt, SlotForgeError> {
        self.require_role(actor, Role::Coordinator, "check_in")?;
        validate_request_id(request_id)?;
        validate_timestamp(at)?;
        let fingerprint = format!("check_in|{}|{}", actor.actor_id, reservation_id);
        if let Some(record) = self.requests.get(request_id).cloned() {
            if record.fingerprint != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_id.to_string()));
            }
            if let RequestOutcome::CheckIn(receipt) = record.outcome {
                self.append_audit(
                    at,
                    "idempotent_replay",
                    actor,
                    request_id,
                    Some(receipt.reservation_id),
                    None,
                    "same_fingerprint",
                );
                return Ok(receipt.replayed());
            }
            return Err(SlotForgeError::IdempotencyConflict(request_id.to_string()));
        }
        let reservation = self
            .reservations
            .get(&reservation_id)
            .ok_or(SlotForgeError::UnknownReservation(reservation_id))?;
        let event_id = reservation.event_id.clone();
        let reservation_status = reservation.status;
        let event = self
            .events
            .get(&event_id)
            .ok_or_else(|| SlotForgeError::UnknownEvent(event_id.clone()))?;
        if at < event.starts_at - 30 * 60 || at > event.ends_at {
            return Err(SlotForgeError::EventClosed(
                event.event_id.clone(),
                event.ends_at,
            ));
        }
        if reservation_status != ReservationStatus::Confirmed {
            return Err(SlotForgeError::InvalidTransition(
                reservation_id,
                reservation_status,
                ReservationStatus::CheckedIn,
            ));
        }
        if let Some(item) = self.reservations.get_mut(&reservation_id) {
            item.status = ReservationStatus::CheckedIn;
            item.checked_in_at = Some(at);
        }
        let receipt = CheckInReceipt {
            reservation_id,
            checked_in_at: at,
            replayed: false,
        };
        self.requests.insert(
            request_id.to_string(),
            RequestRecord {
                fingerprint,
                outcome: RequestOutcome::CheckIn(receipt.clone()),
            },
        );
        self.append_audit(
            at,
            "check_in",
            actor,
            request_id,
            Some(reservation_id),
            Some(&event_id),
            "checked_in",
        );
        Ok(receipt)
    }

    pub fn set_capacity(
        &mut self,
        actor: &Actor,
        request_id: &str,
        event_id: &str,
        capacity: u32,
        at: i64,
    ) -> Result<Vec<u64>, SlotForgeError> {
        self.require_role(actor, Role::Coordinator, "set_capacity")?;
        validate_request_id(request_id)?;
        validate_id(event_id, "event_id")?;
        validate_timestamp(at)?;
        let expected = self
            .events
            .get(event_id)
            .ok_or_else(|| SlotForgeError::UnknownEvent(event_id.to_string()))?
            .revision;
        Ok(self
            .update_event(
                actor,
                request_id,
                event_id,
                expected,
                EventPatch {
                    capacity: Some(capacity),
                    ..EventPatch::default()
                },
                at,
            )?
            .promoted)
    }

    pub fn update_event(
        &mut self,
        actor: &Actor,
        request_id: &str,
        event_id: &str,
        expected_revision: u64,
        patch: EventPatch,
        at: i64,
    ) -> Result<EventUpdateReceipt, SlotForgeError> {
        self.require_role(actor, Role::Coordinator, "update_event")?;
        validate_request_id(request_id)?;
        validate_id(event_id, "event_id")?;
        validate_timestamp(at)?;
        let current = self
            .events
            .get(event_id)
            .cloned()
            .ok_or_else(|| SlotForgeError::UnknownEvent(event_id.to_string()))?;
        if current.revision != expected_revision {
            return Err(SlotForgeError::RevisionConflict {
                event_id: event_id.to_string(),
                expected: expected_revision,
                current: current.revision,
            });
        }
        let mut updated = current.clone();
        if let Some(title) = patch.title {
            if title.trim().is_empty() {
                return Err(SlotForgeError::InvalidId {
                    field: "title",
                    max: MAX_ID_LEN,
                });
            }
            updated.title = title;
        }
        if let Some(capacity) = patch.capacity {
            if capacity == 0 {
                return Err(SlotForgeError::InvalidCapacity(event_id.to_string()));
            }
            let occupied = self.occupied_for(event_id);
            if (capacity as usize) < occupied {
                return Err(SlotForgeError::CapacityBelowOccupied {
                    event_id: event_id.to_string(),
                    requested: capacity,
                    occupied,
                });
            }
            updated.capacity = capacity;
        }
        if let Some(starts_at) = patch.starts_at {
            updated.starts_at = starts_at;
        }
        if let Some(ends_at) = patch.ends_at {
            updated.ends_at = ends_at;
        }
        if let Some(booking_close_at) = patch.booking_close_at {
            updated.booking_close_at = booking_close_at;
        }
        if let Some(active) = patch.active {
            updated.active = active;
        }
        if updated.starts_at < 1
            || updated.ends_at <= updated.starts_at
            || updated.booking_close_at < 1
            || updated.booking_close_at > updated.starts_at
        {
            return Err(SlotForgeError::InvalidEventWindow(event_id.to_string()));
        }
        updated.revision = current.revision + 1;
        let promoted = if at < updated.booking_close_at && updated.active {
            self.events.insert(event_id.to_string(), updated.clone());
            self.promote_waitlist(event_id, at, actor, request_id)
        } else {
            self.events.insert(event_id.to_string(), updated.clone());
            Vec::new()
        };
        self.append_audit(
            at,
            "event_updated",
            actor,
            request_id,
            None,
            Some(event_id),
            &format!("revision={};promoted={}", updated.revision, promoted.len()),
        );
        Ok(EventUpdateReceipt {
            event: updated,
            promoted,
            replayed: false,
        })
    }

    pub fn waitlist(&self, event_id: &str) -> Vec<&Reservation> {
        let mut result: Vec<_> = self
            .reservations
            .values()
            .filter(|reservation| {
                reservation.event_id == event_id
                    && reservation.status == ReservationStatus::Waitlisted
            })
            .collect();
        result.sort_by_key(|reservation| self.waitlist_key(reservation));
        result
    }

    pub fn metrics(&self) -> EngineMetrics {
        let mut confirmed = 0;
        let mut waitlisted = 0;
        let mut checked_in = 0;
        let mut cancelled = 0;
        let mut max_wait_seconds = 0;
        for reservation in self.reservations.values() {
            match reservation.status {
                ReservationStatus::Confirmed => confirmed += 1,
                ReservationStatus::Waitlisted => waitlisted += 1,
                ReservationStatus::CheckedIn => checked_in += 1,
                ReservationStatus::Cancelled => cancelled += 1,
            }
            if let Some(wait) = reservation.wait_seconds {
                max_wait_seconds = max_wait_seconds.max(wait);
            }
        }
        let capacity_slots: u64 = self
            .events
            .values()
            .map(|event| event.capacity as u64)
            .sum();
        let occupied_slots = (confirmed + checked_in) as u64;
        let active_total = confirmed + waitlisted + checked_in;
        let promotions = self
            .audit
            .iter()
            .filter(|entry| entry.action == "promote")
            .count();
        let idempotent_replays = self
            .audit
            .iter()
            .filter(|entry| entry.action == "idempotent_replay")
            .count();
        EngineMetrics {
            events: self.events.len(),
            reservations_total: self.reservations.len(),
            confirmed,
            waitlisted,
            checked_in,
            cancelled,
            promotions,
            idempotent_replays,
            capacity_slots,
            occupied_slots,
            utilization: ratio(occupied_slots as f64, capacity_slots as f64),
            waitlist_rate: ratio(waitlisted as f64, active_total as f64),
            fairness_index: jain_index(self.cohort_grants.values().copied()),
            attendee_fairness_index: jain_index(self.attendee_grants.values().copied()),
            max_wait_seconds,
        }
    }

    pub fn snapshot(&self) -> EngineSnapshot {
        EngineSnapshot {
            schema_version: 1,
            config: self.config.clone(),
            events: self.events.values().cloned().collect(),
            reservations: self.reservations.values().cloned().collect(),
            audit: self.audit.clone(),
            metrics: self.metrics(),
        }
    }

    fn require_role(
        &self,
        actor: &Actor,
        expected: Role,
        action: &'static str,
    ) -> Result<(), SlotForgeError> {
        if actor.role == expected {
            Ok(())
        } else {
            Err(SlotForgeError::Forbidden {
                actor: actor.actor_id.clone(),
                action,
            })
        }
    }

    fn require_cancel_role(&self, actor: &Actor) -> Result<(), SlotForgeError> {
        match actor.role {
            Role::Attendee | Role::Coordinator => Ok(()),
            Role::Observer => Err(SlotForgeError::Forbidden {
                actor: actor.actor_id.clone(),
                action: "cancel",
            }),
        }
    }

    fn active_count_for(&self, attendee_id: &str) -> usize {
        self.reservations
            .values()
            .filter(|reservation| {
                reservation.attendee_id == attendee_id
                    && matches!(
                        reservation.status,
                        ReservationStatus::Confirmed
                            | ReservationStatus::Waitlisted
                            | ReservationStatus::CheckedIn
                    )
            })
            .count()
    }

    fn has_active_reservation(&self, attendee_id: &str, event_id: &str) -> bool {
        self.reservations.values().any(|reservation| {
            reservation.attendee_id == attendee_id
                && reservation.event_id == event_id
                && reservation.status != ReservationStatus::Cancelled
        })
    }

    fn occupied_for(&self, event_id: &str) -> usize {
        self.reservations
            .values()
            .filter(|reservation| {
                reservation.event_id == event_id
                    && matches!(
                        reservation.status,
                        ReservationStatus::Confirmed | ReservationStatus::CheckedIn
                    )
            })
            .count()
    }

    fn waitlist_position(&self, reservation_id: u64) -> Option<usize> {
        self.waitlist(self.reservations.get(&reservation_id)?.event_id.as_str())
            .iter()
            .position(|reservation| reservation.reservation_id == reservation_id)
            .map(|position| position + 1)
    }

    fn waitlist_key(&self, reservation: &Reservation) -> (u64, u64, u64, u64) {
        match self.config.policy {
            WaitlistPolicy::Fifo => (0, 0, reservation.queue_seq, reservation.reservation_id),
            WaitlistPolicy::Fair => (
                self.cohort_grants
                    .get(&reservation.cohort)
                    .copied()
                    .unwrap_or(0),
                self.attendee_grants
                    .get(&reservation.attendee_id)
                    .copied()
                    .unwrap_or(0),
                reservation.queue_seq,
                reservation.reservation_id,
            ),
        }
    }

    fn promote_waitlist(
        &mut self,
        event_id: &str,
        at: i64,
        actor: &Actor,
        request_id: &str,
    ) -> Vec<u64> {
        let capacity = self
            .events
            .get(event_id)
            .map(|event| event.capacity)
            .unwrap_or(0) as usize;
        let mut promoted = Vec::new();
        while self.occupied_for(event_id) < capacity {
            let next_id = self
                .waitlist(event_id)
                .first()
                .map(|reservation| reservation.reservation_id);
            let Some(next_id) = next_id else { break };
            let (attendee_id, cohort, queue_seq) = {
                let reservation = self.reservations.get(&next_id).expect("waitlist id exists");
                (
                    reservation.attendee_id.clone(),
                    reservation.cohort.clone(),
                    reservation.queue_seq,
                )
            };
            if let Some(reservation) = self.reservations.get_mut(&next_id) {
                reservation.status = ReservationStatus::Confirmed;
                reservation.confirmed_at = Some(at);
                reservation.wait_seconds = Some((at - reservation.requested_at).max(0));
            }
            self.record_grant(&attendee_id, &cohort);
            promoted.push(next_id);
            self.append_audit(
                at,
                "promote",
                actor,
                request_id,
                Some(next_id),
                Some(event_id),
                &format!("queue_seq={queue_seq}"),
            );
        }
        promoted
    }

    fn record_grant(&mut self, attendee_id: &str, cohort: &str) {
        *self
            .attendee_grants
            .entry(attendee_id.to_string())
            .or_default() += 1;
        *self.cohort_grants.entry(cohort.to_string()).or_default() += 1;
    }

    // Audit entries deliberately mirror the public transition receipt fields;
    // keeping this helper explicit makes each mutation call site self-auditing.
    #[allow(clippy::too_many_arguments)]
    fn append_audit(
        &mut self,
        at: i64,
        action: &str,
        actor: &Actor,
        request_id: &str,
        reservation_id: Option<u64>,
        event_id: Option<&str>,
        detail: &str,
    ) {
        self.audit.push(AuditEntry {
            sequence: self.next_audit_seq,
            at,
            action: action.to_string(),
            actor_id: actor.actor_id.clone(),
            request_id: request_id.to_string(),
            reservation_id,
            event_id: event_id.map(str::to_string),
            detail: detail.to_string(),
        });
        self.next_audit_seq += 1;
    }
}

pub fn validate_id(value: &str, field: &'static str) -> Result<(), SlotForgeError> {
    if value.is_empty()
        || value.len() > MAX_ID_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(SlotForgeError::InvalidId {
            field,
            max: MAX_ID_LEN,
        });
    }
    Ok(())
}

fn validate_request_id(request_id: &str) -> Result<(), SlotForgeError> {
    validate_id(request_id, "request_id").map_err(|_| SlotForgeError::InvalidRequestId)
}

fn validate_timestamp(timestamp: i64) -> Result<(), SlotForgeError> {
    if timestamp < 1 {
        Err(SlotForgeError::InvalidTimestamp)
    } else {
        Ok(())
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator == 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

fn jain_index<I>(values: I) -> f64
where
    I: IntoIterator<Item = u64>,
{
    let values: Vec<f64> = values.into_iter().map(|value| value as f64).collect();
    if values.is_empty() {
        return 1.0;
    }
    let sum: f64 = values.iter().sum();
    let squares: f64 = values.iter().map(|value| value * value).sum();
    if squares == 0.0 {
        1.0
    } else {
        (sum * sum) / (values.len() as f64 * squares)
    }
}

#[allow(dead_code)]
fn _active_ids(reservations: &BTreeMap<u64, Reservation>) -> BTreeSet<u64> {
    reservations
        .values()
        .filter(|reservation| reservation.status != ReservationStatus::Cancelled)
        .map(|reservation| reservation.reservation_id)
        .collect()
}
