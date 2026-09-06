package main

import (
	"encoding/json"
	"net/http"
	"strconv"
	"sync/atomic"
	"time"
)

// The upstream is a real concurrent HTTP server with controlled latency. It is
// test equipment, not an additional dependency of a deployed celld worker.
func upstream() http.Handler {
	var requests, completed, active, peak atomic.Int64
	mux := http.NewServeMux()
	mux.HandleFunc("/stats", func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]int64{"requests": requests.Load(), "completed": completed.Load(), "active": active.Load(), "peak": peak.Load()})
	})
	mux.HandleFunc("/reset", func(w http.ResponseWriter, r *http.Request) {
		if active.Load() != 0 {
			http.Error(w, "requests active", 409)
			return
		}
		requests.Store(0)
		completed.Store(0)
		peak.Store(0)
	})
	mux.HandleFunc("/price", func(w http.ResponseWriter, r *http.Request) {
		var args struct {
			Name string `json:"name"`
		}
		delay, err := strconv.Atoi(r.URL.Query().Get("delay_ms"))
		if err != nil || delay < 0 || delay > 1000 || json.NewDecoder(r.Body).Decode(&args) != nil || args.Name == "" {
			http.Error(w, "invalid request", 400)
			return
		}
		requests.Add(1)
		n := active.Add(1)
		for p := peak.Load(); n > p; p = peak.Load() {
			if peak.CompareAndSwap(p, n) {
				break
			}
		}
		defer active.Add(-1)
		select {
		case <-time.After(time.Duration(delay) * time.Millisecond):
		case <-r.Context().Done():
			return
		}
		w.Header().Set("Content-Type", "application/json")
		// Include a realistic small JSON payload that the Python service validates.
		json.NewEncoder(w).Encode(map[string]any{"customer": args.Name, "unit_price_cents": 1999, "currency": "USD", "stock": 12,
			"product": map[string]any{"sku": "sku-001", "name": "Example item", "tags": []string{"available", "standard"}, "description": "A product returned by the pricing and inventory service."}})
		completed.Add(1)
	})
	return mux
}
