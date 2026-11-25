# Accounts

A Deskway server serves a small group of people: its admin and the people the admin creates
accounts for. Each account uses only its own computers, and nothing is shared between accounts.
A computer is listed, watched and controlled only by the account that owns it, and it trusts
only that account's devices.

## Roles

There are two kinds of account.

The **admin** is the server's first account. It is created when the server is set up, by
running `deskway-server admin NAME` once while there is no state yet (see
[self-hosting.md](self-hosting.md)). A server has exactly one admin, and the role cannot be
transferred.

**Members** are all other accounts. Only the admin can create, reset or remove them, on the
**Members** page described below. There is no self-service sign-up.

## Making an account

On the **Members** page the admin presses **Create user** and enters a sign-in name and,
optionally, a display name (which defaults to the sign-in name). The server creates the account
with a generated initial password and shows that password once, together with a button that
copies a short message the admin can send through any chat app:

```
Deskway
Link: https://<server>/
Account: <sign-in name>
Initial password: <password>
```

The message is written in the admin's language. The server stores only the password's hash, so
this is the only time the password is shown.

## First sign-in

An account whose password is an initial one (because the admin has just created or reset it)
must choose its own password before it can do anything else. On the account's very first
sign-in it must also choose its super password.

Signing in with the initial password approves the account's first browser. No other device
exists that could approve it, and the password came from the admin. The browser registers its
device key on sign-in, so a newly approved computer can trust that browser from its first
connection.

After sign-in, the page asks for a new password and, the first time, for the super password.
Until both are set, nothing else is available: neither the computers nor the account page.

As a result, the admin knows only the initial password and never learns the password the member
actually uses.

## Managing members

**Members** is a separate page, opened from the menu under the admin's name in the top bar,
next to the account page and the device list. Only the admin's menu contains it, and the page
shows nothing to any other account.

The list shows each member's sign-in name, display name, creation time, last activity and
number of computers. It shows nothing about the computers themselves: no names, no pictures and
no way to control them.

**Reset password** generates a new initial password and shows it once, with the same copy
button. The member must choose a new password at the next sign-in. Browsers that were already
approved remain approved.

**Remove**, after a confirmation, deletes the account together with its browsers and computers.
Its pages are signed out immediately, its sessions end, and its computers are told that they
have been removed.

## Limits

An account can have at most 10 computers; an enrollment beyond that is refused with a message
saying why. At most `maxPendingPerUser` computers and at most `maxPendingPerUser` browsers can
be waiting for approval at the same time.

The relay limits each account's traffic (`-relay-account-mbps`, default 30), so that one
account's relayed sessions cannot crowd out another's.
