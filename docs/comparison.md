# How Deskway compares

This page compares Deskway with RustDesk and Chrome Remote Desktop, two widely used remote
desktop tools. It is based on each project's public documentation and repositories as of
November 2026, with the sources listed below the table. Products change; if something here is
out of date or wrong, please open an issue with a link to the source.

| | Deskway | RustDesk | Chrome Remote Desktop |
|---|---|---|---|
| AI control through MCP | Built in | Community projects; built-in support proposed in an upstream pull request that is not merged [1][2] | No [6] |
| AI access control | Approve each AI client, then allow it per computer | Proposed in the same unmerged pull request; community projects offer token sign-in or per-tool confirmation at most [2][3] | No [6] |
| Person and AI on one computer | Separate sessions at the same time | Proposed in the unmerged pull request: a person watches and can take over [2] | No [6] |
| Record of AI actions | Every tool call recorded on the computer | No; Server Pro audit logs cover connections [4] | No [6] |
| Signaling and relay | Your server: direct connection, or its built-in TURN relay | Public servers by default, or self-hosted ID and relay servers [5] | Google's service [7] |
| Web client | Served by your server | Self-hosted web client on Server Pro plans above a size threshold [4] | Provided by Google at remotedesktop.google.com [6] |
| Accounts | Managed on your server | Device ID and password; user accounts, OIDC single sign-on and LDAP in Server Pro [4] | Google account [7] |
| Controlling device | Desktop and mobile browsers | Desktop, mobile and web clients [5] | Web browser on a computer, mobile apps [6] |
| Controlled computers | Windows, macOS | Windows, macOS, Linux, Android [5] | Windows, macOS, Linux [6] |

"No" for Chrome Remote Desktop's AI rows means its documentation describes no such feature.

## Sources

1. Community MCP projects for RustDesk, for example
   [YaoxinCS/RustdeskMCP](https://github.com/YaoxinCS/RustdeskMCP),
   [sandraschi/rustdesk-mcp](https://github.com/sandraschi/rustdesk-mcp) and
   [Cocos666a/rustdesk-mcp](https://github.com/Cocos666a/rustdesk-mcp).
2. RustDesk pull request [#16470](https://github.com/rustdesk/rustdesk/pull/16470), "built-in MCP
   server so AI agents can control remote sessions" (open), following discussion
   [#16246](https://github.com/rustdesk/rustdesk/discussions/16246).
3. The READMEs of the community projects in [1].
4. [RustDesk pricing and Server Pro FAQ](https://rustdesk.com/pricing/).
5. RustDesk documentation: [client](https://rustdesk.com/docs/en/client/) and
   [self-hosting](https://rustdesk.com/docs/en/self-host/).
6. Chrome Remote Desktop help:
   [Access another computer with Chrome Remote Desktop](https://support.google.com/chrome/answer/1649523).
7. Chrome Enterprise help:
   [Chrome Remote Desktop for administrators](https://support.google.com/chrome/a/answer/2799701).
