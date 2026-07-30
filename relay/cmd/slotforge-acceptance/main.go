package main

import (
	"bytes"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/http/cookiejar"
	"net/url"
	"os/exec"
	"regexp"
	"sync"
	"time"

	_ "github.com/mattn/go-sqlite3"
)

type event struct {
	EventID           string `json:"event_id"`
	Title             string `json:"title"`
	Capacity          int    `json:"capacity"`
	StartsAt          int64  `json:"starts_at"`
	EndsAt            int64  `json:"ends_at"`
	BookingCloseAt    int64  `json:"booking_close_at"`
	Active            bool   `json:"active"`
	Revision          int    `json:"revision"`
	ConfirmedCount    int    `json:"confirmed_count"`
	RemainingCapacity int    `json:"remaining_capacity"`
}

type reservation struct {
	ReservationID uint64 `json:"reservation_id"`
	EventID       string `json:"event_id"`
	Status        string `json:"status"`
}

type reservationReceipt struct {
	ReservationID uint64 `json:"reservation_id"`
	EventID       string `json:"event_id"`
	Status        string `json:"status"`
	Waitlist      *int   `json:"waitlist_position"`
	Replayed      bool   `json:"replayed"`
	EventRevision uint64 `json:"event_revision"`
}

type cancellationReceipt struct {
	ReservationID uint64   `json:"reservation_id"`
	Promoted      []uint64 `json:"promoted"`
}

type notification struct {
	NotificationID int64  `json:"notification_id"`
	EventID        string `json:"event_id"`
	Body           string `json:"body"`
}

type delivery struct {
	EventID   string `json:"event_id"`
	Status    string `json:"status"`
	Attempts  int    `json:"attempts"`
	LastError string `json:"last_error"`
}

type createEventRequest struct {
	EventID        string `json:"event_id"`
	Title          string `json:"title"`
	Capacity       int    `json:"capacity"`
	StartsAt       int64  `json:"starts_at"`
	EndsAt         int64  `json:"ends_at"`
	BookingCloseAt int64  `json:"booking_close_at"`
}

func main() {
	baseURL := flag.String("base-url", "http://127.0.0.1:5204", "running SlotForge base URL")
	databasePath := flag.String("database", "runtime/slotforge/slotforge.sqlite", "SlotForge SQLite database")
	relayBinary := flag.String("relay", "", "built slotforge-relay executable")
	flag.Parse()
	if *relayBinary == "" {
		fatal("--relay is required")
	}
	if err := run(*baseURL, *databasePath, *relayBinary); err != nil {
		fatal(err.Error())
	}
}

