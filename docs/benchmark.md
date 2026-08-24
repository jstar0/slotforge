# Benchmark protocol

The benchmark is a behavioral comparison, not a production load test.

1. Create one synthetic event with capacity four.
2. Submit twenty deterministic reservation requests from three cohorts.
3. Replay one request with the same idempotency key.
4. Cancel eight reservations through a coordinator, allowing promotions after each release.
5. Report final confirmed, waitlisted, cancelled, promotion, utilization, maximum-wait, and Jain fairness metrics for both FIFO and fair policies.

The request stream, timestamps, identifiers, and policy order are fixed in `src/fixture.rs`. There is no random seed and no external dataset. The resulting JSON/CSV/Markdown files are hash-bound by `manifest.json`; two runs should have identical payload hashes.

Interpretation is intentionally narrow:

- utilization describes this finite synthetic capacity;
- fairness is a Jain index over cohort grant counts (with an attendee-level companion metric);
- maximum wait is measured in the fixture's logical timestamp units;
- none of these values imply real-world demand, identity accuracy, or commercial impact.
