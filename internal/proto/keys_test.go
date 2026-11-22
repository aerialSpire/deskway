package proto_test

import (
	"encoding/json"
	"os"
	"testing"

	"deskway/internal/proto"
)

// The six digits match the golden vectors, for a browser (code-v1) and an AI client (code-v2).
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
		if v.Format != "deskway-code-v1" && v.Format != "deskway-code-v2" {
			continue
		}
		key, _ := v.Inputs["key"].(string)
		role, _ := v.Inputs["role"].(string)
		if got := (proto.BrowserKey{Key: key, Role: role}).Code(); got != v.Code {
			t.Errorf("%s: %s, want %s", v.Format, got, v.Code)
		}
		checked++
	}
	if checked != 2 {
		t.Fatalf("checked %d code vectors, want 2", checked)
	}
}

// What an approved AI client signs to sign in again matches its golden vector.
func TestAILoginMatchesTheVector(t *testing.T) {
	b, err := os.ReadFile("../../proto/vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var all struct {
		Vectors []struct {
			Format string `json:"format"`
			Inputs struct {
				Name string `json:"name"`
				TS   int64  `json:"ts"`
			} `json:"inputs"`
			String string `json:"string"`
		} `json:"vectors"`
	}
	if err := json.Unmarshal(b, &all); err != nil {
		t.Fatal(err)
	}
	for _, v := range all.Vectors {
		if v.Format == "deskway-ai-login-v1" {
			if got := proto.AILogin(v.Inputs.Name, v.Inputs.TS); got != v.String {
				t.Fatalf("%q, want %q", got, v.String)
			}
			return
		}
	}
	t.Fatal("no deskway-ai-login-v1 vector")
}
