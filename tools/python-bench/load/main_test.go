package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestIDsAreUniqueAndBalanced(t *testing.T) {
	seen := map[string]bool{}
	counts := [16]int{}
	for worker := 0; worker < 256; worker++ {
		for seq := 0; seq < 64; seq++ {
			id := callID("test-unique-prefix", worker, seq)
			if seen[id] || slot(id) != (worker+seq)%16 {
				t.Fatal(id, slot(id))
			}
			seen[id] = true
			counts[slot(id)]++
		}
	}
	for _, count := range counts {
		if count != 1024 {
			t.Fatal(counts)
		}
	}
}

func TestHighConcurrencyKeepsOutstandingSlotsBalanced(t *testing.T) {
	fixedSlots = true
	defer func() { fixedSlots = false }()
	counts := [16]int{}
	for worker := 0; worker < 1024; worker++ {
		for seq := 0; seq < 32; seq++ {
			if slot(callID("test-unique-prefix", worker, seq)) != worker%16 {
				t.Fatal(worker, seq)
			}
		}
		counts[worker%16]++
	}
	for _, n := range counts {
		if n != 64 {
			t.Fatal(counts)
		}
	}
}

func TestRejectErrorsIncorrectAndReplayedReplies(t *testing.T) {
	for _, mode := range []string{"good", "incorrect", "status", "replay", "json"} {
		t.Run(mode, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				var args map[string]string
				if err := json.NewDecoder(r.Body).Decode(&args); err != nil {
					t.Error(err)
				}
				if args["name"] != r.Header.Get("X-Celld-Call-Id") {
					t.Error("uncorrelated request")
				}
				if mode == "status" {
					w.WriteHeader(429)
				}
				if mode == "replay" {
					w.Header().Set("X-Celld-Replayed", "true")
				}
				if mode == "json" {
					w.Write([]byte("broken"))
					return
				}
				result := "Hello, " + args["name"]
				if mode == "incorrect" {
					result = "Hello, somebody else"
				}
				json.NewEncoder(w).Encode(map[string]string{"result": result})
			}))
			defer server.Close()
			err := invoke(server.Client(), server.URL, "test-unique-call-00001")
			if (err == nil) != (mode == "good") {
				t.Fatal(mode, err)
			}
		})
	}
}

func TestQuoteChecksPriceCustomerAndRequestContext(t *testing.T) {
	previous := *workload
	*workload = "io"
	defer func() { *workload = previous }()
	for _, broken := range []string{"", "customer", "trace", "total_cents", "currency"} {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			id := r.Header.Get("X-Celld-Call-Id")
			quote := map[string]any{"customer": id, "trace": id, "total_cents": 3998, "currency": "USD"}
			if broken == "total_cents" {
				quote[broken] = 3999
			} else if broken != "" {
				quote[broken] = "wrong"
			}
			json.NewEncoder(w).Encode(map[string]any{"result": quote})
		}))
		err := invoke(server.Client(), server.URL, "test-request-context-00001")
		server.Close()
		if (err == nil) != (broken == "") {
			t.Fatal(broken, err)
		}
	}
}
