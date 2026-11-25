package server

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"deskway/internal/proto"
)

func TestUpgradeCheck(t *testing.T) {
	for _, tc := range []struct {
		version int
		pending bool
		row     string
		ready   bool
	}{
		{proto.Version, false, fmt.Sprintf("%d\tcurrent", proto.Version), true},
		{proto.MinVersion, false, fmt.Sprintf("%d\tprevious", proto.MinVersion), false},
		{proto.MinVersion - 1, false, fmt.Sprintf("%d\tolder", proto.MinVersion-1), false},
		{0, false, "-\tunknown", false},
		{0, true, "-\tpending", true},
		{proto.Version + 1, false, fmt.Sprintf("%d\tnewer", proto.Version+1), true},
	} {
		t.Run(tc.row, func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "state.json")
			raw := fmt.Appendf(nil, `{"users":{"a":{"name":"owner"},"b":{"name":"admin"}},
				"devices":{"pc1":{"owner":"a","name":"Laptop","version":%d,"pending":%t},
				"pc2":{"owner":"b","name":"Desktop","version":%d}}}`, tc.version, tc.pending, proto.Version)
			if err := os.WriteFile(path, raw, 0o600); err != nil {
				t.Fatal(err)
			}
			var out bytes.Buffer
			ready, err := UpgradeCheck(path, &out)
			want := fmt.Sprintf("ACCOUNT\tCOMPUTER\tPROTOCOL\tSTATUS\nadmin\t\"Desktop\"\t%d\tcurrent\nowner\t\"Laptop\"\t%s\n", proto.Version, tc.row)
			if err != nil || ready != tc.ready || out.String() != want {
				t.Fatalf("ready: %t (want %t), error: %v\nreport: %q\nwant:   %q", ready, tc.ready, err, out.String(), want)
			}
			after, err := os.ReadFile(path)
			if err != nil || !bytes.Equal(after, raw) {
				t.Fatalf("state file changed: %v", err)
			}
			if files, err := os.ReadDir(dir); err != nil || len(files) != 1 {
				t.Fatalf("state directory changed: %v, %v", files, err)
			}
		})
	}
}
