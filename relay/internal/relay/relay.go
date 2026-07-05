// Package relay implements SlotForge's local transactional-outbox consumer.
// It never changes reservation state: Rust owns that state; the relay only
// claims already committed events and writes idempotent inbox notifications.
package relay

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"time"
)

var ErrPayloadHashMismatch = errors.New("outbox payload hash does not match its canonical payload")
var ErrInboxPayloadMismatch = errors.New("inbox event id already exists with a different payload hash")

const leaseDuration = 60 * time.Second
const maxAttempts = 8

type FailureMode string

const (
	NoFailure       FailureMode = ""
	FailBeforeInbox FailureMode = "before_inbox"
	FailAfterInbox  FailureMode = "after_inbox_before_ack"
)

type Clock func() time.Time

type Relay struct {
	db  *sql.DB
	now Clock
}

type Result struct {
	Claimed   bool
	Delivered bool
	EventID   string
	Attempts  int
}

type outboxEvent struct {
	ID          string
	Type        string
	RecipientID string
	Payload     string
	PayloadHash string
	Attempts    int
}

type notificationPayload struct {
	EventID       string `json:"event_id"`
	ReservationID uint64 `json:"reservation_id"`
	Status        string `json:"status"`
}

func New(db *sql.DB, now Clock) (*Relay, error) {
	if now == nil {
		now = time.Now
	}
	if _, err := db.Exec("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;"); err != nil {
		return nil, err
	}
	return &Relay{db: db, now: now}, nil
}

// DeliverOnce claims at most one outbox record. An expired lease is eligible
// again, making a stopped relay recoverable without claiming exactly-once.
func (r *Relay) DeliverOnce(ctx context.Context, failure FailureMode) (Result, error) {
	now := r.now().UTC().Truncate(time.Second)
	event, err := r.claim(ctx, now)
	if errors.Is(err, sql.ErrNoRows) {
		return Result{}, nil
	}
	if err != nil {
		return Result{}, err
	}
	if failure == NoFailure {
		failure, err = r.consumeConfiguredFailure(ctx)
		if err != nil {
			return Result{Claimed: true, EventID: event.ID, Attempts: event.Attempts}, err
		}
	}
	result := Result{Claimed: true, EventID: event.ID, Attempts: event.Attempts}
	if failure == FailBeforeInbox {
		cause := errors.New("injected failure before inbox write")
		if err := r.retry(ctx, event, now, cause); err != nil {
			return result, err
		}
		return result, cause
	}
	if err := r.writeInbox(ctx, event, now); err != nil {
		if errors.Is(err, ErrPayloadHashMismatch) || errors.Is(err, ErrInboxPayloadMismatch) {
			if updateErr := r.deadLetter(ctx, event, err); updateErr != nil {
				return result, updateErr
			}
			return result, err
		}
		if updateErr := r.retry(ctx, event, now, err); updateErr != nil {
			return result, updateErr
		}
		return result, err
	}
	if failure == FailAfterInbox {
		cause := errors.New("injected failure after inbox write before ack")
		if err := r.retry(ctx, event, now, cause); err != nil {
			return result, err
		}
		return result, cause
	}
	if err := r.ack(ctx, event.ID, now); err != nil {
		if updateErr := r.retry(ctx, event, now, err); updateErr != nil {
			return result, updateErr
		}
		return result, err
	}
	result.Delivered = true
	return result, nil
}

func (r *Relay) consumeConfiguredFailure(ctx context.Context) (FailureMode, error) {
	tx, err := r.db.BeginTx(ctx, nil)
	if err != nil {
		return NoFailure, err
	}
	defer tx.Rollback()
	var mode string
	var remaining int
	err = tx.QueryRowContext(ctx, "SELECT mode, remaining FROM relay_faults WHERE remaining > 0 LIMIT 1").Scan(&mode, &remaining)
	if errors.Is(err, sql.ErrNoRows) {
		return NoFailure, tx.Commit()
	}
	if err != nil {
		return NoFailure, err
	}
	if _, err := tx.ExecContext(ctx, "UPDATE relay_faults SET remaining=remaining-1 WHERE mode=?1", mode); err != nil {
		return NoFailure, err
	}
	if err := tx.Commit(); err != nil {
		return NoFailure, err
	}
	switch FailureMode(mode) {
	case FailBeforeInbox, FailAfterInbox:
		return FailureMode(mode), nil
	default:
		return NoFailure, fmt.Errorf("unknown configured relay fault %q", mode)
	}
}

