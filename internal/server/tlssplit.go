package server

import (
	"bufio"
	"crypto/tls"
	"net"
	"time"
)

// SplitTLS terminates TLS once and hands connections to the HTTP and TURN servers.
// Both listeners share the same lifetime: closing either closes the socket.
func SplitTLS(listener net.Listener, config *tls.Config, timeout time.Duration) (http, turn net.Listener) {
	split := &tlsSplit{Listener: tls.NewListener(listener, config), done: make(chan struct{})}
	web := &splitListener{tlsSplit: split, conns: make(chan net.Conn)}
	relay := &splitListener{tlsSplit: split, conns: make(chan net.Conn)}
	go func() {
		defer close(split.done)
		for {
			conn, err := split.Listener.Accept()
			if err != nil {
				split.err = err
				return
			}
			go routeTLS(conn.(*tls.Conn), web.conns, relay.conns, split.done, timeout)
		}
	}()
	return web, relay
}

type tlsSplit struct {
	net.Listener
	done chan struct{}
	err  error
}

type splitListener struct {
	*tlsSplit
	conns chan net.Conn
}

func (l *splitListener) Accept() (net.Conn, error) {
	select {
	case conn := <-l.conns:
		return conn, nil
	case <-l.done:
		return nil, l.err
	}
}

type bufferedTLS struct {
	*tls.Conn
	reader *bufio.Reader
}

func (c bufferedTLS) Read(p []byte) (int, error) { return c.reader.Read(p) }

func routeTLS(conn *tls.Conn, web, relay chan<- net.Conn, done <-chan struct{}, timeout time.Duration) {
	handedOff := false
	defer func() {
		if !handedOff {
			conn.Close()
		}
	}()
	if err := conn.SetDeadline(time.Now().Add(timeout)); err != nil {
		return
	}
	if err := conn.Handshake(); err != nil {
		return
	}

	var routed net.Conn = conn
	target := web
	switch conn.ConnectionState().NegotiatedProtocol {
	case "h2", "http/1.1":
		// net/http uses the original *tls.Conn to serve HTTP/2.
	case "":
		reader := bufio.NewReaderSize(conn, 1)
		first, err := reader.Peek(1)
		if err != nil {
			return
		}
		b := first[0]
		switch {
		case b&0xc0 == 0:
			target = relay
		case 'A' <= b && b <= 'Z', 'a' <= b && b <= 'z':
		default:
			return
		}
		routed = bufferedTLS{Conn: conn, reader: reader}
	default:
		// ACME TLS-ALPN validation completes at the handshake.
		return
	}

	if err := conn.SetDeadline(time.Time{}); err != nil {
		return
	}
	select {
	case target <- routed:
		handedOff = true
	case <-done:
	}
}