func run(baseURL, databasePath, relayBinary string) error {
	database, err := sql.Open("sqlite3", databasePath+"?_busy_timeout=5000&_journal_mode=WAL")
	if err != nil {
		return err
	}
	defer database.Close()
	if err := database.Ping(); err != nil {
		return fmt.Errorf("open acceptance database: %w", err)
	}

	attendeeA, err := session(baseURL, "demo-a")
	if err != nil {
		return err
	}
	attendeeB, err := session(baseURL, "demo-b")
	if err != nil {
		return err
	}
	attendeeC, err := session(baseURL, "demo-c")
	if err != nil {
		return err
	}
	coordinator, err := session(baseURL, "demo-coordinator")
	if err != nil {
		return err
	}

	if err := verifyBuiltWorkbench(baseURL); err != nil {
		return err
	}
	events, status, err := request[[]event](attendeeA, http.MethodGet, baseURL+"/api/v1/events", nil, nil)
	if err != nil || status != http.StatusOK {
		return fmt.Errorf("list events: status=%d err=%v", status, err)
	}
	workshop, ok := findEvent(events, "workshop-a")
	if !ok {
		return fmt.Errorf("fresh runtime is missing seeded workshop-a")
	}

	first, err := reserve(attendeeA, baseURL, workshop.EventID, "acceptance-a")
	if err != nil || first.Status != "confirmed" {
		return fmt.Errorf("first attendee did not get a confirmed seat: receipt=%+v err=%v", first, err)
	}
	second, err := reserve(attendeeB, baseURL, workshop.EventID, "acceptance-b")
	if err != nil || second.Status != "confirmed" {
		return fmt.Errorf("second attendee did not get a confirmed seat: receipt=%+v err=%v", second, err)
	}
	third, err := reserve(attendeeC, baseURL, workshop.EventID, "acceptance-c")
	if err != nil || third.Status != "waitlisted" || third.Waitlist == nil || *third.Waitlist != 1 {
		return fmt.Errorf("third attendee did not enter FIFO waitlist position 1: receipt=%+v err=%v", third, err)
	}

	updatedEvents, status, err := request[[]event](attendeeA, http.MethodGet, baseURL+"/api/v1/events", nil, nil)
	if err != nil || status != http.StatusOK {
		return fmt.Errorf("read availability: status=%d err=%v", status, err)
	}
	workshop, _ = findEvent(updatedEvents, workshop.EventID)
	if workshop.ConfirmedCount != 2 || workshop.RemainingCapacity != 0 {
		return fmt.Errorf("public availability disagrees with reservations: %+v", workshop)
	}

	var cancelled cancellationReceipt
	status, err = requestInto(attendeeA, http.MethodPost, fmt.Sprintf("%s/api/v1/reservations/%d/cancel", baseURL, first.ReservationID), nil, map[string]string{"idempotency-key": "acceptance-cancel-a"}, &cancelled)
	if err != nil || status != http.StatusOK || !containsID(cancelled.Promoted, third.ReservationID) {
		return fmt.Errorf("cancel did not promote FIFO attendee C: status=%d receipt=%+v err=%v", status, cancelled, err)
	}
	cReservations, status, err := request[[]reservation](attendeeC, http.MethodGet, baseURL+"/api/v1/me/reservations", nil, nil)
	if err != nil || status != http.StatusOK || !hasReservation(cReservations, third.ReservationID, "confirmed") {
		return fmt.Errorf("promoted attendee C does not read confirmed state: status=%d err=%v", status, err)
	}

	if err := verifyConcurrentLastSeat(baseURL, coordinator, attendeeA, attendeeB); err != nil {
		return err
	}
	if status, _, err := requestRaw(attendeeA, http.MethodGet, baseURL+"/api/v1/admin/deliveries", nil, nil); err != nil || status != http.StatusForbidden {
		return fmt.Errorf("participant delivery access should be forbidden: status=%d err=%v", status, err)
	}

	if status, _, err := requestRaw(coordinator, http.MethodPost, baseURL+"/api/v1/admin/demo/relay-fault", map[string]string{"mode": "after_inbox_before_ack"}, nil); err != nil || status != http.StatusNoContent {
		return fmt.Errorf("configure after-inbox fault: status=%d err=%v", status, err)
	}
	if output, err := runRelay(relayBinary, databasePath); err != nil {
		return fmt.Errorf("relay with one-shot fault: %w (%s)", err, output)
	}
	faultEventID, attempts, nextAttempt, err := latestRetryingDelivery(database)
	if err != nil {
		return err
	}
	if attempts != 1 || inboxCount(database, faultEventID) != 1 {
		return fmt.Errorf("after-inbox fault did not preserve a single inbox row: event=%s attempts=%d inbox=%d", faultEventID, attempts, inboxCount(database, faultEventID))
	}
	if wait := time.Until(time.Unix(nextAttempt, 0)); wait > 0 {
		time.Sleep(wait + 150*time.Millisecond)
	}
	if output, err := runRelay(relayBinary, databasePath); err != nil {
		return fmt.Errorf("relay retry after inbox fault: %w (%s)", err, output)
	}
	if status, attemptCount, err := deliveryState(database, faultEventID); err != nil || status != "delivered" || attemptCount != 2 || inboxCount(database, faultEventID) != 1 {
		return fmt.Errorf("inbox fault recovery failed: status=%s attempts=%d inbox=%d err=%v", status, attemptCount, inboxCount(database, faultEventID), err)
	}
	if err := drainReadyDeliveries(database, relayBinary, databasePath); err != nil {
		return err
	}
	notifications, status, err := request[[]notification](attendeeC, http.MethodGet, baseURL+"/api/v1/me/notifications", nil, nil)
	if err != nil || status != http.StatusOK || !hasNotification(notifications, workshop.EventID) {
		return fmt.Errorf("promoted attendee notification was not delivered to the local inbox: status=%d err=%v", status, err)
	}

	if err := verifyDeadLetterRetry(baseURL, database, relayBinary, databasePath, coordinator, attendeeA); err != nil {
		return err
	}
	fmt.Println("PASS fresh-runtime booking, FIFO promotion, API availability, concurrent last seat, inbox replay, and coordinator dead-letter retry")
	return nil
}

