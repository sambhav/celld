// A closed-loop HTTP/1.1 keep-alive driver. Every reply is checked against its
// unique input. There are no retries and replayed replies fail the measurement.
package main

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

func slot(id string) int {
	n := 0
	for _, c := range id {
		n = (n*31 + int(c)) % 16
	}
	return n
}

var fixedSlots bool

func callID(prefix string, worker, seq int) string {
	id := fmt.Sprintf("%s-%04x-%08x-", prefix, worker, seq)
	want := (worker + seq) % 16
	if fixedSlots {
		want = worker % 16
	}
	adjust := ((want-slot(id)*31-65)%16 + 16) % 16
	return id + string(rune(65+adjust))
}

var workload = flag.String("workload", "hello", "hello or io")

func invoke(client *http.Client, url, id string) error {
	body, _ := json.Marshal(map[string]string{"name": id})
	req, err := http.NewRequest("POST", url, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Celld-Call-Id", id)
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return err
	}
	if resp.StatusCode != 200 || resp.Header.Get("X-Celld-Replayed") != "" {
		return fmt.Errorf("status=%d replay=%q body=%.200s", resp.StatusCode, resp.Header.Get("X-Celld-Replayed"), raw)
	}
	var reply struct {
		Result json.RawMessage `json:"result"`
	}
	if err := json.Unmarshal(raw, &reply); err != nil {
		return err
	}
	if *workload == "io" {
		var quote struct {
			Customer string `json:"customer"`
			Total    int    `json:"total_cents"`
			Currency string `json:"currency"`
			Trace    string `json:"trace"`
		}
		if err := json.Unmarshal(reply.Result, &quote); err != nil {
			return err
		}
		if quote.Customer != id || quote.Total != 3998 || quote.Currency != "USD" || quote.Trace != id {
			return fmt.Errorf("incorrect quote: %.200s", raw)
		}
	} else {
		var greeting string
		if err := json.Unmarshal(reply.Result, &greeting); err != nil {
			return err
		}
		if greeting != "Hello, "+id {
			return fmt.Errorf("incorrect reply: %.200s", raw)
		}
	}
	return nil
}

func cpuSeconds() float64 {
	var r syscall.Rusage
	if err := syscall.Getrusage(syscall.RUSAGE_SELF, &r); err != nil {
		panic(err)
	}
	return float64(r.Utime.Sec+r.Stime.Sec) + float64(r.Utime.Usec+r.Stime.Usec)/1e6
}

func main() {
	url := flag.String("url", "", "hello endpoint")
	clients := flag.Int("clients", 16, "concurrent clients")
	seconds := flag.Float64("seconds", 10, "timed sampling seconds")
	count := flag.Int("count", 0, "fixed calls/client for untimed warmup")
	serve := flag.String("serve", "", "run the controlled upstream HTTP fixture")
	flag.Parse()
	if *serve != "" {
		if err := http.ListenAndServe(*serve, upstream()); err != nil {
			panic(err)
		}
		return
	}
	if *url == "" || *clients < 1 || *seconds <= 0 || *count < 0 {
		panic("invalid arguments")
	}
	// At high concurrency, keep equal outstanding work per cell. Otherwise a
	// slower cell collects most of a closed-loop driver's clients and hits its
	// 64-request admission limit before the rest of the pool is saturated.
	fixedSlots = *clients >= 16
	transport := &http.Transport{MaxIdleConns: *clients, MaxIdleConnsPerHost: *clients,
		MaxConnsPerHost: *clients, DisableCompression: true, ForceAttemptHTTP2: false}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 60 * time.Second}
	var random [16]byte
	if _, err := rand.Read(random[:]); err != nil {
		panic(err)
	}
	prefix := hex.EncodeToString(random[:])
	startGate := make(chan struct{})
	var wg sync.WaitGroup
	var failed atomic.Bool
	var firstError string
	var once sync.Once
	latencies := make([][]int64, *clients)
	var deadline time.Time
	for worker := 0; worker < *clients; worker++ {
		wg.Add(1)
		go func(worker int) {
			defer wg.Done()
			<-startGate
			for seq := 0; !failed.Load(); seq++ {
				if (*count > 0 && seq >= *count) || (*count == 0 && !time.Now().Before(deadline)) {
					break
				}
				id := callID(prefix, worker, seq)
				start := time.Now()
				if err := invoke(client, *url, id); err != nil {
					once.Do(func() { firstError = err.Error() })
					failed.Store(true)
					break
				}
				latencies[worker] = append(latencies[worker], time.Since(start).Nanoseconds())
			}
		}(worker)
	}
	cpuStart := cpuSeconds()
	start := time.Now()
	deadline = start.Add(time.Duration(*seconds * 1e9))
	close(startGate)
	wg.Wait()
	elapsed := time.Since(start).Seconds()
	cpu := cpuSeconds() - cpuStart
	var all []int64
	for _, values := range latencies {
		all = append(all, values...)
	}
	sort.Slice(all, func(i, j int) bool { return all[i] < all[j] })
	percentile := func(p float64) float64 {
		if len(all) == 0 {
			return 0
		}
		return float64(all[int(float64(len(all)-1)*p)]) / 1e6
	}
	report := map[string]any{"clients": *clients, "requests": len(all), "elapsed_seconds": elapsed,
		"rps": float64(len(all)) / elapsed, "p50_ms": percentile(.5), "p95_ms": percentile(.95),
		"p99_ms": percentile(.99), "client_cpu_cores": cpu / elapsed, "errors": 0}
	if failed.Load() {
		report["errors"] = 1
		report["first_error"] = firstError
	}
	if err := json.NewEncoder(os.Stdout).Encode(report); err != nil {
		panic(err)
	}
	if failed.Load() {
		os.Exit(1)
	}
}
