use slotforge::{
    Actor, Engine, EngineConfig, Event, EventPatch, ReservationStatus, SharedEngine,
    SlotForgeError, WaitlistPolicy,
};
use std::sync::Arc;
use std::thread;

fn event(id: &str, capacity: u32) -> Event {
    Event::with_booking_close(id, "Synthetic event", capacity, 1_000, 2_000, 900)
        .expect("valid fixture event")
}

fn engine(policy: WaitlistPolicy, capacity: u32) -> Engine {
    Engine::new(
        vec![event("event", capacity)],
        EngineConfig {
            max_active_per_attendee: 3,
            policy,
        },
    )
    .expect("valid engine")
}

#[test]
fn event_rejects_zero_capacity() {
    let error = Event::new("bad", "Bad", 0, 100, 200).expect_err("zero capacity is invalid");
    assert_eq!(error, SlotForgeError::InvalidCapacity("bad".to_string()));
}

#[test]
fn reserve_confirms_then_waitlists() {
    let mut engine = engine(WaitlistPolicy::Fifo, 2);
    for index in 0..3 {
        let actor = Actor::attendee(format!("attendee-{index}")).expect("valid actor");
        let receipt = engine
            .reserve(
                &actor,
                &format!("request-{index}"),
                "event",
                "student",
                200 + index,
            )
            .expect("reservation succeeds");
        if index < 2 {
            assert_eq!(receipt.status, ReservationStatus::Confirmed);
        } else {
            assert_eq!(receipt.status, ReservationStatus::Waitlisted);
            assert_eq!(receipt.waitlist_position, Some(1));
        }
    }
    assert_eq!(engine.metrics().occupied_slots, 2);
}

#[test]
fn duplicate_active_reservation_is_rejected() {
    let mut engine = engine(WaitlistPolicy::Fair, 2);
    let actor = Actor::attendee("same-attendee").expect("valid actor");
    engine
        .reserve(&actor, "first", "event", "student", 200)
        .expect("first succeeds");
    let error = engine
        .reserve(&actor, "second", "event", "student", 201)
        .expect_err("duplicate should fail");
    assert_eq!(
        error,
        SlotForgeError::DuplicateReservation("same-attendee".to_string(), "event".to_string())
    );
}

#[test]
fn fifo_cancellation_promotes_oldest_waiter() {
    let mut engine = engine(WaitlistPolicy::Fifo, 1);
    let first = Actor::attendee("first").expect("valid actor");
    let second = Actor::attendee("second").expect("valid actor");
    let third = Actor::attendee("third").expect("valid actor");
    engine
        .reserve(&first, "r1", "event", "staff", 200)
        .expect("first succeeds");
    engine
        .reserve(&second, "r2", "event", "student", 201)
        .expect("second succeeds");
    engine
        .reserve(&third, "r3", "event", "student", 202)
        .expect("third succeeds");
    let coordinator = Actor::coordinator("coord").expect("valid coordinator");
    let receipt = engine
        .cancel(&coordinator, "cancel-1", 1, 300)
        .expect("coordinator can cancel");
    assert_eq!(receipt.promoted, vec![2]);
    assert_eq!(
        engine.reservation(2).expect("promoted exists").status,
        ReservationStatus::Confirmed
    );
}

#[test]
fn fair_policy_prefers_cohort_with_fewer_grants() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let first = Actor::attendee("first").expect("valid actor");
    let staff = Actor::attendee("staff-waiter").expect("valid actor");
    let student = Actor::attendee("student-waiter").expect("valid actor");
    engine
        .reserve(&first, "r1", "event", "staff", 200)
        .expect("first succeeds");
    engine
        .reserve(&staff, "r2", "event", "staff", 201)
        .expect("staff waiter succeeds");
    engine
        .reserve(&student, "r3", "event", "student", 202)
        .expect("student waiter succeeds");
    let coordinator = Actor::coordinator("coord").expect("valid coordinator");
    let receipt = engine
        .cancel(&coordinator, "cancel-1", 1, 300)
        .expect("coordinator can cancel");
    assert_eq!(receipt.promoted, vec![3]);
    assert_eq!(engine.metrics().fairness_index, 1.0);
}