func verifyConcurrentLastSeat(baseURL string, coordinator, attendeeA, attendeeB *http.Client) error {
	now := time.Now().Unix()
	id := fmt.Sprintf("acceptance-race-%d", now)
	created := createEventRequest{
		EventID: id, Title: "Acceptance concurrent last seat", Capacity: 1,
		StartsAt: now + 4*3600, EndsAt: now + 5*3600, BookingCloseAt: now + 3*3600,
	}
	var eventResult event
	status, err := requestInto(coordinator, http.MethodPost, baseURL+"/api/v1/events", created, map[string]string{"idempotency-key": "acceptance-create-race"}, &eventResult)
	if err != nil || status != http.StatusCreated {
		return fmt.Errorf("create concurrent-capacity event: status=%d err=%v", status, err)
	}
	results := make(chan reservationResult, 2)
	var wait sync.WaitGroup
	for _, item := range []struct {
		client *http.Client
		key    string
	}{{attendeeA, "acceptance-race-a"}, {attendeeB, "acceptance-race-b"}} {
		wait.Add(1)
		go func(client *http.Client, requestKey string) {
			defer wait.Done()
			receipt, err := reserve(client, baseURL, id, requestKey)
			results <- reservationResult{receipt: receipt, err: err}
		}(item.client, item.key)
	}
	wait.Wait()
	close(results)
	confirmed, waitlisted := 0, 0
	for result := range results {
		if result.err != nil {
			return fmt.Errorf("concurrent reservation failed: %w", result.err)
		}
		switch result.receipt.Status {
		case "confirmed":
			confirmed++
		case "waitlisted":
			waitlisted++
		default:
			return fmt.Errorf("unexpected concurrent reservation status %q", result.receipt.Status)
		}
	}
	if confirmed != 1 || waitlisted != 1 {
		return fmt.Errorf("last-seat race oversold or lost a request: confirmed=%d waitlisted=%d", confirmed, waitlisted)
	}
	var roster []reservation
	status, err = requestInto(coordinator, http.MethodGet, baseURL+"/api/v1/admin/events/"+url.PathEscape(id)+"/roster", nil, nil, &roster)
	if err != nil || status != http.StatusOK {
		return fmt.Errorf("read concurrent event roster: status=%d err=%v", status, err)
	}
	confirmed, waitlisted = 0, 0
	for _, item := range roster {
		if item.Status == "confirmed" {
			confirmed++
		}
		if item.Status == "waitlisted" {
			waitlisted++
		}
	}
	if confirmed != 1 || waitlisted != 1 {
		return fmt.Errorf("SQLite-backed roster disagrees with concurrent receipts: confirmed=%d waitlisted=%d", confirmed, waitlisted)
	}
	events, status, err := request[[]event](coordinator, http.MethodGet, baseURL+"/api/v1/events", nil, nil)
	if err != nil || status != http.StatusOK {
		return fmt.Errorf("read concurrent event availability: status=%d err=%v", status, err)
	}
	available, ok := findEvent(events, id)
	if !ok || available.ConfirmedCount != 1 || available.RemainingCapacity != 0 {
		return fmt.Errorf("concurrent public availability is wrong: %+v", available)
	}
	return nil
}

