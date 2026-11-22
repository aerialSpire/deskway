package proto_test

// Contract test: the Go signaling types against proto/messages.json, both ways.

import (
	"encoding/json"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"reflect"
	"sort"
	"strconv"
	"strings"
	"testing"

	"deskway/internal/proto"
)

type field struct {
	Type     string `json:"type"`
	Optional *bool  `json:"optional"`
}

type message struct {
	Type   string           `json:"type"`
	Status string           `json:"status"`
	Fields map[string]field `json:"fields"`
}

type definition struct {
	ProtocolVersion int `json:"protocol_version"`
	MinVersion      int `json:"min_version"`
	Types           map[string]struct {
		Fields map[string]field `json:"fields"`
	} `json:"types"`
	Channels map[string]struct {
		Discriminator string    `json:"discriminator"`
		Messages      []message `json:"messages"`
	} `json:"channels"`
}

func load(t *testing.T) definition {
	t.Helper()
	b, err := os.ReadFile("../../proto/messages.json")
	if err != nil {
		t.Fatal(err)
	}
	var d definition
	if err := json.Unmarshal(b, &d); err != nil {
		t.Fatal(err)
	}
	return d
}

// jsonFields returns the wire names of a struct's fields and whether each is omitempty.
func jsonFields(v any) map[string]bool {
	out := map[string]bool{}
	rt := reflect.TypeOf(v)
	for i := 0; i < rt.NumField(); i++ {
		tag := rt.Field(i).Tag.Get("json")
		name, opts, _ := strings.Cut(tag, ",")
		if name == "" || name == "-" {
			continue
		}
		out[name] = strings.Contains(opts, "omitempty")
	}
	return out
}

func keys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

// typeConstants reads every Type* constant from proto.go, so a constant cannot be added
// without the definition knowing it.
func typeConstants(t *testing.T) map[string]string {
	t.Helper()
	f, err := parser.ParseFile(token.NewFileSet(), "proto.go", nil, 0)
	if err != nil {
		t.Fatal(err)
	}
	out := map[string]string{}
	for _, d := range f.Decls {
		g, ok := d.(*ast.GenDecl)
		if !ok || g.Tok != token.CONST {
			continue
		}
		for _, s := range g.Specs {
			vs := s.(*ast.ValueSpec)
			for i, n := range vs.Names {
				if !strings.HasPrefix(n.Name, "Type") || i >= len(vs.Values) {
					continue
				}
				lit, ok := vs.Values[i].(*ast.BasicLit)
				if !ok {
					continue
				}
				v, _ := strconv.Unquote(lit.Value)
				out[v] = n.Name
			}
		}
	}
	return out
}

func TestSignalingTypesMatchDefinition(t *testing.T) {
	d := load(t)
	defined := map[string]bool{}
	fields := map[string]bool{}
	for _, name := range []string{"signaling.browser", "signaling.host"} {
		ch, ok := d.Channels[name]
		if !ok {
			t.Fatalf("no channel %s", name)
		}
		if ch.Discriminator != "type" {
			t.Errorf("%s: discriminator %q, Msg uses \"type\"", name, ch.Discriminator)
		}
		for _, m := range ch.Messages {
			defined[m.Type] = true
			for f, def := range m.Fields {
				fields[f] = true
				if def.Optional == nil {
					t.Errorf("%s %s.%s: optional not set", name, m.Type, f)
				}
			}
		}
	}
	consts := typeConstants(t)
	for ty := range defined {
		if consts[ty] == "" {
			t.Errorf("message %q is defined but has no Type constant", ty)
		}
	}
	for ty, c := range consts {
		if !defined[ty] {
			t.Errorf("%s = %q is not in messages.json", c, ty)
		}
	}
	msg := jsonFields(proto.Msg{})
	delete(msg, "type")
	for f := range fields {
		if _, ok := msg[f]; !ok {
			t.Errorf("field %q is defined but proto.Msg has no such field", f)
		}
	}
	for _, f := range keys(msg) {
		if !fields[f] {
			t.Errorf("proto.Msg field %q is in no signaling message", f)
		}
		if !msg[f] {
			t.Errorf("proto.Msg field %q must be omitempty: not every message carries it", f)
		}
	}
}

func TestSharedTypesMatchDefinition(t *testing.T) {
	d := load(t)
	for name, v := range map[string]any{
		"Device":     proto.Device{},
		"Holder":     proto.Holder{},
		"Controller": proto.Controller{},
		"Approval":   proto.Approval{},
		"ICEServer":  proto.ICEServer{},
		"View":       proto.View{},
		"LogEntry":   proto.LogEntry{},
		"AiSwitch":   proto.AiSwitch{},
		"AiView":     proto.AiView{},
	} {
		def, ok := d.Types[name]
		if !ok {
			t.Errorf("no type %s in messages.json", name)
			continue
		}
		have := jsonFields(v)
		if got, want := keys(have), keys(def.Fields); !reflect.DeepEqual(got, want) {
			t.Errorf("%s fields: Go %v, definition %v", name, got, want)
			continue
		}
		for f, fd := range def.Fields {
			// Required booleans and numbers may be zero, so they must not be omitted.
			if !*fd.Optional && have[f] {
				t.Errorf("%s.%s is required but omitempty", name, f)
			}
		}
	}
}

func TestVersionMatchesDefinition(t *testing.T) {
	d := load(t)
	if d.ProtocolVersion != proto.Version || d.MinVersion != proto.MinVersion {
		t.Errorf("definition %d/%d, Go %d/%d", d.ProtocolVersion, d.MinVersion, proto.Version, proto.MinVersion)
	}
}