#[test]
fn idempotent_retry_returns_same_reservation_and_is_audited() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let actor = Actor::attendee("retrying").expect("valid actor");
    let first = engine
        .reserve(&actor, "same-key", "event", "student", 200)
        .expect("first succeeds");
    let retry = engine
        .reserve(&actor, "same-key", "event", "student", 200)
        .expect("retry succeeds");
    assert_eq!(first.reservation_id, retry.reservation_id);
    assert!(retry.replayed);
    assert_eq!(engine.metrics().idempotent_replays, 1);
}

#[test]
fn idempotency_conflict_is_rejected() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let actor = Actor::attendee("retrying").expect("valid actor");
    engine
        .reserve(&actor, "same-key", "event", "student", 200)
        .expect("first succeeds");
    let error = engine
        .reserve(&actor, "same-key", "event", "community", 201)
        .expect_err("changed fingerprint should fail");
    assert_eq!(
        error,
        SlotForgeError::IdempotencyConflict("same-key".to_string())
    );
}

#[test]
fn role_boundaries_are_enforced() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let coordinator = Actor::coordinator("coord").expect("valid coordinator");
    let error = engine
        .reserve(&coordinator, "r1", "event", "staff", 200)
        .expect_err("coordinator cannot reserve");
    assert_eq!(
        error,
        SlotForgeError::Forbidden {
            actor: "coord".to_string(),
            action: "reserve"
        }
    );
}

#[test]
fn attendee_limit_is_enforced_across_events() {
    let events = vec![
        event("event-a", 4),
        event("event-b", 4),
        event("event-c", 4),
    ];
    let mut engine = Engine::new(
        events,
        EngineConfig {
            max_active_per_attendee: 2,
            policy: WaitlistPolicy::Fair,
        },
    )
    .expect("valid engine");
    let actor = Actor::attendee("limited").expect("valid actor");
    engine
        .reserve(&actor, "r1", "event-a", "student", 200)
        .expect("first succeeds");
    engine
        .reserve(&actor, "r2", "event-b", "student", 201)
        .expect("second succeeds");
    let error = engine
        .reserve(&actor, "r3", "event-c", "student", 202)
        .expect_err("third active reservation should fail");
    assert_eq!(
        error,
        SlotForgeError::AttendeeLimit("limited".to_string(), 2)
    );
}

#[test]
fn checked_in_slot_remains_occupied() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let attendee = Actor::attendee("attendee").expect("valid actor");
    engine
        .reserve(&attendee, "r1", "event", "student", 200)
        .expect("reservation succeeds");
    let coordinator = Actor::coordinator("coord").expect("valid coordinator");
    engine
        .check_in(&coordinator, "checkin", 1, 980)
        .expect("check-in succeeds");
    let other = Actor::attendee("other").expect("valid actor");
    let receipt = engine
        .reserve(&other, "r2", "event", "student", 210)
        .expect("waitlist succeeds");
    assert_eq!(receipt.status, ReservationStatus::Waitlisted);
    assert_eq!(engine.metrics().occupied_slots, 1);
}

#[test]
fn closed_event_rejects_new_reservation() {
    let mut engine = Engine::new(
        vec![Event::with_booking_close("event", "Closed", 1, 100, 200, 100).expect("valid event")],
        EngineConfig::default(),
    )
    .expect("valid engine");
    let actor = Actor::attendee("late").expect("valid actor");
    let error = engine
        .reserve(&actor, "late-request", "event", "student", 100)
        .expect_err("closed event should fail");
    assert_eq!(error, SlotForgeError::EventClosed("event".to_string(), 100));
}

#[test]
fn snapshot_is_sorted_and_versioned() {
    let mut engine = Engine::new(
        vec![event("z-event", 1), event("a-event", 1)],
        EngineConfig::default(),
    )
    .expect("valid engine");
    let actor = Actor::attendee("snapshot-user").expect("valid actor");
    engine
        .reserve(&actor, "snapshot-request", "z-event", "student", 200)
        .expect("reservation succeeds");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.schema_version, 1);
    assert_eq!(snapshot.events[0].event_id, "a-event");
    assert_eq!(snapshot.events[1].event_id, "z-event");
}

