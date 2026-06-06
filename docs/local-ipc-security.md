# Local IPC Security

Clipper's daemon is a per-user local service. Its Unix socket is intended for
Clipper UI clients running as the same OS user, not for cross-user access or
remote access.

## Current Boundary

The daemon enforces a same-user boundary before the IPC HMAC handshake:

- Linux uses `SO_PEERCRED` to read the peer uid.
- macOS uses `getpeereid` to read the peer uid.
- Peers whose effective uid differs from the daemon's effective uid are
  rejected before request handling.

The IPC HMAC secret still gates command execution after the peer uid check.
This is defense in depth against stale sockets, misconfigured socket paths, and
cross-user local access. It is not a complete defense against malicious code
already running as the same desktop user.

For now, Clipper accepts same-user local processes as the practical local trust
boundary. This matches the platform clipboard threat model: a compromised
same-user desktop session can often observe or modify clipboard contents without
using Clipper.

## macOS Future Hardening

The daemon can be hardened to accept only the signed Clipper app by validating
the code signature of the process connected to the Unix socket:

1. Sign `Clipper.app` and the daemon at build or install time.
2. On accept, get the peer pid from the socket with `LOCAL_PEERPID`.
3. Build a `SecCode` object for the peer with
   `SecCodeCopyGuestWithAttributes` and `kSecGuestAttributePid`.
4. Compile an expected requirement with `SecRequirementCreateWithString`.
5. Call `SecCodeCheckValidity` and reject peers that do not satisfy the
   requirement.

For Developer ID distribution, the requirement can pin the bundle identifier and
Apple team identity, for example:

```text
identifier "com.clipper.clipperApp"
and anchor apple generic
and certificate leaf[subject.OU] = "TEAMID"
```

For non-Apple distribution, the requirement can pin a self-signed code-signing
certificate and bundle identifier. The private key is only needed while signing
builds; the daemon stores only the public requirement. A malicious app can copy
Clipper's bundle identifier, but it cannot satisfy the requirement without the
signing private key.

This is a release/install-time hardening path. It does not require sending a
signature or private key over IPC.

## Linux Future Hardening

Linux does not have a universal desktop app identity equivalent to macOS code
signing. `SO_PEERCRED` proves the uid, gid, and pid of the peer, but it does not
distinguish the official Clipper app from another same-user process.

Stronger Linux IPC identity requires distro or sandbox policy:

- AppArmor: confine the Clipper app and daemon under profiles, then use
  `SO_PEERSEC` or profile-mediated socket access to allow only the Clipper app
  profile to connect.
- SELinux: label the app, daemon, and socket with Clipper-specific types, then
  allow only the app type to connect to the daemon type/socket type.
- Flatpak or Snap: rely on sandbox identity and filesystem mediation when
  Clipper is packaged for that sandbox.

These policies usually require root/admin installation and are distribution
specific. Ubuntu is the most natural AppArmor target. Fedora is the most natural
SELinux target. NixOS and Arch can support AppArmor manually, but Clipper should
not assume those policies are present by default.

If such policy is added later, the daemon should fail closed when configured to
require it and the peer label cannot be verified.

## File Path IPC

The daemon still exposes path-based file upload/download commands. This keeps
the current UX where the app can upload any selected file and save downloads to
any selected target path.

The security assumption is that same-user local IPC clients are trusted for now.
If Clipper later needs to harden against same-user clients without relying on
macOS code-signature checks or Linux LSM/sandbox policy, the path-based IPC
commands should be replaced with byte or chunk-oriented upload/download
commands so that file picker authorization and filesystem access stay in the UI
process.
