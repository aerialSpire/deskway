// Package web embeds the browser client.
package web

import (
	"embed"
	"io/fs"
)

//go:embed static
var files embed.FS

// Files returns the client's static files.
func Files() fs.FS {
	sub, err := fs.Sub(files, "static")
	if err != nil {
		panic(err)
	}
	return sub
}
