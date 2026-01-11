use crate::model::{
    Actor, Engine, EngineConfig, EngineMetrics, Event, ReservationStatus, Role, SlotForgeError,
    WaitlistPolicy,
};
use serde::Serialize;

const DEMO_START: i64 = 1_700_000_000;

#[derive(Clone, Debug, Serialize)]
pub struct OperationRecord {
    pub step: u32,
    pub action: String,
    pub request_id: String,
    pub result: String,
}

#[derive(Debug)]
pub struct DemoRun {
    pub engine: Engine,
    pub operations: Vec<OperationRecord>,
    pub expected_rejections: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct BenchmarkRow {
    pub policy: WaitlistPolicy,
    pub requests: usize,
    pub reservations_total: usize,
    pub confirmed: usize,
    pub waitlisted: usize,
    pub cancelled: usize,
    pub promotions: usize,
    pub idempotent_replays: usize,
    pub utilization: f64,
    pub fairness_index: f64,
    pub attendee_fairness_index: f64,
    pub max_wait_seconds: i64,
}

pub fn events() -> Result<Vec<Event>, SlotForgeError> {
    Ok(vec![
        Event::new(
            "event-a",
            "Open Lab Orientation",
            3,
            DEMO_START + 1_000,
            DEMO_START + 4_600,
        )?,
        Event::new(
            "event-b",
            "Research Showcase Walkthrough",
            2,
            DEMO_START + 5_000,
            DEMO_START + 8_600,
        )?,
    ])
}

pub fn build_engine(policy: WaitlistPolicy) -> Result<Engine, SlotForgeError> {
    Engine::new(
        events()?,
        EngineConfig {
            max_active_per_attendee: 3,
            policy,
        },
    )
}

pub fn run_demo(policy: WaitlistPolicy) -> Result<DemoRun, SlotForgeError> {
    let mut engine = build_engine(policy)?;
    let mut operations = Vec::new();
    let mut expected_rejections = 0;
    let mut step = 1;

    for (index, cohort) in [
        "staff",
        "staff",
        "student",
        "student",
        "community",
        "student",
        "staff",
        "community",
    ]
    .into_iter()
    .enumerate()
    {
        let attendee_id = format!("attendee-{index:02}");
        let request_id = format!("reserve-{index:02}");
        let actor = Actor::attendee(&attendee_id)?;
        let result = engine.reserve(
            &actor,
            &request_id,
            "event-a",
            cohort,
            DEMO_START + index as i64 * 10,
        );
        match result {
            Ok(receipt) => operations.push(OperationRecord {
                step,
                action: "reserve".to_string(),
                request_id,
                result: format!("{}#{:?}", receipt.reservation_id, receipt.status),
            }),
            Err(error) => {
                expected_rejections += 1;
                operations.push(OperationRecord {
                    step,
                    action: "reserve_rejected".to_string(),
                    request_id,
                    result: error.to_string(),
                });
            }
        }
        step += 1;
    }

    let replay_actor = Actor::attendee("attendee-03")?;
    let replay = engine.reserve(
        &replay_actor,
        "reserve-03",
        "event-a",
        "student",
        DEMO_START + 30,
    )?;
    operations.push(OperationRecord {
        step,
        action: "reserve_retry".to_string(),
        request_id: "reserve-03".to_string(),
        result: format!("replayed={}#{}", replay.replayed, replay.reservation_id),
    });
    step += 1;

    let owner = Actor::attendee("attendee-00")?;
    let cancelled = engine.cancel(&owner, "cancel-00", 1, DEMO_START + 100)?;
    operations.push(OperationRecord {
        step,
        action: "cancel".to_string(),
        request_id: "cancel-00".to_string(),
        result: format!("promoted={:?}", cancelled.promoted),
    });
    step += 1;

    let coordinator = Actor::coordinator("coordinator-01")?;
    let check_in = engine.check_in(&coordinator, "checkin-02", 2, DEMO_START + 1_005)?;
    operations.push(OperationRecord {
        step,
        action: "check_in".to_string(),
        request_id: "checkin-02".to_string(),
        result: format!(
            "reservation={} at={}",
            check_in.reservation_id, check_in.checked_in_at
        ),
    });
    step += 1;

    let second_owner = Actor::attendee("attendee-02")?;
    let cancelled = engine.cancel(&second_owner, "cancel-02", 3, DEMO_START + 120)?;
    operations.push(OperationRecord {
        step,
        action: "cancel".to_string(),
        request_id: "cancel-02".to_string(),
        result: format!("promoted={:?}", cancelled.promoted),
    });
    step += 1;

    let observer = Actor::observer("observer-01")?;
    let error = engine.cancel(&observer, "observer-cancel", 2, DEMO_START + 130);
    let error = error.expect_err("observer cancellation must be rejected");
    expected_rejections += 1;
    operations.push(OperationRecord {
        step,
        action: "observer_rejected".to_string(),
        request_id: "observer-cancel".to_string(),
        result: error.to_string(),
    });
    step += 1;

    let coordinator_cancel = engine.cancel(&coordinator, "cancel-waitlist", 6, DEMO_START + 140)?;
    operations.push(OperationRecord {
        step,
        action: "coordinator_cancel".to_string(),
        request_id: "cancel-waitlist".to_string(),
        result: format!("promoted={:?}", coordinator_cancel.promoted),
    });
    step += 1;

    let second_event_actor = Actor::attendee("attendee-00")?;
    let second_event = engine.reserve(
        &second_event_actor,
        "reserve-b-00",
        "event-b",
        "staff",
        DEMO_START + 200,
    )?;
    operations.push(OperationRecord {
        step,
        action: "reserve_second_event".to_string(),
        request_id: "reserve-b-00".to_string(),
        result: format!("{}#{:?}", second_event.reservation_id, second_event.status),
    });
    step += 1;

    let wrong_role = engine.reserve(
        &coordinator,
        "coordinator-reserve",
        "event-b",
        "staff",
        DEMO_START + 210,
    );
    let wrong_role = wrong_role.expect_err("coordinator cannot reserve as an attendee");
    expected_rejections += 1;
    operations.push(OperationRecord {
        step,
        action: "role_rejected".to_string(),
        request_id: "coordinator-reserve".to_string(),
        result: wrong_role.to_string(),
    });

    Ok(DemoRun {
        engine,
        operations,
        expected_rejections,
    })
}

pub fn run_benchmark() -> Result<Vec<BenchmarkRow>, SlotForgeError> {
    [WaitlistPolicy::Fifo, WaitlistPolicy::Fair]
        .into_iter()
        .map(run_policy_benchmark)
        .collect()
}

fn run_policy_benchmark(policy: WaitlistPolicy) -> Result<BenchmarkRow, SlotForgeError> {
    let mut engine = Engine::new(
        vec![Event::with_booking_close(
            "benchmark-event",
            "Synthetic capacity stress",
            4,
            DEMO_START + 1_000,
            DEMO_START + 10_000,
            DEMO_START + 800,
        )?],
        EngineConfig {
            max_active_per_attendee: 2,
            policy,
        },
    )?;
    let requests = 20;
    for index in 0..requests {
        let attendee = Actor::attendee(format!("bench-{index:02}"))?;
        let cohort = if index < 12 {
            "staff"
        } else if index % 2 == 0 {
            "student"
        } else {
            "community"
        };
        let _ = engine.reserve(
            &attendee,
            &format!("bench-reserve-{index:02}"),
            "benchmark-event",
            cohort,
            DEMO_START + index as i64,
        )?;
    }

    let retry_actor = Actor::attendee("bench-19")?;
    let _ = engine.reserve(
        &retry_actor,
        "bench-reserve-19",
        "benchmark-event",
        "community",
        DEMO_START + 19,
    )?;

    let coordinator = Actor::coordinator("benchmark-coordinator")?;
    for index in 0..8 {
        let reservation_id = engine
            .reservations()
            .find(|reservation| reservation.status == ReservationStatus::Confirmed)
            .map(|reservation| reservation.reservation_id)
            .expect("the benchmark keeps one confirmed slot available");
        let _ = engine.cancel(
            &coordinator,
            &format!("bench-cancel-{index:02}"),
            reservation_id,
            DEMO_START + 500 + index as i64,
        )?;
    }

    let metrics = engine.metrics();
    Ok(row(policy, requests, metrics))
}

fn row(policy: WaitlistPolicy, requests: usize, metrics: EngineMetrics) -> BenchmarkRow {
    BenchmarkRow {
        policy,
        requests,
        reservations_total: metrics.reservations_total,
        confirmed: metrics.confirmed,
        waitlisted: metrics.waitlisted,
        cancelled: metrics.cancelled,
        promotions: metrics.promotions,
        idempotent_replays: metrics.idempotent_replays,
        utilization: metrics.utilization,
        fairness_index: metrics.fairness_index,
        attendee_fairness_index: metrics.attendee_fairness_index,
        max_wait_seconds: metrics.max_wait_seconds,
    }
}

pub fn status_name(status: ReservationStatus) -> &'static str {
    match status {
        ReservationStatus::Confirmed => "confirmed",
        ReservationStatus::Waitlisted => "waitlisted",
        ReservationStatus::CheckedIn => "checked_in",
        ReservationStatus::Cancelled => "cancelled",
    }
}

pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Attendee => "attendee",
        Role::Coordinator => "coordinator",
        Role::Observer => "observer",
    }
}