func (r *Relay) claim(ctx context.Context, now time.Time) (outboxEvent, error) {
	conn, err := r.db.Conn(ctx)
	if err != nil {
		return outboxEvent{}, err
	}
	defer conn.Close()
	if _, err := conn.ExecContext(ctx, "BEGIN IMMEDIATE"); err != nil {
		return outboxEvent{}, err
	}
	rollback := true
	defer func() {
		if rollback {
			_, _ = conn.ExecContext(context.Background(), "ROLLBACK")
		}
	}()
	row := conn.QueryRowContext(ctx, `SELECT event_id, type, recipient_id, payload, payload_sha256, attempts
        FROM outbox
        WHERE (status IN ('pending', 'retrying') AND next_attempt_at <= ?1)
           OR (status = 'processing' AND lease_until <= ?1)
        ORDER BY occurred_at, event_id LIMIT 1`, now.Unix())
	var event outboxEvent
	if err := row.Scan(&event.ID, &event.Type, &event.RecipientID, &event.Payload, &event.PayloadHash, &event.Attempts); err != nil {
		return outboxEvent{}, err
	}
	if _, err := conn.ExecContext(ctx, `UPDATE outbox
        SET status='processing', attempts=attempts+1, lease_until=?2, last_error=NULL
        WHERE event_id=?1`, event.ID, now.Add(leaseDuration).Unix()); err != nil {
		return outboxEvent{}, err
	}
	if _, err := conn.ExecContext(ctx, "COMMIT"); err != nil {
		return outboxEvent{}, err
	}
	rollback = false
	event.Attempts++
	return event, nil
}

func (r *Relay) writeInbox(ctx context.Context, event outboxEvent, now time.Time) error {
	if hash(event.Payload) != event.PayloadHash {
		return ErrPayloadHashMismatch
	}
	body, err := notificationBody(event.Type, event.Payload)
	if err != nil {
		return err
	}
	tx, err := r.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var priorHash string
	err = tx.QueryRowContext(ctx, "SELECT payload_sha256 FROM inbox WHERE event_id=?1", event.ID).Scan(&priorHash)
	if err == nil {
		if priorHash != event.PayloadHash {
			return ErrInboxPayloadMismatch
		}
		return tx.Commit()
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return err
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO inbox(event_id, recipient_id, body, received_at, payload_sha256)
        VALUES (?1, ?2, ?3, ?4, ?5)`, event.ID, event.RecipientID, body, now.Unix(), event.PayloadHash); err != nil {
		return err
	}
	return tx.Commit()
}

func (r *Relay) ack(ctx context.Context, eventID string, now time.Time) error {
	result, err := r.db.ExecContext(ctx, `UPDATE outbox
        SET status='delivered', lease_until=NULL, delivered_at=?2, next_attempt_at=?2, last_error=NULL
        WHERE event_id=?1 AND status='processing'`, eventID, now.Unix())
	if err != nil {
		return err
	}
	changed, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if changed != 1 {
		return errors.New("outbox lease was lost before acknowledgement")
	}
	return nil
}

func (r *Relay) retry(ctx context.Context, event outboxEvent, now time.Time, cause error) error {
	if event.Attempts >= maxAttempts {
		return r.deadLetter(ctx, event, cause)
	}
	_, err := r.db.ExecContext(ctx, `UPDATE outbox
        SET status='retrying', lease_until=NULL, next_attempt_at=?2, last_error=?3
        WHERE event_id=?1 AND status='processing'`, event.ID, now.Add(backoff(event.Attempts)).Unix(), cause.Error())
	return err
}

func (r *Relay) deadLetter(ctx context.Context, event outboxEvent, cause error) error {
	_, err := r.db.ExecContext(ctx, `UPDATE outbox
        SET status='dead_letter', lease_until=NULL, last_error=?2
        WHERE event_id=?1`, event.ID, cause.Error())
	return err
}

func backoff(attempt int) time.Duration {
	steps := []time.Duration{2, 4, 8, 16, 32, 60, 60, 60}
	if attempt <= 0 {
		return steps[0] * time.Second
	}
	if attempt > len(steps) {
		return steps[len(steps)-1] * time.Second
	}
	return steps[attempt-1] * time.Second
}

func notificationBody(kind, raw string) (string, error) {
	var payload notificationPayload
	if err := json.Unmarshal([]byte(raw), &payload); err != nil {
		return "", fmt.Errorf("decode notification payload: %w", err)
	}
	if payload.EventID == "" {
		return "", errors.New("notification payload has no event_id")
	}
	if payload.ReservationID == 0 {
		return fmt.Sprintf("活动 %s 已更新。", payload.EventID), nil
	}
	return fmt.Sprintf("活动 %s 的预约 #%d：%s。", payload.EventID, payload.ReservationID, kind), nil
}

func hash(value string) string {
	sum := sha256.Sum256([]byte(value))
	return hex.EncodeToString(sum[:])
}
