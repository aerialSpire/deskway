# Install Deskway Host on Windows

Deskway Host runs on the computer you want to control. On Windows it installs as a system service,
so the computer can be reached even at the lock screen and the sign-in screen.

You need:

- 64-bit Windows 10 or Windows 11 on an x64 processor.
- An administrator account on that computer, for installing.
- A Deskway server address, such as `https://desk.example.net`, and the account the computer
  belongs to. See [Self-hosting](self-hosting.md) to run your own.
- A browser that is already signed in and approved for that account, to approve the computer.

## 1. Download

Open the [Releases page](https://github.com/aerialSpire/deskway/releases), pick the latest release, and download
**`DeskwayHost-Setup-windows-x64.exe`** from its **Assets** list. (The other `.exe` there,
`deskway-windows-x64.exe`, is the AI client program, not the host.)

The release builds are not code-signed. When you open the file, Windows SmartScreen may show
"Windows protected your PC". Choose **More info**, check that the file is `DeskwayHost-Setup-windows-x64.exe`,
and choose **Run anyway**.

To build it yourself instead, see [Building from source](#building-from-source).

## 2. Install

1. Run `DeskwayHost-Setup-windows-x64.exe`. The **Install Deskway Host** window opens.
2. Click **Install**. When Windows asks for administrator rights, click **Yes**.
3. The installer installs and starts the system service, then shows **Installation complete**.
   Click **Done**.

Deskway Host now starts with Windows. Its icon is in the notification area (system tray).

## 3. Sign in this computer

The Deskway Host panel opens when installation finishes. To open it later, choose
**Open Deskway Host** from the tray icon.

1. Under **Sign in this computer**, enter the **Server address**, the **Account** and the
   **Password**, then click **Sign in**.
2. The panel shows **Waiting for approval on a signed-in device** with a six-digit code. Your
   approved browser shows the request "A new computer wants to join" with the computer's name,
   address and code. Approve it there, or on the **My devices** page, only if the codes match.
3. The panel shows **Online**. The computer now appears in the browser's list of computers.

For a [local server](self-hosting.md#local-mode), paste the whole
`https://name.local:port#ca=<64 hex digits>` address into **Server address**. Use the
fingerprint from the hub's startup log. The agent adds the verified root to Windows'
machine-wide trusted roots for browsers and removes that certificate when you sign out.

## 4. Allow the devices you control from

A computer accepts control only from devices that were added to it at the computer itself. The
first time you open the computer from another device's browser, you are asked to add that
device.

- To add a device that can scan a QR code, click **Add a device** in the Deskway Host
  panel, scan the code with that device, and choose **Add** on it.
- To add a device that cannot scan, open Deskway in its browser, click this computer and
  choose **Request control**. Then choose **Allow** at this computer.

The **Allowed devices** list in the panel shows every device that may control this computer, and
you can remove devices from it there.

## Terminal and Files in the browser

After signing in to Windows, open the computer’s page and choose **Terminal** or **Files**.
Terminal starts PowerShell 7 if available and uses Windows PowerShell otherwise. Files starts
in your profile folder. The root lists drives, and you can browse, upload, replace and
download files with your Windows account’s rights.

When Windows is locked or showing a secure prompt, take over the desktop and unlock it to
use these actions. A terminal you left keeps running while Windows is locked. When the
Windows user signs out or changes, their terminal ends. An interrupted upload can resume from
the same browser page after unlocking. A computer set to view only offers desktop viewing.

## Settings in the panel

| Setting | What it does |
|---|---|
| **Start with Windows** | On by default. When off, the host doesn't run after a restart and shows as offline |
| **Keep this computer awake** | Requests that Windows prevent idle sleep while Deskway Host runs. The display may still turn off |
| **Lock when disconnected** | Locks Windows when a remote desktop session ends |
| **Sign out** | Stops this computer using the account. The account keeps it: signing in again with the same account brings it back as it was. Removing it on **My devices** takes it off the account |

On battery below 15%, Deskway releases the setting's keep-awake request. It requests keep-awake
again on external power or at 15% or higher. Active sessions keep their own requests. On Modern
Standby devices running on battery, Windows can end a keep-awake request on its own. Deskway
reissues it when external power returns.

The person at the computer can always see when it is being controlled, and can end any session
from the on-screen notice or from the panel.

### Keeping a laptop reachable with its lid closed

In Windows, open **Control Panel → Hardware and Sound → Power Options → Choose what closing
the lid does**. Set **When I close the lid** to **Do nothing** in the **Plugged in** column,
then save changes.

## Updating

Download the new release's `DeskwayHost-Setup-windows-x64.exe` and run it. It detects the installed version and
offers **Update**. Sign-in details and allowed devices are kept.

## Uninstalling

Run `DeskwayHost-Setup-windows-x64.exe` again, or use **Settings → Apps → Deskway Host → Uninstall**, then choose
**Uninstall**. The installer keeps this computer's sign-in details and allowed devices for a later
reinstall unless you check the option to delete them.

## Building from source

On Windows with [Rust](https://rustup.rs/) (stable, with the MSVC toolchain and the Visual Studio
C++ build tools):

```powershell
cargo build --locked --release -p deskway-host
```

The result is `target\release\deskway-host.exe`. Copy or rename it to `DeskwayHost-Setup-windows-x64.exe` and run
it. When the program's file name contains "setup", it opens the installer.

To try Deskway without installing the service, run `scripts\try-local.ps1`. It builds and starts
a server and the host on one Windows computer. On the first run, open the link it prints and
set up the admin account, then use that account to enroll the trial host.

## Troubleshooting

| Symptom | Check |
|---|---|
| "You clicked “No” in the Windows prompt" | Run the installer again and click **Yes** when Windows asks |
| **Can't reach the server** | The server address is right and opens in a browser on this computer |
| Sign-in fails with a wrong password | Use the password chosen when setting up or recovering the account |
| Stays at **Waiting for approval on a signed-in device** | Approve the computer from an already approved browser, on **My devices** |
| The account can't add more computers | Each account can have up to 10 computers. Remove unused ones on **My devices** |
| The browser shows "This computer needs an update" | Update Deskway Host to the server's version |
