package server

import (
	"crypto/tls"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"
	"time"

	"github.com/pion/turn/v5"
)

func splitForTest(t *testing.T, timeout time.Duration) (net.Listener, net.Listener, *tls.Config, *tls.Config) {
	t.Helper()
	fixture := httptest.NewTLSServer(nil)
	fixture.Close()
	serverTLS := &tls.Config{Certificates: fixture.TLS.Certificates, NextProtos: []string{"h2", "http/1.1"}}
	clientTLS := fixture.Client().Transport.(*http.Transport).TLSClientConfig
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	web, relay := SplitTLS(listener, serverTLS.Clone(), timeout)
	t.Cleanup(func() { web.Close() })
	return web, relay, serverTLS, clientTLS
}

func TestSplitTLSRoutesWithoutConsumingData(t *testing.T) {
	for _, tt := range []struct {
		name, alpn, payload string
		turn                bool
	}{
		{name: "HTTP/1.1 by ALPN", alpn: "http/1.1"},
		{name: "HTTP/2 by ALPN", alpn: "h2"},
		{name: "HTTP/1.1 by first byte", payload: "GET / HTTP/1.1\r\n"},
		{name: "STUN by first byte", payload: "\x00\x03\x00\x00", turn: true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			web, relay, _, clientTLS := splitForTest(t, time.Second)
			if tt.alpn != "" {
				clientTLS.NextProtos = []string{tt.alpn}
			}
			client, err := tls.Dial("tcp", web.Addr().String(), clientTLS)
			if err != nil {
				t.Fatal(err)
			}
			defer client.Close()
			if got := client.ConnectionState().NegotiatedProtocol; got != tt.alpn {
				t.Fatalf("ALPN = %q, want %q", got, tt.alpn)
			}
			if tt.payload != "" {
				if _, err := io.WriteString(client, tt.payload); err != nil {
					t.Fatal(err)
				}
			}

			listener := web
			if tt.turn {
				listener = relay
			}
			accepted := make(chan net.Conn, 1)
			go func() {
				conn, _ := listener.Accept()
				if conn != nil {
					accepted <- conn
				}
			}()
			select {
			case conn := <-accepted:
				defer conn.Close()
				_ = conn.SetReadDeadline(time.Now().Add(time.Second))
				got := make([]byte, len(tt.payload))
				if _, err := io.ReadFull(conn, got); err != nil || string(got) != tt.payload {
					t.Fatalf("read %q, error %v, want %q", got, err, tt.payload)
				}
			case <-time.After(2 * time.Second):
				t.Fatal("connection was not routed")
			}
		})
	}
}

func TestSplitTLSClosesIdleConnection(t *testing.T) {
	web, _, _, clientTLS := splitForTest(t, 250*time.Millisecond)
	dialer := &net.Dialer{Timeout: time.Second}
	for _, tt := range []struct {
		name string
		dial func() (net.Conn, error)
	}{
		{"before handshake", func() (net.Conn, error) { return dialer.Dial("tcp", web.Addr().String()) }},
		{"after handshake", func() (net.Conn, error) { return tls.DialWithDialer(dialer, "tcp", web.Addr().String(), clientTLS) }},
	} {
		t.Run(tt.name, func(t *testing.T) {
			conn, err := tt.dial()
			if err != nil {
				t.Fatal(err)
			}
			defer conn.Close()
			_ = conn.SetReadDeadline(time.Now().Add(time.Second))
			_, err = conn.Read(make([]byte, 1))
			if err == nil || errors.Is(err, os.ErrDeadlineExceeded) {
				t.Fatalf("idle connection should close before the client deadline, got %v", err)
			}
		})
	}
}

func TestTURNAndHTTPSShareTheTLSListener(t *testing.T) {
	web, secure, serverTLS, clientTLS := splitForTest(t, 2*time.Second)
	_, port, err := net.SplitHostPort(web.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	cfg := TURNConfig{PublicIP: "127.0.0.1", MinPort: 49160, MaxPort: 49359, Secret: "test", TLSAddr: net.JoinHostPort("localhost", port)}
	relay, err := StartTURN(cfg, secure)
	if err != nil {
		t.Fatal(err)
	}
	defer relay.Close()
	srv := &http.Server{TLSConfig: serverTLS, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.TLS == nil {
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		io.WriteString(w, "deskway")
	})}
	go srv.Serve(web)
	defer srv.Close()

	ice := ICEProvider(&cfg, nil)("test-account")[0]
	if got, want := ice.URLs[len(ice.URLs)-1], "turns:localhost:"+port+"?transport=tcp"; got != want {
		t.Fatalf("TLS TURN URL = %q, want %q", got, want)
	}
	addr := web.Addr().String()
	conn, err := tls.DialWithDialer(&net.Dialer{Timeout: time.Second}, "tcp", addr, clientTLS)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	client, err := turn.NewClient(&turn.ClientConfig{
		Conn: turn.NewSTUNConn(conn), TURNServerAddr: addr, Username: ice.Username, Password: ice.Credential, RTO: 20 * time.Millisecond,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	if err := client.Listen(); err != nil {
		t.Fatal(err)
	}
	allocation, err := client.Allocate()
	if err != nil {
		t.Fatal(err)
	}
	defer allocation.Close()
	if got := allocation.LocalAddr().(*net.UDPAddr); !got.IP.IsLoopback() || got.Port < int(cfg.MinPort) || got.Port > int(cfg.MaxPort) {
		t.Fatalf("unexpected relayed address %s", got)
	}

	for _, major := range []int{1, 2} {
		transport := &http.Transport{TLSClientConfig: clientTLS.Clone(), ForceAttemptHTTP2: major == 2}
		defer transport.CloseIdleConnections()
		httpClient := &http.Client{Transport: transport, Timeout: 2 * time.Second}
		resp, err := httpClient.Get("https://" + addr)
		if err != nil {
			t.Fatal(err)
		}
		body, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil || string(body) != "deskway" || resp.ProtoMajor != major {
			t.Fatalf("HTTPS response: %s %s %q (%v), want HTTP/%d and deskway", resp.Proto, resp.Status, body, err, major)
		}
	}
}
