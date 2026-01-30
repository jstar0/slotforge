use crate::{
    Actor, CancellationReceipt, CheckInReceipt, Engine, Event, EventPatch, EventUpdateReceipt,
    ReservationReceipt, SlotForgeError,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Domain(#[from] SlotForgeError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("stored command payload is invalid")]
    InvalidCommandPayload,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Notification {
    pub notification_id: i64,
    /// Activity identifier from the committed outbox payload.
    pub event_id: String,
    /// Unique outbox message identifier used for delivery deduplication.
    pub delivery_event_id: String,
    pub body: String,
    pub received_at: i64,
    pub read_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Delivery {
    pub event_id: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub recipient_id: String,
    pub attempts: i64,
    pub status: String,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub delivered_at: Option<i64>,
}

/// SQLite-backed command boundary. It deliberately owns both the committed
/// engine and its connection: callers mutate a cloned engine, commit state and
/// command receipt in one transaction, then replace memory only after commit.
pub struct SqliteEngine {
    connection: Connection,
    engine: Engine,
}

impl SqliteEngine {
    pub fn open(path: &Path, initial: Engine) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS engine_state (
                 id INTEGER PRIMARY KEY CHECK(id=1), schema_version INTEGER NOT NULL, revision INTEGER NOT NULL, payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS commands (
                 actor_id TEXT NOT NULL, request_key TEXT NOT NULL, fingerprint TEXT NOT NULL, assigned_at INTEGER NOT NULL, response TEXT NOT NULL,
                 PRIMARY KEY(actor_id, request_key)
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 session_id_hash TEXT PRIMARY KEY, actor_id TEXT NOT NULL, expires_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS outbox (
                 event_id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, aggregate_version INTEGER NOT NULL,
                 type TEXT NOT NULL, recipient_id TEXT NOT NULL, payload TEXT NOT NULL, payload_sha256 TEXT NOT NULL,
                 occurred_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                 status TEXT NOT NULL CHECK(status IN ('pending','processing','retrying','delivered','dead_letter')),
                 lease_until INTEGER, next_attempt_at INTEGER NOT NULL, last_error TEXT, delivered_at INTEGER
             );
             CREATE INDEX IF NOT EXISTS outbox_ready_idx ON outbox(status, next_attempt_at);
             CREATE TABLE IF NOT EXISTS inbox (
                 notification_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE,
                 recipient_id TEXT NOT NULL, body TEXT NOT NULL, received_at INTEGER NOT NULL,
                 read_at INTEGER, payload_sha256 TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS relay_faults (
                 mode TEXT PRIMARY KEY CHECK(mode IN ('before_inbox','after_inbox_before_ack')),
                 remaining INTEGER NOT NULL CHECK(remaining >= 0)
             );",
        )?;
        let persisted: Option<String> = connection
            .query_row("SELECT payload FROM engine_state WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()?;
        let engine = match persisted {
            Some(payload) => Engine::from_persisted_state(serde_json::from_str(&payload)?)?,
            None => {
                let payload = serde_json::to_string(&initial.persisted_state())?;
                connection.execute(
                    "INSERT INTO engine_state(id, schema_version, revision, payload) VALUES (1, 1, 1, ?1)",
                    params![payload],
                )?;
                initial
            }
        };
        backfill_outbox(&connection, &engine)?;
        Ok(Self { connection, engine })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn save_session(
        &self,
        session_hash: &str,
        actor_id: &str,
        expires_at: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO sessions(session_id_hash, actor_id, expires_at) VALUES (?1, ?2, ?3) ON CONFLICT(session_id_hash) DO UPDATE SET actor_id=excluded.actor_id, expires_at=excluded.expires_at",
            params![session_hash, actor_id, expires_at],
        )?;
        Ok(())
    }

    pub fn session_actor(&self, session_hash: &str, at: i64) -> Result<Option<String>, StoreError> {
        self.connection
            .execute("DELETE FROM sessions WHERE expires_at <= ?1", params![at])?;
        Ok(self
            .connection
            .query_row(
                "SELECT actor_id FROM sessions WHERE session_id_hash=?1 AND expires_at>?2",
                params![session_hash, at],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn notifications(&self, actor: &Actor) -> Result<Vec<Notification>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT inbox.notification_id, inbox.event_id, outbox.payload, inbox.body, inbox.received_at, inbox.read_at
             FROM inbox JOIN outbox ON outbox.event_id=inbox.event_id
             WHERE inbox.recipient_id=?1 ORDER BY inbox.received_at DESC, inbox.notification_id DESC",
        )?;
        let rows = statement.query_map(params![actor.actor_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(
                |(notification_id, delivery_event_id, payload, body, received_at, read_at)| {
                    let payload: Value = serde_json::from_str(&payload)?;
                    let event_id = payload
                        .get("event_id")
                        .and_then(Value::as_str)
                        .unwrap_or(&delivery_event_id)
                        .to_string();
                    Ok(Notification {
                        notification_id,
                        event_id,
                        delivery_event_id,
                        body,
                        received_at,
                        read_at,
                    })
                },
            )
            .collect()
    }

    pub fn mark_notification_read(
        &self,
        actor: &Actor,
        notification_id: i64,
        at: i64,
    ) -> Result<bool, StoreError> {
        let changed = self.connection.execute(
            "UPDATE inbox SET read_at=COALESCE(read_at, ?3)
             WHERE notification_id=?1 AND recipient_id=?2",
            params![notification_id, actor.actor_id, at],
        )?;
        Ok(changed == 1)
    }

    pub fn deliveries(&self) -> Result<Vec<Delivery>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT event_id, aggregate_id, type, recipient_id, attempts, status, next_attempt_at, last_error, delivered_at
             FROM outbox ORDER BY occurred_at DESC, event_id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Delivery {
                event_id: row.get(0)?,
                aggregate_id: row.get(1)?,
                event_type: row.get(2)?,
                recipient_id: row.get(3)?,
                attempts: row.get(4)?,
                status: row.get(5)?,
                next_attempt_at: row.get(6)?,
                last_error: row.get(7)?,
                delivered_at: row.get(8)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn retry_delivery(&self, event_id: &str, at: i64) -> Result<bool, StoreError> {
        let changed = self.connection.execute(
            "UPDATE outbox SET status='pending', attempts=0, lease_until=NULL, next_attempt_at=?2, last_error=NULL
             WHERE event_id=?1 AND status='dead_letter'",
            params![event_id, at],
        )?;
        Ok(changed == 1)
    }

    pub fn configure_relay_fault(&self, mode: &str) -> Result<(), StoreError> {
        self.connection.execute("DELETE FROM relay_faults", [])?;
        self.connection.execute(
            "INSERT INTO relay_faults(mode, remaining) VALUES (?1, 1)",
            params![mode],
        )?;
        Ok(())
    }

    pub fn reserve(
        &mut self,
        actor: &Actor,
        request_key: &str,
        event_id: &str,
        cohort: &str,
        assigned_at: i64,
    ) -> Result<ReservationReceipt, StoreError> {
        let fingerprint = format!("reserve|{}|{}|{}", actor.actor_id, event_id, cohort);
        if let Some((known, response)) = self.command(actor, request_key)? {
            if known != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_key.to_string()).into());
            }
            let mut receipt: ReservationReceipt = decode(&response)?;
            receipt.replayed = true;
            return Ok(receipt);
        }
        let mut candidate = self.engine.clone();
        let receipt = candidate.reserve(
            actor,
            &domain_request_id(actor, request_key),
            event_id,
            cohort,
            assigned_at,
        )?;
        self.commit(
            actor,
            request_key,
            &fingerprint,
            assigned_at,
            &receipt,
            candidate,
        )?;
        Ok(receipt)
    }

    pub fn create_event(
        &mut self,
        actor: &Actor,
        request_key: &str,
        event: Event,
        assigned_at: i64,
    ) -> Result<Event, StoreError> {
        let fingerprint = format!(
            "create_event|{}|{}",
            actor.actor_id,
            serde_json::to_string(&event)?
        );
        if let Some((known, response)) = self.command(actor, request_key)? {
            if known != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_key.to_string()).into());
            }
            return decode(&response);
        }
        let mut candidate = self.engine.clone();
        candidate.add_event(
            actor,
            &domain_request_id(actor, request_key),
            event.clone(),
            assigned_at,
        )?;
        self.commit(
            actor,
            request_key,
            &fingerprint,
            assigned_at,
            &event,
            candidate,
        )?;
        Ok(event)
    }

    pub fn cancel(
        &mut self,
        actor: &Actor,
        request_key: &str,
        reservation_id: u64,
        assigned_at: i64,
    ) -> Result<CancellationReceipt, StoreError> {
        let fingerprint = format!("cancel|{}|{}", actor.actor_id, reservation_id);
        if let Some((known, response)) = self.command(actor, request_key)? {
            if known != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_key.to_string()).into());
            }
            let mut receipt: CancellationReceipt = decode(&response)?;
            receipt.replayed = true;
            return Ok(receipt);
        }
        let mut candidate = self.engine.clone();
        let receipt = candidate.cancel(
            actor,
            &domain_request_id(actor, request_key),
            reservation_id,
            assigned_at,
        )?;
        self.commit(
            actor,
            request_key,
            &fingerprint,
            assigned_at,
            &receipt,
            candidate,
        )?;
        Ok(receipt)
    }

    pub fn check_in(
        &mut self,
        actor: &Actor,
        request_key: &str,
        reservation_id: u64,
        assigned_at: i64,
    ) -> Result<CheckInReceipt, StoreError> {
        let fingerprint = format!("check_in|{}|{}", actor.actor_id, reservation_id);
        if let Some((known, response)) = self.command(actor, request_key)? {
            if known != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_key.to_string()).into());
            }
            let mut receipt: CheckInReceipt = decode(&response)?;
            receipt.replayed = true;
            return Ok(receipt);
        }
        let mut candidate = self.engine.clone();
        let receipt = candidate.check_in(
            actor,
            &domain_request_id(actor, request_key),
            reservation_id,
            assigned_at,
        )?;
        self.commit(
            actor,
            request_key,
            &fingerprint,
            assigned_at,
            &receipt,
            candidate,
        )?;
        Ok(receipt)
    }

    pub fn set_capacity(
        &mut self,
        actor: &Actor,
        request_key: &str,
        event_id: &str,
        capacity: u32,
        assigned_at: i64,
    ) -> Result<Vec<u64>, StoreError> {
        let fingerprint = format!("set_capacity|{}|{}|{}", actor.actor_id, event_id, capacity);
        if let Some((known, response)) = self.command(actor, request_key)? {
            if known != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_key.to_string()).into());
            }
            return decode(&response);
        }
        let mut candidate = self.engine.clone();
        let promoted = candidate.set_capacity(
            actor,
            &domain_request_id(actor, request_key),
            event_id,
            capacity,
            assigned_at,
        )?;
        self.commit(
            actor,
            request_key,
            &fingerprint,
            assigned_at,
            &promoted,
            candidate,
        )?;
        Ok(promoted)
    }

    pub fn update_event(
        &mut self,
        actor: &Actor,
        request_key: &str,
        event_id: &str,
        expected_revision: u64,
        patch: EventPatch,
        assigned_at: i64,
    ) -> Result<EventUpdateReceipt, StoreError> {
        let fingerprint = format!(
            "update_event|{}|{}|{}|{}",
            actor.actor_id,
            event_id,
            expected_revision,
            serde_json::to_string(&patch)?
        );
        if let Some((known, response)) = self.command(actor, request_key)? {
            if known != fingerprint {
                return Err(SlotForgeError::IdempotencyConflict(request_key.to_string()).into());
            }
            let mut receipt: EventUpdateReceipt = decode(&response)?;
            receipt.replayed = true;
            return Ok(receipt);
        }
        let mut candidate = self.engine.clone();
        let receipt = candidate.update_event(
            actor,
            &domain_request_id(actor, request_key),
            event_id,
            expected_revision,
            patch,
            assigned_at,
        )?;
        self.commit(
            actor,
            request_key,
            &fingerprint,
            assigned_at,
            &receipt,
            candidate,
        )?;
        Ok(receipt)
    }

    fn command(
        &self,
        actor: &Actor,
        request_key: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT fingerprint, response FROM commands WHERE actor_id=?1 AND request_key=?2",
                params![actor.actor_id, request_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    fn commit<T: Serialize>(
        &mut self,
        actor: &Actor,
        request_key: &str,
        fingerprint: &str,
        assigned_at: i64,
        response: &T,
        candidate: Engine,
    ) -> Result<(), StoreError> {
        let payload = serde_json::to_string(&candidate.persisted_state())?;
        let response = serde_json::to_string(response)?;
        let prior_audit_sequence = self
            .engine
            .audit()
            .last()
            .map(|entry| entry.sequence)
            .unwrap_or(0);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE engine_state SET revision=revision+1, payload=?1 WHERE id=1",
            params![payload],
        )?;
        transaction.execute(
            "INSERT INTO commands(actor_id, request_key, fingerprint, assigned_at, response) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![actor.actor_id, request_key, fingerprint, assigned_at, response],
        )?;
        enqueue_audit_events(&transaction, &candidate, prior_audit_sequence)?;
        transaction.commit()?;
        self.engine = candidate;
        Ok(())
    }
}

