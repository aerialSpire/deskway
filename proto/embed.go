// Package proto embeds the shared protocol version definition.
package proto

import "embed"

// Files contains the version definition used by Go and served to browsers.
//
//go:embed version.json
var Files embed.FS
