# Accounts

A Deskway server serves its admin and the members the admin creates. Each account sees and
controls its own computers, and each computer trusts devices of its own account.

## Making an account

The server operator runs `admin-link` as the service user, for example
`sudo -u deskway deskway-server admin-link -data FILE -url https://YOUR_DOMAIN`, and opens the link.
If the server has no admin, the page asks for a sign-in name, an optional display name and a
password. A server has one admin and the role cannot be transferred. See [Self-hosting](self-hosting.md#7-first-sign-in-and-adding-people).

The admin opens **Members** from the menu under their name, chooses **Create user** and enters
a sign-in name and an optional display name. The display name defaults to the sign-in name.
Deskway shows the one-time invitation link and account name in a dialog. **Copy** puts both
into a message in the admin’s language. Send it privately to the intended person.

## First sign-in

Open the invitation link in a browser that can store a device key and choose a password. The
link approves the browser, registers its device key and signs you in. A newly approved computer
can trust that browser from its first connection. Once you have set a password, use it for later sign-ins.
An approved device must approve a new browser that signs in with a password.

## Managing members

**Members** shows each member’s sign-in name, display name, creation time, last activity and
computer count. It also shows unused invitations and reset links with their remaining lifetime,
and marks expired links. All member links expire after seven days.

From the member’s **⋯** menu, choose **New link** to replace a pending or expired invitation or
to issue a reset link for a member with a password. Choose **Revoke link** to invalidate an unused
link. Deskway shows each new link only when it is issued. **Remove** asks for confirmation and
removes the member and its devices, signs out its browsers and ends its sessions.

## Recovery

- **Member**. Ask the admin for a new link. The admin issues it under **Members**.
- **Admin**. Run the same `admin-link` command as the service user. The command works while the
  service is running. The link expires after one hour.

Opening a reset link lets you choose a password and signs in the browser. The account’s existing
password works until the link is redeemed. Its computers, approved browsers and data are kept.
A browser approved by a link may still need to be added at a computer that already trusts other
devices. See [Adding a device](security.md#adding-a-device).

## Limits

An account can have up to 10 computers, including those waiting for approval. At most five
computers and five browsers can wait for approval at the same time.

The relay limits each account’s traffic (`-relay-account-mbps`, default 30) so one account’s
relayed sessions cannot crowd out another’s.