#[test]
fn shared_engine_serializes_concurrent_reservations() {
    let shared = Arc::new(SharedEngine::new(engine(WaitlistPolicy::Fair, 4)));
    let mut handles = Vec::new();
    for index in 0..16 {
        let shared = Arc::clone(&shared);
        handles.push(thread::spawn(move || {
            let actor = Actor::attendee(format!("concurrent-{index}")).expect("valid actor");
            shared
                .reserve(
                    &actor,
                    &format!("concurrent-request-{index}"),
                    "event",
                    "student",
                    200 + index,
                )
                .expect("serialized reservation succeeds")
        }));
    }
    let receipts: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread completes"))
        .collect();
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.status == ReservationStatus::Confirmed)
            .count(),
        4
    );
    assert_eq!(
        shared
            .snapshot()
            .expect("snapshot succeeds")
            .metrics
            .occupied_slots,
        4
    );
}

#[test]
fn observer_cannot_cancel_even_when_reservation_exists() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let attendee = Actor::attendee("owner").expect("valid actor");
    engine
        .reserve(&attendee, "r1", "event", "student", 200)
        .expect("reservation succeeds");
    let observer = Actor::observer("read-only").expect("valid observer");
    let error = engine
        .cancel(&observer, "cancel", 1, 300)
        .expect_err("observer cannot mutate");
    assert_eq!(
        error,
        SlotForgeError::Forbidden {
            actor: "read-only".to_string(),
            action: "cancel"
        }
    );
}

#[test]
fn invalid_timestamp_is_rejected_before_mutation() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let actor = Actor::attendee("invalid-time").expect("valid actor");
    let error = engine
        .reserve(&actor, "bad-time", "event", "student", 0)
        .expect_err("non-positive timestamp should fail");
    assert_eq!(error, SlotForgeError::InvalidTimestamp);
    assert_eq!(engine.metrics().reservations_total, 0);
}

#[test]
fn cancelled_waiter_is_removed_without_promotion() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let first = Actor::attendee("first").expect("valid actor");
    let second = Actor::attendee("second").expect("valid actor");
    engine
        .reserve(&first, "r1", "event", "staff", 200)
        .expect("first succeeds");
    engine
        .reserve(&second, "r2", "event", "student", 201)
        .expect("second succeeds");
    let receipt = engine
        .cancel(&second, "cancel-waiter", 2, 300)
        .expect("owner can cancel waiter");
    assert!(receipt.promoted.is_empty());
    assert_eq!(engine.metrics().waitlisted, 0);
}

#[test]
fn coordinator_can_cancel_other_attendee() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let attendee = Actor::attendee("owner").expect("valid actor");
    engine
        .reserve(&attendee, "r1", "event", "student", 200)
        .expect("reservation succeeds");
    let coordinator = Actor::coordinator("coord").expect("valid coordinator");
    engine
        .cancel(&coordinator, "admin-cancel", 1, 300)
        .expect("coordinator can cancel");
    assert_eq!(engine.metrics().cancelled, 1);
}

#[test]
fn booking_close_is_distinct_from_event_end() {
    let event = Event::with_booking_close("event", "Close early", 1, 1_000, 2_000, 900)
        .expect("valid early close");
    let mut engine = Engine::new(vec![event], EngineConfig::default()).expect("engine");
    let attendee = Actor::attendee("late-booker").expect("actor");
    let error = engine
        .reserve(&attendee, "late", "event", "student", 900)
        .expect_err("booking closes at booking_close_at, not event end");
    assert_eq!(error, SlotForgeError::EventClosed("event".to_string(), 900));
}

