package server

import (
	"path/filepath"
	"slices"
	"testing"
	"unsafe"

	"golang.org/x/sys/windows"
)

func TestLocalKeyDACL(t *testing.T) {
	dir := t.TempDir()
	if _, err := OpenLocalCA(dir, "Hub", "hub"); err != nil {
		t.Fatal(err)
	}
	sd, err := windows.GetNamedSecurityInfo(filepath.Join(dir, "local-ca", "intermediate.key"), windows.SE_FILE_OBJECT,
		windows.OWNER_SECURITY_INFORMATION|windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		t.Fatal(err)
	}
	control, _, err := sd.Control()
	if err != nil || control&windows.SE_DACL_PROTECTED == 0 {
		t.Fatalf("DACL is unprotected: %v (control %x)", err, control)
	}
	owner, _, err := sd.Owner()
	if err != nil {
		t.Fatal(err)
	}
	user, err := windows.GetCurrentProcessToken().GetTokenUser()
	if err != nil {
		t.Fatal(err)
	}
	if !owner.Equals(user.User.Sid) {
		t.Fatalf("owner SID = %s, want %s", owner, user.User.Sid)
	}
	dacl, _, err := sd.DACL()
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for i := range uint32(dacl.AceCount) {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(dacl, i, &ace); err != nil {
			t.Fatal(err)
		}
		if ace.Header.AceType != windows.ACCESS_ALLOWED_ACE_TYPE || ace.Header.AceFlags != 0 ||
			ace.Mask&windows.FILE_GENERIC_READ != windows.FILE_GENERIC_READ {
			t.Fatalf("unexpected ACE: %+v", ace)
		}
		got = append(got, (*windows.SID)(unsafe.Pointer(&ace.SidStart)).String())
	}
	want := []string{owner.String(), "S-1-5-18", "S-1-5-32-544"} // owner, SYSTEM, Administrators
	slices.Sort(got)
	slices.Sort(want)
	if !slices.Equal(got, want) {
		t.Fatalf("DACL granted access to %v, want %v", got, want)
	}
}