func verifyDeadLetterRetry(baseURL string, database *sql.DB, relayBinary, databasePath string, coordinator, attendee *http.Client) error {
	var eventID, payload string
	if err := database.QueryRow(`SELECT event_id, payload FROM outbox
		WHERE type LIKE 'reservation.%' AND status='delivered'
		ORDER BY occurred_at DESC, event_id DESC LIMIT 1`).Scan(&eventID, &payload); err != nil {
		return fmt.Errorf("find delivered reservation event for dead-letter exercise: %w", err)
	}
	if _, err := database.Exec(`UPDATE outbox SET status='pending', payload_sha256='acceptance-corrupt-hash',
		next_attempt_at=?2, delivered_at=NULL, last_error=NULL WHERE event_id=?1`, eventID, time.Now().Unix()); err != nil {
		return fmt.Errorf("inject acceptance-only payload corruption: %w", err)
	}
	if output, err := runRelay(relayBinary, databasePath); err != nil {
		return fmt.Errorf("relay should move the corrupt payload to dead-letter: %w (%s)", err, output)
	}
	status, attempts, err := deliveryState(database, eventID)
	if err != nil || status != "dead_letter" || attempts < 1 {
		return fmt.Errorf("integrity failure did not create a dead-letter: status=%s attempts=%d err=%v", status, attempts, err)
	}
	checksum := sha256.Sum256([]byte(payload))
	if _, err := database.Exec("UPDATE outbox SET payload_sha256=?2 WHERE event_id=?1 AND status='dead_letter'", eventID, hex.EncodeToString(checksum[:])); err != nil {
		return fmt.Errorf("restore the isolated acceptance fixture payload hash: %w", err)
	}
	var deliveries []delivery
	statusCode, err := requestInto(coordinator, http.MethodGet, baseURL+"/api/v1/admin/deliveries", nil, nil, &deliveries)
	if err != nil || statusCode != http.StatusOK || !hasDelivery(deliveries, eventID, "dead_letter") {
		return fmt.Errorf("coordinator cannot read the dead-letter before retry: status=%d err=%v", statusCode, err)
	}
	statusCode, _, err = requestRaw(attendee, http.MethodPost, baseURL+"/api/v1/admin/deliveries/"+url.PathEscape(eventID)+"/retry", nil, nil)
	if err != nil || statusCode != http.StatusForbidden {
		return fmt.Errorf("participant must not retry a dead-letter: status=%d err=%v", statusCode, err)
	}
	statusCode, body, err := requestRaw(coordinator, http.MethodPost, baseURL+"/api/v1/admin/deliveries/"+url.PathEscape(eventID)+"/retry", nil, nil)
	if err != nil || statusCode != http.StatusNoContent {
		return fmt.Errorf("coordinator dead-letter retry failed: status=%d body=%s err=%v", statusCode, body, err)
	}
	status, attempts, err = deliveryState(database, eventID)
	if err != nil || status != "pending" || attempts != 0 {
		return fmt.Errorf("manual retry did not reset dead-letter for delivery: status=%s attempts=%d err=%v", status, attempts, err)
	}
	if output, err := runRelay(relayBinary, databasePath); err != nil {
		return fmt.Errorf("relay after coordinator retry: %w (%s)", err, output)
	}
	status, attempts, err = deliveryState(database, eventID)
	if err != nil || status != "delivered" || attempts != 1 || inboxCount(database, eventID) != 1 {
		return fmt.Errorf("manual dead-letter retry did not redeliver exactly once: status=%s attempts=%d inbox=%d err=%v", status, attempts, inboxCount(database, eventID), err)
	}
	return nil
}

type reservationResult struct {
	receipt reservationReceipt
	err     error
}

func session(baseURL, actor string) (*http.Client, error) {
	jar, err := cookiejar.New(nil)
	if err != nil {
		return nil, err
	}
	client := &http.Client{Jar: jar, Timeout: 10 * time.Second}
	var response map[string]string
	status, err := requestInto(client, http.MethodPost, baseURL+"/api/v1/demo/session", map[string]string{"actor_key": actor}, nil, &response)
	if err != nil || status != http.StatusOK {
		return nil, fmt.Errorf("open demo session %s: status=%d err=%v", actor, status, err)
	}
	return client, nil
}

func reserve(client *http.Client, baseURL, eventID, requestKey string) (reservationReceipt, error) {
	var receipt reservationReceipt
	status, err := requestInto(client, http.MethodPost, baseURL+"/api/v1/reservations", map[string]string{"event_id": eventID, "cohort": "acceptance"}, map[string]string{"idempotency-key": requestKey}, &receipt)
	if err != nil || status != http.StatusOK {
		return receipt, fmt.Errorf("reserve event %s: status=%d err=%v", eventID, status, err)
	}
	return receipt, nil
}

func request[T any](client *http.Client, method, endpoint string, payload any, headers map[string]string) (T, int, error) {
	var value T
	status, body, err := requestRaw(client, method, endpoint, payload, headers)
	if err != nil {
		return value, status, err
	}
	if len(body) > 0 {
		if err := json.Unmarshal(body, &value); err != nil {
			return value, status, fmt.Errorf("decode %s response: %w", endpoint, err)
		}
	}
	return value, status, nil
}

func requestInto(client *http.Client, method, endpoint string, payload any, headers map[string]string, target any) (int, error) {
	status, body, err := requestRaw(client, method, endpoint, payload, headers)
	if err != nil {
		return status, err
	}
	if len(body) > 0 && target != nil {
		if err := json.Unmarshal(body, target); err != nil {
			return status, fmt.Errorf("decode %s response: %w", endpoint, err)
		}
	}
	return status, nil
}

func requestRaw(client *http.Client, method, endpoint string, payload any, headers map[string]string) (int, []byte, error) {
	var body io.Reader
	if payload != nil {
		encoded, err := json.Marshal(payload)
		if err != nil {
			return 0, nil, err
		}
		body = bytes.NewReader(encoded)
	}
	request, err := http.NewRequest(method, endpoint, body)
	if err != nil {
		return 0, nil, err
	}
	if payload != nil {
		request.Header.Set("content-type", "application/json")
	}
	for key, value := range headers {
		request.Header.Set(key, value)
	}
	response, err := client.Do(request)
	if err != nil {
		return 0, nil, err
	}
	defer response.Body.Close()
	responseBody, err := io.ReadAll(response.Body)
	return response.StatusCode, responseBody, err
}