fn domain_request_id(actor: &Actor, request_key: &str) -> String {
    sha256_hex(format!("{}\x1f{request_key}", actor.actor_id).as_bytes())
}

fn backfill_outbox(connection: &Connection, engine: &Engine) -> Result<(), StoreError> {
    let transaction = connection.unchecked_transaction()?;
    enqueue_audit_events(&transaction, engine, 0)?;
    transaction.commit()?;
    Ok(())
}

fn enqueue_audit_events(
    transaction: &Transaction<'_>,
    engine: &Engine,
    after_sequence: u64,
) -> Result<(), StoreError> {
    for audit in engine
        .audit()
        .iter()
        .filter(|entry| entry.sequence > after_sequence)
    {
        let Some((event_type, aggregate_id, recipient_id, payload)) = outbox_payload(engine, audit)
        else {
            continue;
        };
        let payload = serde_json::to_string(&payload)?;
        let payload_sha256 = sha256_hex(payload.as_bytes());
        transaction.execute(
            "INSERT OR IGNORE INTO outbox(event_id, aggregate_id, aggregate_version, type, recipient_id, payload, payload_sha256, occurred_at, attempts, status, next_attempt_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 'pending', ?8)",
            params![
                format!("evt-{:020}", audit.sequence),
                aggregate_id,
                audit.sequence as i64,
                event_type,
                recipient_id,
                payload,
                payload_sha256,
                audit.at,
            ],
        )?;
    }
    Ok(())
}

