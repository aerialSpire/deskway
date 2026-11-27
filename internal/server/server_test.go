package server

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestContentSecurityPolicy(t *testing.T) {
	w := httptest.NewRecorder()
	securityHeaders(http.NotFoundHandler(), true).ServeHTTP(w, httptest.NewRequest("GET", "/", nil))
	const want = "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'"
	if got := w.Header().Get("Content-Security-Policy"); got != want {
		t.Fatalf("Content-Security-Policy = %q, want %q", got, want)
	}
}

func TestLocalUsageExcludesOnlyTheConnectedHub(t *testing.T) {
	s := &Server{
		hosts: map[string]*conn{"hub": {}, "pc1": {}, "pc2": {}},
		clients: map[*conn]bool{
			{browser: "one"}: true, {browser: "one"}: true, {browser: "ai", ai: true}: true,
		},
	}
	for hub, want := range map[string]int{"hub": 2, "offline": 3, "": 3} {
		if computers, browsers := s.LocalUsage(hub); computers != want || browsers != 1 {
			t.Errorf("LocalUsage(%q) = %d computers, %d browsers; want %d, 1", hub, computers, browsers, want)
		}
	}
}
