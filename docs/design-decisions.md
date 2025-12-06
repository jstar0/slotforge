# SlotForge design decisions and limits

SlotForge is a synthetic local capacity and waitlist engine, not a production booking, identity, or payment system.

## Decisions embodied in the implementation

1. **Capacity is an invariant across reservation, cancellation, check-in and promotion.** Rejected alternative: calculate availability only in the UI.
2. **Idempotency keys bind retry requests and reject conflicts.** Rejected alternative: treat every retry as a new reservation.
3. **Roles are explicit for observer, attendee, coordinator and owner actions.** Rejected alternative: let any event participant cancel any reservation.
4. **FIFO and cohort-fair promotion are separate policies.** Rejected alternative: call one queue policy universally fair without comparison.
5. **SharedEngine serializes concurrent mutations.** Rejected alternative: rely on callers to avoid races.

## Known weaknesses

1. **Events, attendees, cohorts and capacity are synthetic fixtures.**
2. **The in-process lock is not distributed transaction or multi-instance coordination.**
3. **Fairness is measured on one skewed benchmark and a Jain index, not user welfare or policy compliance.**
4. **No authentication, consent, notification, calendar integration, payment, cancellation policy or persistence service exists.**
5. **Waitlist promotion ignores real priority, accessibility, time zone and attendance rules.**
6. **Audit records are local artifacts, not signed or immutable legal records.**
7. **Benchmark repeatability does not establish operational fairness or load behavior.**

## Interview-safe description

Describe SlotForge as a deterministic Rust capacity-state-machine benchmark with idempotency, role checks and explicit FIFO/fair policy comparison.