fn outbox_payload(
    engine: &Engine,
    audit: &crate::AuditEntry,
) -> Option<(&'static str, String, String, Value)> {
    match audit.action.as_str() {
        "reserve" | "promote" | "cancel" | "check_in" => {
            let reservation = engine.reservation(audit.reservation_id?)?;
            let event_type = match audit.action.as_str() {
                "reserve" => match reservation.status {
                    crate::ReservationStatus::Confirmed => "reservation.confirmed",
                    crate::ReservationStatus::Waitlisted => "reservation.waitlisted",
                    _ => return None,
                },
                "promote" => "reservation.promoted",
                "cancel" => "reservation.cancelled",
                "check_in" => "reservation.checked_in",
                _ => return None,
            };
            Some((
                event_type,
                format!("reservation_{}", reservation.reservation_id),
                reservation.attendee_id.clone(),
                json!({
                    "schema_version": 1,
                    "reservation_id": reservation.reservation_id,
                    "event_id": reservation.event_id,
                    "attendee_id": reservation.attendee_id,
                    "status": reservation.status,
                }),
            ))
        }
        "event_created" | "event_updated" => {
            let event = engine
                .events()
                .find(|event| Some(&event.event_id) == audit.event_id.as_ref())?;
            Some((
                "event.updated",
                format!("event_{}", event.event_id),
                audit.actor_id.clone(),
                json!({
                    "schema_version": 1,
                    "event_id": event.event_id,
                    "revision": event.revision,
                    "active": event.active,
                }),
            ))
        }
        _ => None,
    }
}

fn sha256_hex(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode<T: DeserializeOwned>(payload: &str) -> Result<T, StoreError> {
    serde_json::from_str(payload).map_err(|_| StoreError::InvalidCommandPayload)
}
