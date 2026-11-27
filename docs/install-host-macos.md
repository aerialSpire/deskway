# Install Deskway Host on macOS

Deskway Host runs on the Mac you want to control and lives in the menu bar. In the browser,
open the Mac’s desktop to use its screen, keyboard, clipboard and desktop file transfer.
Desktop control requires Screen Recording and Accessibility. You can also open **Terminal**
or **Files** on the computer’s page. Before you sign in to macOS, the Mac appears offline.

You need:

- macOS 15 or later, on Apple silicon or Intel. The release app is a universal binary, so the
  same download works on both.
- A Deskway server address, such as `https://desk.example.net`, and the account the Mac belongs
  to. See [Self-hosting](self-hosting.md) to run your own.
- A browser that is already signed in and approved for that account, to approve the Mac.

## 1. Download and install

1. Open the [Releases page](https://github.com/aerialSpire/deskway/releases), pick the latest release, and download
   **`DeskwayHost-macos-universal.dmg`** from its **Assets** list. (The `deskway-macos-arm64` and
   `deskway-macos-x64` files there are the AI client programs, not the host.)
2. Open the DMG and drag **Deskway.app** into **Applications**.
3. Open Deskway from Applications.

The release app is ad-hoc signed, not notarized by Apple. If macOS says it can't verify the app,
open **System Settings → Privacy & Security**, find the message about Deskway near the bottom, and
choose **Open Anyway**. This is needed once for each version.

To build it yourself instead, see [Building from source](#building-from-source).

## 2. Grant permissions

Click the Deskway Host icon in the menu bar to open its panel. While a required permission is
missing, the status shows **macOS permissions needed**.

1. Click **Allow Screen Recording**, and turn on Deskway Host in the System Settings page that
   opens. When macOS offers to quit and reopen the app, accept, or click **Restart Deskway** in the
   panel.
2. Click **Allow Accessibility**, and turn on Deskway Host in System Settings. This lets remote
   mouse and keyboard input reach the Mac.

As you grant the required permissions, the panel shows **Screen Recording allowed** or
**Accessibility allowed** until both are granted. If you dismissed a request, the panel offers
**Open Screen Recording settings** or **Open Accessibility settings** instead.

To skip folder-by-folder prompts during file transfers or AI file operations, click
**Open Full Disk Access settings** in the panel and turn on this optional permission for Deskway
Host. Quit and reopen Deskway if macOS asks.

The ad-hoc signature identifies one specific build, so macOS may ask for these permissions again
after an update.

## 3. Sign in this Mac

1. Under **Sign in this computer**, enter the **Server address**, the **Account** and the
   **Password**, then click **Sign in**.
2. The panel shows **Waiting for approval on a signed-in device** with a six-digit code. Approve
   the request "A new computer wants to join" in your approved browser, or on its **My devices**
   page, only if it shows the same code.
3. The panel shows **Online**. The Mac appears in the browser's list of computers.

## 4. Allow the devices you control from

A Mac accepts control only from devices that were added to it at the Mac itself.

- To add a device that can scan a QR code, click **Add a device** in the panel, scan the
  code with that device, and choose **Add** on it.
- To add a device that cannot scan, open Deskway in its browser, click this Mac and choose
  **Request control**. Then choose **Allow** at the Mac.

The panel's **Allowed devices** list shows every device that may control this Mac.

## Starting at login and staying awake

Finishing setup turns on **Open at login** and **Keep this computer awake**.

- **Open at login** opens Deskway Host after you sign in to macOS. If the panel shows
  **Login item awaiting approval**, click **Approve the login item in System Settings** and allow
  Deskway under **General → Login Items & Extensions**.
- **Keep this computer awake** prevents idle system sleep while Deskway Host runs. On battery
  below 15%, Deskway releases the setting's request and reapplies it on external power or at 15%
  or higher. Active sessions keep their own requests. The display may still turn off between
  sessions. A desk session wakes the display.

You can turn each setting on or off separately. Signing out in the panel also removes the login item and stops keeping
the Mac awake. If
macOS refuses to remove the login item, use **Try again to stop opening at login**.

A Mac laptop sleeps when its lid is closed unless it is in macOS closed-display mode, which
requires external power, an external display, and a keyboard and mouse or trackpad. Deskway
cannot change this macOS behavior.

## Using a Mac remotely

- Use the remote keyboard's Command key for Mac shortcuts such as ⌘C and ⌘V.
- With several displays, the browser shows a preview of each and can switch between them.
- Current browsers draw the pointer locally. For older browsers, the pointer is drawn into the
  video. To leave it out of the video, start Deskway Host with `DESKWAY_CAPTURE_CURSOR=0` in its
  environment.
- To lock the Mac, send Control-Command-Q with the remote keyboard. While you stay signed in to
  macOS and the permissions are granted, you can type the Mac's password at the lock screen in the
  remote view. Signing out of macOS closes Deskway Host.
- Clipboard sharing is off until you turn it on in the session toolbar. Once it is on, text, HTML
  and pictures copied on one side can be pasted on the other. Files copied on the Mac can be
  fetched with **Get it here**.
- **Send files or images** works both ways. Files sent to the Mac go to **Downloads → Deskway**
  and are also put on the clipboard. macOS may ask for access to Downloads the first time.
- The Mac can be controlled without a monitor attached when macOS provides a display to capture.
- During a session, the Deskway Host menu and panel appear in the video. The notice with the
  disconnect button and the border remain visible on the Mac itself.

Settings and the sign-in token are stored in `~/Library/Application Support/Deskway/host.json`.

## Terminal and Files in the browser

**Terminal** opens the login shell in your home folder. It uses `$SHELL -l` or `/bin/zsh -l`
if `SHELL` is unset. **Files** starts in your home folder and lets you browse, upload and download
with your macOS account’s rights. Terminal and Files work without Screen Recording or
Accessibility. If macOS blocks access to a folder, Files shows the denial at that folder.
The host panel has an optional **Open Full Disk Access settings** button. The shell runs with
Deskway Host’s privacy permissions, which can differ from the Terminal app’s permissions.

Locking the screen or switching to another macOS user ends the Terminal or Files session. A kept terminal
continues to run, and you can resume an interrupted transfer from the same page after you return
and unlock the Mac. Logging out ends the host and its terminal. A view-only host omits Terminal
and Files.

## Updating

Quit Deskway Host (**Quit** in its menu bar menu), download the new release's DMG and replace
`Deskway.app` in Applications. macOS may ask for the permissions again.

## Uninstalling

Remove the Mac on **My devices** in the browser, quit Deskway Host, and move `Deskway.app` from
Applications to the Trash. (**Sign out** in the panel keeps the Mac on the account: signing in
again with the same account brings it back as it was.) To remove its settings too, delete `~/Library/Application Support/Deskway`.

## Building from source

On an Apple silicon Mac with macOS 15 or later, [Rust](https://rustup.rs/) and Apple's
command-line developer tools (`xcode-select --install`):

```sh
./scripts/build-mac-host.sh --install          # build, install to /Applications and open it
./scripts/build-mac-host.sh --dmg              # bin/DeskwayHost-macos-arm64.dmg (Apple silicon only)
./scripts/build-mac-host.sh --universal-dmg    # bin/DeskwayHost-macos-universal.dmg (Apple silicon and Intel)
```

`--universal-dmg` needs the Intel Rust target: `rustup target add x86_64-apple-darwin`. Run
`--install` again to update an installed copy in place.

## Troubleshooting

| Symptom | Check |
|---|---|
| The browser shows a black screen | Screen Recording is allowed for the installed Deskway Host, and the app was reopened after allowing it |
| Video works but mouse and keyboard don't | Accessibility is allowed for Deskway Host |
| Permissions are on but still reported missing after an update | Remove Deskway Host from both lists in System Settings, then allow it again |
| **Can't reach the server** | The server address opens in Safari on this Mac |
| Stays at **Waiting for approval on a signed-in device** | Approve the Mac from an approved browser, on **My devices** |