#[test]
fn capacity_increase_promotes_before_close_and_never_after_close() {
    let event =
        Event::with_booking_close("event", "Capacity", 1, 1_000, 2_000, 900).expect("event");
    let mut engine = Engine::new(
        vec![event],
        EngineConfig {
            max_active_per_attendee: 3,
            policy: WaitlistPolicy::Fifo,
        },
    )
    .expect("engine");
    let first = Actor::attendee("first").expect("actor");
    let second = Actor::attendee("second").expect("actor");
    let third = Actor::attendee("third").expect("actor");
    let coordinator = Actor::coordinator("coord").expect("actor");
    engine
        .reserve(&first, "r1", "event", "student", 100)
        .expect("confirmed");
    engine
        .reserve(&second, "r2", "event", "student", 101)
        .expect("waitlisted");
    engine
        .reserve(&third, "r3", "event", "student", 102)
        .expect("waitlisted");
    assert_eq!(
        engine
            .set_capacity(&coordinator, "cap-2", "event", 2, 800)
            .expect("increase"),
        vec![2]
    );
    assert_eq!(
        engine.reservation(2).expect("reservation").status,
        ReservationStatus::Confirmed
    );
    assert!(engine
        .set_capacity(&coordinator, "cap-3", "event", 3, 900)
        .expect("closed increase")
        .is_empty());
    assert_eq!(
        engine.reservation(3).expect("reservation").status,
        ReservationStatus::Waitlisted
    );
    let error = engine
        .set_capacity(&coordinator, "cap-too-low", "event", 1, 850)
        .expect_err("cannot evict occupied reservations through capacity change");
    assert_eq!(
        error,
        SlotForgeError::CapacityBelowOccupied {
            event_id: "event".to_string(),
            requested: 1,
            occupied: 2
        }
    );
}

#[test]
fn event_patch_requires_matching_revision_and_promotes_in_the_same_change() {
    let mut engine = engine(WaitlistPolicy::Fifo, 1);
    let coordinator = Actor::coordinator("coord").expect("coordinator");
    let first = Actor::attendee("first").expect("attendee");
    let second = Actor::attendee("second").expect("attendee");
    engine
        .reserve(&first, "first", "event", "student", 100)
        .expect("confirmed");
    engine
        .reserve(&second, "second", "event", "student", 101)
        .expect("waitlisted");

    let updated = engine
        .update_event(
            &coordinator,
            "edit-1",
            "event",
            1,
            EventPatch {
                title: Some("Renamed workshop".to_string()),
                capacity: Some(2),
                ..EventPatch::default()
            },
            200,
        )
        .expect("matching revision edits event");
    assert_eq!(updated.event.revision, 2);
    assert_eq!(updated.promoted, vec![2]);
    assert_eq!(
        engine.reservation(2).expect("promoted reservation").status,
        ReservationStatus::Confirmed
    );

    let error = engine
        .update_event(
            &coordinator,
            "edit-stale",
            "event",
            1,
            EventPatch {
                active: Some(false),
                ..EventPatch::default()
            },
            201,
        )
        .expect_err("stale editor must not overwrite");
    assert_eq!(
        error,
        SlotForgeError::RevisionConflict {
            event_id: "event".to_string(),
            expected: 1,
            current: 2,
        }
    );
    assert!(engine.events().next().expect("event").active);
}

#[test]
fn persisted_state_keeps_sequences_grants_and_idempotency_across_restore() {
    let mut engine = engine(WaitlistPolicy::Fair, 1);
    let first = Actor::attendee("first").expect("actor");
    let second = Actor::attendee("second").expect("actor");
    let original = engine
        .reserve(&first, "stable-key", "event", "student", 200)
        .expect("reserve");
    engine
        .reserve(&second, "second-key", "event", "staff", 201)
        .expect("waitlist");
    let state =
        serde_json::from_str(&serde_json::to_string(&engine.persisted_state()).expect("serialize"))
            .expect("deserialize");
    let mut restored = Engine::from_persisted_state(state).expect("restore full engine state");
    let replay = restored
        .reserve(&first, "stable-key", "event", "student", 999)
        .expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.reservation_id, original.reservation_id);
    let third = Actor::attendee("third").expect("actor");
    let new_receipt = restored
        .reserve(&third, "third-key", "event", "community", 202)
        .expect("new id");
    assert_eq!(new_receipt.reservation_id, 3);
    assert_eq!(restored.waitlist("event")[0].reservation_id, 2);
}
