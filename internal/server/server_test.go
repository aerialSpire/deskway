package server

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestContentSecurityPolicy(t *testing.T) {
	w := httptest.NewRecorder()
	securityHeaders(http.NotFoundHandler()).ServeHTTP(w, httptest.NewRequest("GET", "/", nil))
	const want = "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'"
	if got := w.Header().Get("Content-Security-Policy"); got != want {
		t.Fatalf("Content-Security-Policy = %q, want %q", got, want)
	}
}
