package main

import (
	"bytes"
	"encoding/json"
	"net"
	"os"
	"reflect"
	"strings"
	"testing"
)

func TestHostControlContract(t *testing.T) {
	var def struct {
		Version int
		Stdin   map[string]string
		Stdout  map[string]map[string]any
	}
	raw, err := os.ReadFile("../../proto/local-host.json")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(raw, &def); err != nil {
		t.Fatal(err)
	}
	if def.Version != hostControlVersion {
		t.Fatalf("control version: %d", hostControlVersion)
	}
	var output bytes.Buffer
	ready := def.Stdout["ready"]
	hostCommands(strings.NewReader(def.Stdin["usage"]+"\n"+def.Stdin["usage_excluding_hub"]+"\n"), &output, ready["hostname"].(string), ready["fingerprint"].(string), func(hub string) (int, int) {
		switch hub {
		case "":
			return 3, 2
		case "H1":
			return 2, 2
		default:
			t.Fatalf("unexpected hub ID %q", hub)
			return 0, 0
		}
	})
	listener, err := net.ListenTCP("tcp", &net.TCPAddr{})
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	addr := listener.Addr().(*net.TCPAddr)
	if second, err := net.ListenTCP("tcp", addr); err == nil {
		second.Close()
		t.Fatal("the test listener already owns this port")
	} else {
		reportHostBindError(&output, err)
	}
	def.Stdout["port"]["port"] = float64(addr.Port)
	dec := json.NewDecoder(&output)
	for _, kind := range []string{"ready", "usage", "usage_excluding_hub", "port"} {
		var got map[string]any
		if err := dec.Decode(&got); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(got, def.Stdout[kind]) {
			t.Fatalf("%s: got %v, want %v", kind, got, def.Stdout[kind])
		}
	}
}
