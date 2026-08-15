package main

import (
	"context"
	"database/sql"
	"flag"
	"fmt"
	"log"
	"time"

	"github.com/jstar/slotforge-relay/internal/relay"
	_ "github.com/mattn/go-sqlite3"
)

func main() {
	database := flag.String("database", "runtime/slotforge/slotforge.sqlite", "SlotForge SQLite database path")
	once := flag.Bool("once", false, "claim and process at most one delivery")
	poll := flag.Duration("poll", time.Second, "idle polling interval in loop mode")
	failBefore := flag.Bool("fail-before-inbox", false, "inject one failure after a lease and before inbox write")
	failAfter := flag.Bool("fail-after-inbox-before-ack", false, "inject one failure after inbox write and before outbox acknowledgement")
	flag.Parse()
	if *failBefore && *failAfter {
		log.Fatal("choose at most one injected relay failure")
	}
	db, err := sql.Open("sqlite3", *database+"?_busy_timeout=5000&_journal_mode=WAL")
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	r, err := relay.New(db, nil)
	if err != nil {
		log.Fatal(err)
	}
	mode := relay.NoFailure
	if *failBefore {
		mode = relay.FailBeforeInbox
	}
	if *failAfter {
		mode = relay.FailAfterInbox
	}
	for {
		result, err := r.DeliverOnce(context.Background(), mode)
		if err != nil {
			log.Printf("relay event=%s attempt=%d: %v", result.EventID, result.Attempts, err)
		} else if result.Claimed {
			fmt.Printf("delivered event=%s attempt=%d\n", result.EventID, result.Attempts)
		}
		if *once {
			return
		}
		mode = relay.NoFailure
		time.Sleep(*poll)
	}
}
