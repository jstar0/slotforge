use rusqlite::Connection;
use slotforge::{
    Actor, Engine, EngineConfig, Event, EventPatch, ReservationStatus, SqliteEngine, StoreError,
    WaitlistPolicy,
};
use tempfile::tempdir;

fn initial() -> Engine {
    Engine::new(
        vec![
            Event::with_booking_close("event", "Persistent event", 1, 1_000, 2_000, 900)
                .expect("event"),
        ],
        EngineConfig {
            max_active_per_attendee: 3,
            policy: WaitlistPolicy::Fifo,
        },
    )
    .expect("engine")
}

#[test]
fn committed_command_replays_after_reopen_without_a_new_reservation() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("slotforge.sqlite");
    let actor = Actor::attendee("attendee").expect("actor");
    let first = {
        let mut store = SqliteEngine::open(&path, initial()).expect("open");
        store
            .reserve(&actor, "same-key", "event", "student", 100)
            .expect("reserve")
    };
    let mut reopened = SqliteEngine::open(&path, initial()).expect("reopen committed state");
    let replay = reopened
        .reserve(&actor, "same-key", "event", "student", 500)
        .expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.reservation_id, first.reservation_id);
    assert_eq!(reopened.engine().metrics().reservations_total, 1);
}

#[test]
fn failed_command_does_not_replace_committed_memory_or_sqlite_state() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("slotforge.sqlite");
    let actor = Actor::attendee("attendee").expect("actor");
    let mut store = SqliteEngine::open(&path, initial()).expect("open");
    let error = store
        .reserve(&actor, "late", "event", "student", 900)
        .expect_err("closed booking");
    assert!(error.to_string().contains("closed"));
    assert_eq!(store.engine().metrics().reservations_total, 0);
    let reopened = SqliteEngine::open(&path, initial()).expect("reopen");
    assert_eq!(reopened.engine().metrics().reservations_total, 0);
}

#[test]
fn cancel_capacity_and_check_in_share_the_same_recoverable_command_boundary() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("slotforge.sqlite");
    let attendee_a = Actor::attendee("attendee-a").expect("actor");
    let attendee_b = Actor::attendee("attendee-b").expect("actor");
    let coordinator = Actor::coordinator("coordinator").expect("actor");
    let mut store = SqliteEngine::open(&path, initial()).expect("open");
    let first = store
        .reserve(&attendee_a, "reserve-a", "event", "student", 100)
        .expect("first");
    store
        .reserve(&attendee_b, "reserve-b", "event", "student", 101)
        .expect("waitlist");
    assert_eq!(
        store
            .set_capacity(&coordinator, "capacity", "event", 2, 200)
            .expect("promote"),
        vec![2]
    );
    let checked = store
        .check_in(&coordinator, "checkin", first.reservation_id, 980)
        .expect("check in");
    assert_eq!(checked.reservation_id, first.reservation_id);
    let cancelled = store
        .cancel(&coordinator, "cancel-b", 2, 300)
        .expect("cancel promoted");
    assert!(cancelled.promoted.is_empty());
    drop(store);
    let reopened = SqliteEngine::open(&path, initial()).expect("reopen");
    assert_eq!(
        reopened.engine().reservation(1).expect("first").status,
        ReservationStatus::CheckedIn
    );
    assert_eq!(
        reopened.engine().reservation(2).expect("second").status,
        ReservationStatus::Cancelled
    );
}

#[test]
fn event_patch_replays_after_reopen_and_rejects_a_stale_revision() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("slotforge.sqlite");
    let coordinator = Actor::coordinator("coordinator").expect("actor");
    let first = {
        let mut store = SqliteEngine::open(&path, initial()).expect("open");
        store
            .update_event(
                &coordinator,
                "edit-event",
                "event",
                1,
                EventPatch {
                    title: Some("Edited title".to_string()),
                    active: Some(false),
                    ..EventPatch::default()
                },
                100,
            )
            .expect("edit")
    };
    assert_eq!(first.event.revision, 2);
    let mut reopened = SqliteEngine::open(&path, initial()).expect("reopen");
    let replay = reopened
        .update_event(
            &coordinator,
            "edit-event",
            "event",
            1,
            EventPatch {
                title: Some("Edited title".to_string()),
                active: Some(false),
                ..EventPatch::default()
            },
            500,
        )
        .expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.event, first.event);
    let error = reopened
        .update_event(
            &coordinator,
            "stale-edit",
            "event",
            1,
            EventPatch {
                active: Some(true),
                ..EventPatch::default()
            },
            501,
        )
        .expect_err("stale revision must fail");
    assert!(matches!(
        error,
        StoreError::Domain(slotforge::SlotForgeError::RevisionConflict { .. })
    ));
}

