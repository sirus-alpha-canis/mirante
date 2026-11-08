# Security

## Reporting a vulnerability

Please report security problems privately, not in a public issue: use GitHub's
**Report a vulnerability** button on this repository's **Security** tab (private
vulnerability reporting). Say what you found, how to reproduce it, and what an attacker
gains. The maintainer aims to answer within a week. Mirante is an early prototype kept by
one person, so there is no bounty, but you will be credited in the fix unless you prefer
not to be.

Only the latest `main` is supported; there are no released versions yet.

## Threat model, in short

Mirante runs coding agents on your machine, as your user, and never on a server. What it
defends, and what it cannot:

- **The daemon socket is local only.** No TCP port, no remote access. The socket lives in
  `~/.mirante` (mode `0700`), is mode `0600`, and the daemon checks the peer's uid.
- **Agents are not people.** The daemon tells a person (the app, the CLI) from a process in
  a Mirante-launched agent's process tree, and gives agents a short allowlist: report their
  own hook events, read and propose memory for their own task, propose skills, read status.
  They cannot create or steer tasks, answer permission requests, or accept proposals.
  Methods are person-only unless listed (fail closed).
- **Nothing is auto-approved.** Mirante never passes skip-permission flags. Each permission
  request is answered once, by a person. Memory and skill decisions also ask the person to
  type the verb on the terminal (an intent check; the boundary is the daemon refusing
  agents).
- **git is hardened.** Agents write the worktrees; the daemon's git calls override every
  config key that could run code (hooks, fsmonitor, filters, diff drivers, pager, ssh and
  credential helpers), clear redirecting `GIT_*` variables and ignore replace refs.
- **Untrusted text is labeled.** Memory notes and attached context given to an agent are
  wrapped as untrusted data and cannot close their own wrapper.
- **Skills are signed.** Each accepted skill version is hashed and signed with a local
  ed25519 key (`ssh-keygen -Y`), and checked again before every injection.
- **Redaction is best effort.** Credential patterns are masked in step summaries and
  agent-written proposals. It lowers what leaks; it guarantees nothing.

**Same-uid limits.** Agents run as you. A prompt-injected agent with shell access can read
anything you can, including `~/.mirante` and the skills signing key, and it can open the
socket. The identity check stops it from acting *as a person through the daemon*, and the
signature catches skill edits made outside Mirante; neither is a sandbox against a process
running as your user. Use the agents' own sandboxes and permission modes for that.

Out of scope: attacks that need root or another local user's account, a compromised agent
CLI or model provider, and the unsigned app bundle being replaced on disk (build it
yourself; signed releases are planned).
