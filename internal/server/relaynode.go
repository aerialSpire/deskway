package server

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"time"
)

type relayNodeState struct {
	relayJoined
	Main string `json:"main"`
	Port int    `json:"port"`
}

// RunRelayNode joins the main server on first start, serves TURN, and reports until ctx ends.
func RunRelayNode(ctx context.Context, main, token, data string, cfg TURNConfig) error {
	return runRelayNode(ctx, nil, main, token, data, cfg)
}

func runRelayNode(ctx context.Context, transport http.RoundTripper, main, token, data string, cfg TURNConfig) error {
	u, err := url.Parse(main)
	if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil ||
		u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || (u.Path != "" && u.Path != "/") {
		return errors.New("-join must be the HTTPS origin of the main server")
	}
	main = "https://" + u.Host
	if cfg.PublicIP != "" {
		ip := net.ParseIP(cfg.PublicIP).To4()
		if ip == nil || ip.IsUnspecified() {
			return errors.New("-public-ip must be an IPv4 address")
		}
		cfg.PublicIP = ip.String()
	}
	client := &http.Client{
		Transport: transport,
		Timeout:   10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	path := filepath.Join(data, "relay.json")
	lock, err := lockState(path)
	if err != nil {
		return err
	}
	defer lock.Close()
	state, err := loadRelayNode(ctx, client, main, token, path, cfg)
	if err != nil {
		return err
	}
	cfg.PublicIP, cfg.Secret, cfg.AccountMbps = state.PublicIP, state.Secret, state.AccountMbps
	relay, err := StartTURN(cfg, nil)
	if err != nil {
		return err
	}
	defer relay.Close()
	log.Printf("relay %s serving TURN at %s:%d", state.ID, cfg.PublicIP, cfg.Port)

	tick := time.NewTicker(relayReportInterval)
	defer tick.Stop()
	for ctx.Err() == nil {
		ts := time.Now().Unix()
		report := relayReport{ID: state.ID, TS: ts, MAC: relayReportMAC(state.Secret, state.ID, ts)}
		if err := relayPost(ctx, client, main+"/api/relays/report", report, http.StatusNoContent, nil); err != nil && ctx.Err() == nil {
			log.Printf("relay report: %v", err)
		}
		select {
		case <-ctx.Done():
		case <-tick.C:
		}
	}
	return nil
}

func loadRelayNode(ctx context.Context, client *http.Client, main, token, path string, cfg TURNConfig) (relayNodeState, error) {
	var state relayNodeState
	raw, err := os.ReadFile(path)
	if err == nil {
		if err := json.Unmarshal(raw, &state); err != nil {
			return state, fmt.Errorf("parse %s: %w", path, err)
		}
		if state.Main != main || state.Port != cfg.Port || (cfg.PublicIP != "" && state.PublicIP != cfg.PublicIP) {
			return state, errors.New("relay state belongs to a different main server or TURN address")
		}
		return state, nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return state, err
	}
	if token == "" {
		return state, errors.New("-token is required to join a relay for the first time")
	}
	body := relayJoin{Token: token, PublicIP: cfg.PublicIP, Port: cfg.Port}
	if err := relayPost(ctx, client, main+"/api/relays/join", body, http.StatusOK, &state.relayJoined); err != nil {
		return state, fmt.Errorf("join relay: %w", err)
	}
	state.Main, state.Port = main, cfg.Port
	raw, err = json.Marshal(state)
	if err != nil {
		return state, err
	}
	return state, writeFile(path, raw)
}

func relayPost(ctx context.Context, client *http.Client, endpoint string, body any, status int, answer any) error {
	raw, err := json.Marshal(body)
	if err != nil {
		return err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(raw))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	res, err := client.Do(req)
	if err != nil {
		return err
	}
	defer res.Body.Close()
	if res.StatusCode != status {
		if res.StatusCode == http.StatusBadRequest {
			raw, err := io.ReadAll(io.LimitReader(res.Body, 1024))
			if detail := bytes.TrimSpace(raw); err == nil && len(detail) > 0 {
				return fmt.Errorf("main server returned HTTP %d: %q", res.StatusCode, detail)
			}
		}
		return fmt.Errorf("main server returned HTTP %d", res.StatusCode)
	}
	if answer != nil {
		return json.NewDecoder(res.Body).Decode(answer)
	}
	return nil
}
