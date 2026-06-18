use slotforge::fixture::run_benchmark;

#[test]
fn benchmark_contains_fifo_and_fair_rows() {
    let rows = run_benchmark().expect("benchmark fixture is valid");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].policy, slotforge::WaitlistPolicy::Fifo);
    assert_eq!(rows[1].policy, slotforge::WaitlistPolicy::Fair);
}

#[test]
fn fair_policy_improves_cohort_fairness_on_skewed_queue() {
    let rows = run_benchmark().expect("benchmark fixture is valid");
    assert!(rows[1].fairness_index > rows[0].fairness_index);
    assert_eq!(rows[1].fairness_index, 1.0);
}

#[test]
fn benchmark_metrics_are_bounded_and_capacity_safe() {
    for row in run_benchmark().expect("benchmark fixture is valid") {
        assert_eq!(
            row.confirmed + row.waitlisted + row.cancelled,
            row.reservations_total
        );
        assert_eq!(row.confirmed, 4);
        assert_eq!(row.utilization, 1.0);
        assert!((0.0..=1.0).contains(&row.fairness_index));
        assert!((0.0..=1.0).contains(&row.attendee_fairness_index));
        assert!(row.max_wait_seconds > 0);
    }
}

#[test]
fn benchmark_is_deterministic() {
    let first = run_benchmark().expect("first run is valid");
    let second = run_benchmark().expect("second run is valid");
    assert_eq!(first, second);
}
