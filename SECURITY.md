# Security

quai-dash runs on Quai node hosts, usually as the node's own user, and
reads what that user can read. Reports of anything that weakens that host
are welcome.

## Reporting a vulnerability

Report it privately through GitHub:
**[Report a vulnerability](https://github.com/mpoletiek/quai-node-dashboard/security/advisories/new)**
(the repository's Security tab → "Report a vulnerability"). Please don't
open a public issue for a security problem.

Include what you can of:

- the version (`quai-dash --version`) or commit, and how it was installed
  (release archive, source, service installer);
- what an attacker needs (a local account, a miner on the node's stratum,
  a position on the network, a web page the operator visits, …);
- steps or input that reproduce it, and what happens.

You should hear back within a week. Once a fix is released, the advisory
is published with credit to you, unless you'd rather not be named.

## Supported versions

Fixes go into the latest release only. quai-dash has no long-term
branches; upgrading is replacing one binary.

## What quai-dash defends against

The design assumes these are untrusted, and treats a way past any of
them as a vulnerability:

- **Miners on the node's stratum.** Worker names and addresses are
  attacker-chosen: they must not reach the browser or the terminal as
  markup or control sequences, crash quai-dash, or make it grow without
  bound.
- **Other local users.** They must not be able to point detection at
  their own processes or files, read files through the dashboard, or
  take over the service's log path. Nothing quai-dash or its installer
  runs as root may write in a path another user controls.
- **The network.** Replies from a node, a stratum API or ip-api.com are
  bounded in size and time and parsed without panicking.
- **Web pages the operator visits.** The dashboard answers only requests
  addressed to this machine (no DNS rebinding) and its page loads
  nothing from other sites.

## Known limits (by design, documented)

- The web dashboard has **no authentication**. It listens on 127.0.0.1
  by default; listening on another address shows the node's logs, peers
  and miners to anyone who can reach it (quai-dash warns). Use an SSH
  tunnel instead.
- The node's stratum API has no authentication either; keep it on the
  node's host.
- `--geoip online` sends peer IP addresses to ip-api.com over plain HTTP.
  It is off unless asked for.

Reports about these as such aren't vulnerabilities, but ways to make them
safer are welcome as issues.

## Verifying a release

`install.sh` refuses any archive that doesn't match the release's
`SHA256SUMS`. Each release archive is also attested as built by this
repository's release workflow; to check both by hand:

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify quai-dash-<version>-<arch>-linux.tar.gz --repo mpoletiek/quai-node-dashboard
```
