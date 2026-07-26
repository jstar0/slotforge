package relay

import (
	"context"
	"database/sql"
	"errors"
	"testing"
	"time"

	_ "github.com/mattn/go-sqlite3"
)

func testRelay(t *testing.T, now *time.Time) (*Relay, *sql.DB) {
	t.Helper()
	db, err := sql.Open("sqlite3", "file:relay-test?mode=memory&cache=shared")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	_, err = db.Exec(`CREATE TABLE outbox (
        event_id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, aggregate_version INTEGER NOT NULL,
        type TEXT NOT NULL, recipient_id TEXT NOT NULL, payload TEXT NOT NULL, payload_sha256 TEXT NOT NULL,
        occurred_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL,
        lease_until INTEGER, next_attempt_at INTEGER NOT NULL, last_error TEXT, delivered_at INTEGER
    ); CREATE TABLE inbox (
        notification_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE,
        recipient_id TEXT NOT NULL, body TEXT NOT NULL, received_at INTEGER NOT NULL,
        read_at INTEGER, payload_sha256 TEXT NOT NULL
    ); CREATE TABLE relay_faults (mode TEXT PRIMARY KEY, remaining INTEGER NOT NULL);`)
	if err != nil {
		t.Fatal(err)
	}
	r, err := New(db, func() time.Time { return *now })
	if err != nil {
		t.Fatal(err)
	}
	return r, db
}

func insertEvent(t *testing.T, db *sql.DB, id string, payload string, payloadHash string, now time.Time) {
	t.Helper()
	_, err := db.Exec(`INSERT INTO outbox(event_id, aggregate_id, aggregate_version, type, recipient_id, payload, payload_sha256, occurred_at, attempts, status, next_attempt_at)
        VALUES (?1, 'reservation_1', 1, 'reservation.confirmed', 'demo-a', ?2, ?3, ?4, 0, 'pending', ?4)`,
		id, payload, payloadHash, now.Unix())
	if err != nil {
		t.Fatal(err)
	}
}

func TestInboxDedupeSurvivesAckBeforeCrashFault(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	r, db := testRelay(t, &now)
	payload := `{"schema_version":1,"reservation_id":1,"event_id":"workshop-a","attendee_id":"demo-a","status":"confirmed"}`
	insertEvent(t, db, "evt-1", payload, hash(payload), now)

	first, err := r.DeliverOnce(context.Background(), FailAfterInbox)
	if err == nil || !first.Claimed || first.Delivered {
		t.Fatalf("expected injected post-inbox failure, got result=%+v err=%v", first, err)
	}
	var count int
	if err := db.QueryRow("SELECT COUNT(*) FROM inbox WHERE event_id='evt-1'").Scan(&count); err != nil {
		t.Fatal(err)
	}
	if count != 1 {
		t.Fatalf("inbox count after crash simulation = %d, want 1", count)
	}
	now = now.Add(2 * time.Second)
	second, err := r.DeliverOnce(context.Background(), NoFailure)
	if err != nil || !second.Delivered || second.Attempts != 2 {
		t.Fatalf("retry result=%+v err=%v", second, err)
	}
	if err := db.QueryRow("SELECT COUNT(*) FROM inbox WHERE event_id='evt-1'").Scan(&count); err != nil {
		t.Fatal(err)
	}
	if count != 1 {
		t.Fatalf("duplicate delivery created %d inbox rows", count)
	}
	var status string
	if err := db.QueryRow("SELECT status FROM outbox WHERE event_id='evt-1'").Scan(&status); err != nil {
		t.Fatal(err)
	}
	if status != "delivered" {
		t.Fatalf("outbox status=%q, want delivered", status)
	}
}

func TestCorruptPayloadIsRejectedAndNeedsAttention(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	r, db := testRelay(t, &now)
	payload := `{"schema_version":1,"reservation_id":1,"event_id":"workshop-a","attendee_id":"demo-a","status":"confirmed"}`
	insertEvent(t, db, "evt-bad", payload, "not-the-payload-hash", now)
	_, err := r.DeliverOnce(context.Background(), NoFailure)
	if !errors.Is(err, ErrPayloadHashMismatch) {
		t.Fatalf("error=%v, want payload hash mismatch", err)
	}
	var status string
	if err := db.QueryRow("SELECT status FROM outbox WHERE event_id='evt-bad'").Scan(&status); err != nil {
		t.Fatal(err)
	}
	if status != "dead_letter" {
		t.Fatalf("outbox status=%q, want dead_letter", status)
	}
}

func TestConfiguredOneShotFaultIsConsumedByNormalRelayRun(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	r, db := testRelay(t, &now)
	payload := `{"schema_version":1,"reservation_id":1,"event_id":"workshop-a","attendee_id":"demo-a","status":"confirmed"}`
	insertEvent(t, db, "evt-control", payload, hash(payload), now)
	if _, err := db.Exec("INSERT INTO relay_faults(mode, remaining) VALUES ('before_inbox', 1)"); err != nil {
		t.Fatal(err)
	}
	_, err := r.DeliverOnce(context.Background(), NoFailure)
	if err == nil {
		t.Fatal("configured one-shot fault must fail the claimed delivery")
	}
	var remaining int
	if err := db.QueryRow("SELECT remaining FROM relay_faults WHERE mode='before_inbox'").Scan(&remaining); err != nil {
		t.Fatal(err)
	}
	if remaining != 0 {
		t.Fatalf("remaining=%d, want 0", remaining)
	}
}
