package server

import (
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"

	"deskway/internal/proto"
)

// UpgradeCheck reports the computers in an atomically written state snapshot. The next
// protocol's minimum is this release's current version. Only approved computers affect readiness.
func UpgradeCheck(path string, out io.Writer) (ready bool, err error) {
	var state Store
	if err := readState(path, &state); err != nil {
		return false, err
	}
	ready = true
	rows := make([]string, 0, len(state.Devices))
	for _, d := range state.Devices {
		version := "-"
		if d.Version > 0 {
			version = strconv.Itoa(d.Version)
		}
		var status string
		switch {
		case d.Pending:
			status = "pending"
		case d.Version <= 0:
			status = "unknown"
		case d.Version < proto.MinVersion:
			status = "older"
		case d.Version == proto.MinVersion:
			status = "previous"
		case d.Version == proto.Version:
			status = "current"
		default:
			status = "newer"
		}
		ready = ready && (d.Pending || d.Version >= proto.Version)
		rows = append(rows, fmt.Sprintf("%s\t%q\t%s\t%s\n", state.Name(d.Owner), d.Name, version, status))
	}
	slices.Sort(rows)
	_, err = fmt.Fprintf(out, "ACCOUNT\tCOMPUTER\tPROTOCOL\tSTATUS\n%s", strings.Join(rows, ""))
	return ready, err
}
