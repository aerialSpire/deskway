package proto_test

import (
	"encoding/json"
	"os"
	"testing"

	"deskway/internal/proto"
)

// The six digits match the golden vectors, for a browser (code-v1), an AI client (code-v2) and
// a computer waiting for approval (computer-code-v1).
func TestCodesMatchTheVectors(t *testing.T) {
	b, err := os.ReadFile("../../proto/vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var all struct {
		Vectors []struct {
			Format string         `json:"format"`
			Inputs map[string]any `json:"inputs"`
			Code   string         `json:"code"`
		} `json:"vectors"`
	}
	if err := json.Unmarshal(b, &all); err != nil {
		t.Fatal(err)
	}
	checked := 0
	for _, v := range all.Vectors {
		key, _ := v.Inputs["key"].(string)
		role, _ := v.Inputs["role"].(string)
		id, _ := v.Inputs["id"].(string)
		var got string
		switch v.Format {
		case "deskway-code-v1", "deskway-code-v2":
			got = (proto.BrowserKey{Key: key, Role: role}).Code()
		case "deskway-computer-code-v1":
			got = proto.ComputerCode(id)
		default:
			continue
		}
		if got != v.Code {
			t.Errorf("%s: %s, want %s", v.Format, got, v.Code)
		}
		checked++
	}
	if checked != 3 {
		t.Fatalf("checked %d code vectors, want 3", checked)
	}
}

// AI sign-in and endorsement match their golden vectors.
func TestAISignaturesMatchTheVectors(t *testing.T) {
	b, err := os.ReadFile("../../proto/vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var all struct {
		Vectors []struct {
			Format string `json:"format"`
			Inputs struct {
				proto.BrowserKey
				TS int64 `json:"ts"`
			} `json:"inputs"`
			String string `json:"string"`
		} `json:"vectors"`
	}
	if err := json.Unmarshal(b, &all); err != nil {
		t.Fatal(err)
	}
	checked := 0
	for _, v := range all.Vectors {
		var got string
		switch v.Format {
		case "deskway-ai-login-v1":
			got = proto.AILogin(v.Inputs.Name, v.Inputs.TS)
		case "deskway-endorse-v2":
			got = v.Inputs.AIEndorsement(v.Inputs.TS)
		default:
			continue
		}
		if got != v.String {
			t.Errorf("%s: %q, want %q", v.Format, got, v.String)
		}
		checked++
	}
	if checked != 2 {
		t.Fatalf("checked %d AI signature vectors, want 2", checked)
	}
}