#[test]
fn reservation_and_promotion_outbox_records_commit_with_the_engine_state() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("slotforge.sqlite");
    let first = Actor::attendee("first").expect("actor");
    let second = Actor::attendee("second").expect("actor");
    let coordinator = Actor::coordinator("coordinator").expect("actor");
    {
        let mut store = SqliteEngine::open(&path, initial()).expect("open");
        store
            .reserve(&first, "first", "event", "student", 100)
            .expect("confirmed reservation");
        store
            .reserve(&second, "second", "event", "student", 101)
            .expect("waitlisted reservation");
        store
            .update_event(
                &coordinator,
                "grow",
                "event",
                1,
                EventPatch {
                    capacity: Some(2),
                    ..EventPatch::default()
                },
                200,
            )
            .expect("capacity edit and promotion");
    }
    let database = Connection::open(path).expect("open committed database");
    let count: i64 = database
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .expect("outbox count");
    assert_eq!(
        count, 4,
        "confirmed, waitlisted, promoted, and event update"
    );
    let promoted: String = database
        .query_row(
            "SELECT type FROM outbox WHERE type='reservation.promoted'",
            [],
            |row| row.get(0),
        )
        .expect("promotion record");
    assert_eq!(promoted, "reservation.promoted");
}

#[test]
fn create_event_key_cannot_be_reused_with_a_different_title() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("slotforge.sqlite");
    let coordinator = Actor::coordinator("coordinator").expect("actor");
    let mut store = SqliteEngine::open(&path, initial()).expect("open");
    store
        .create_event(
            &coordinator,
            "new-event",
            Event::new("new-event", "First title", 2, 3_000, 4_000).expect("event"),
            100,
        )
        .expect("first event create");
    let error = store
        .create_event(
            &coordinator,
            "new-event",
            Event::new("different-event", "Different title", 2, 3_000, 4_000).expect("event"),
            101,
        )
        .expect_err("different command must conflict");
    assert!(matches!(
        error,
        StoreError::Domain(slotforge::SlotForgeError::IdempotencyConflict(_))
    ));
}

#[test]
fn notifications_distinguish_activity_id_from_outbox_delivery_id() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("slotforge.sqlite");
    let attendee = Actor::attendee("attendee").expect("actor");
    {
        let mut store = SqliteEngine::open(&path, initial()).expect("open");
        store
            .reserve(&attendee, "reserve", "event", "student", 100)
            .expect("reservation");
    }

    let database = Connection::open(&path).expect("open committed database");
    let (delivery_event_id, payload): (String, String) = database
        .query_row(
            "SELECT event_id, payload FROM outbox WHERE recipient_id='attendee'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("committed outbox record");
    database
        .execute(
            "INSERT INTO inbox(event_id, recipient_id, body, received_at, payload_sha256)
             VALUES (?1, 'attendee', 'confirmed', 100, 'test-hash')",
            [&delivery_event_id],
        )
        .expect("inbox record");
    drop(database);

    let store = SqliteEngine::open(&path, initial()).expect("reopen");
    let notifications = store.notifications(&attendee).expect("notifications");
    let notification = notifications.first().expect("notification");
    let payload: serde_json::Value = serde_json::from_str(&payload).expect("outbox payload");
    assert_eq!(
        notification.event_id.as_str(),
        payload["event_id"].as_str().expect("source event id")
    );
    assert_eq!(notification.event_id, "event");
    assert_eq!(notification.delivery_event_id, delivery_event_id);
}
