package server

import (
	"errors"
	"os"
	"unsafe"

	"golang.org/x/sys/windows"
)

func writeLocalKey(path string, data []byte) error {
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return err
	}
	user, err := windows.GetCurrentProcessToken().GetTokenUser()
	if err != nil {
		return err
	}
	owner := user.User.Sid.String()
	// Set the owner and protect the DACL at creation, before writing any key bytes.
	sd, err := windows.SecurityDescriptorFromString("O:" + owner + "D:P(A;;FA;;;" + owner + ")(A;;FA;;;SY)(A;;FA;;;BA)")
	if err != nil {
		return err
	}
	sa := windows.SecurityAttributes{SecurityDescriptor: sd}
	sa.Length = uint32(unsafe.Sizeof(sa))
	handle, err := windows.CreateFile(name, windows.GENERIC_WRITE, 0, &sa, windows.CREATE_NEW, windows.FILE_ATTRIBUTE_NORMAL, 0)
	if err != nil {
		return &os.PathError{Op: "create", Path: path, Err: err}
	}
	f := os.NewFile(uintptr(handle), path)
	_, err = f.Write(data)
	return errors.Join(err, f.Close())
}

func readLocalKey(path string) ([]byte, error) {
	return os.ReadFile(path)
}