func verifyBuiltWorkbench(baseURL string) error {
	client := &http.Client{Timeout: 10 * time.Second}
	response, err := client.Get(baseURL + "/")
	if err != nil {
		return fmt.Errorf("load workbench: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("workbench returned HTTP %d", response.StatusCode)
	}
	html, err := io.ReadAll(response.Body)
	if err != nil {
		return err
	}
	assetPath := regexp.MustCompile(`src="([^"]+\.js)"`).FindSubmatch(html)
	if len(assetPath) != 2 {
		return fmt.Errorf("workbench HTML does not reference a built JavaScript asset")
	}
	assetURL, err := url.Parse(string(assetPath[1]))
	if err != nil {
		return err
	}
	assetURL = response.Request.URL.ResolveReference(assetURL)
	assetResponse, err := client.Get(assetURL.String())
	if err != nil {
		return fmt.Errorf("load workbench JavaScript: %w", err)
	}
	defer assetResponse.Body.Close()
	if assetResponse.StatusCode != http.StatusOK {
		return fmt.Errorf("workbench JavaScript returned HTTP %d", assetResponse.StatusCode)
	}
	asset, err := io.ReadAll(assetResponse.Body)
	if err != nil {
		return err
	}
	for _, label := range []string{"重新投递", "dead_letter", "按周", "时间冲突提醒", "历史记录"} {
		if !bytes.Contains(asset, []byte(label)) {
			return fmt.Errorf("built workbench is missing UI copy %q", label)
		}
	}
	return nil
}

func runRelay(binary, database string) (string, error) {
	command := exec.Command(binary, "--database", database, "--once")
	output, err := command.CombinedOutput()
	return string(output), err
}

func latestRetryingDelivery(database *sql.DB) (string, int, int64, error) {
	var eventID string
	var attempts int
	var nextAttempt int64
	err := database.QueryRow(`SELECT event_id, attempts, next_attempt_at FROM outbox
		WHERE status='retrying' ORDER BY occurred_at, event_id LIMIT 1`).Scan(&eventID, &attempts, &nextAttempt)
	if err != nil {
		return "", 0, 0, fmt.Errorf("read after-inbox retry: %w", err)
	}
	return eventID, attempts, nextAttempt, nil
}

func deliveryState(database *sql.DB, eventID string) (string, int, error) {
	var status string
	var attempts int
	err := database.QueryRow("SELECT status, attempts FROM outbox WHERE event_id=?1", eventID).Scan(&status, &attempts)
	return status, attempts, err
}

func inboxCount(database *sql.DB, eventID string) int {
	var count int
	if err := database.QueryRow("SELECT COUNT(*) FROM inbox WHERE event_id=?1", eventID).Scan(&count); err != nil {
		return -1
	}
	return count
}

func drainReadyDeliveries(database *sql.DB, relayBinary, databasePath string) error {
	for attempt := 0; attempt < 100; attempt++ {
		var ready int
		if err := database.QueryRow(`SELECT COUNT(*) FROM outbox
			WHERE status IN ('pending','retrying') AND next_attempt_at <= ?1`, time.Now().Unix()).Scan(&ready); err != nil {
			return err
		}
		if ready == 0 {
			return nil
		}
		if output, err := runRelay(relayBinary, databasePath); err != nil {
			return fmt.Errorf("drain relay deliveries: %w (%s)", err, output)
		}
	}
	return fmt.Errorf("relay did not drain ready events after 100 one-shot runs")
}

func findEvent(events []event, eventID string) (event, bool) {
	for _, item := range events {
		if item.EventID == eventID {
			return item, true
		}
	}
	return event{}, false
}

func hasReservation(items []reservation, reservationID uint64, status string) bool {
	for _, item := range items {
		if item.ReservationID == reservationID && item.Status == status {
			return true
		}
	}
	return false
}

func hasNotification(items []notification, eventID string) bool {
	for _, item := range items {
		if item.EventID == eventID {
			return true
		}
	}
	return false
}

func hasDelivery(items []delivery, eventID, status string) bool {
	for _, item := range items {
		if item.EventID == eventID && item.Status == status {
			return true
		}
	}
	return false
}

func containsID(items []uint64, target uint64) bool {
	for _, item := range items {
		if item == target {
			return true
		}
	}
	return false
}

func fatal(message string) {
	log.Fatal(message)
}
