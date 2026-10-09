# Changelog

All notable changes to quai-dash. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/). Each release's section
here becomes its release notes.

## [0.1.0] - 2026-10-08

The first release: a live monitor for a Quai node, rs-quai or go-quai,
in the browser (`quai-dash web`) or the terminal (`quai-dash tui`), in
two looks, GHOST and ANGEL.

### Added

- **Zero-flag start.** Finds the node on the zone RPC port, tells rs-quai
  from go-quai (process, log format, stratum API, RPC headers), follows
  its `nodelogs`, and finds its stratum API. `quai-dash config` shows what
  it found and why.
- **Chain view.** Zone height and block timer; prime, region and zone
  heads; block intervals, transactions, workshares and gas; hashrate,
  share time and pending workshares for KawPoW, SHA and Scrypt; base fee,
  rewards and supply; a block lattice of the last few minutes; events
  for prime and region blocks, reorgs, stalls and RPC loss.
- **Comparison.** `--compare` checks every block against a second node
  and shows the sync ratio.
- **Peer map.** The node process's TCP peers from `/proc`, placed on a
  globe (GHOST) or a flat map (ANGEL) with a local MaxMind City database;
  a pixel map in kitty, Ghostty and WezTerm.
- **Mining view.** The node's stratum: workers and miners on all three
  algorithms, shares handed to the node and settled against the chain,
  and what the chain paid each miner; addresses link to the explorer.
- **Node log.** Colored by level, with a WARN+ filter.
- **Configuration.** Every option as a flag, a `QUAI_DASH_*` variable or a
  key in a TOML config file (flag > env > file > detected > default).
- **Service.** `scripts/install-service.sh` installs a systemd (system or
  user) or OpenRC service that runs `quai-dash web` with no flags.
- **Demo mode** (`--demo`) and `quai-dash record` with the `web/tui.html`
  player.
- **Release archives.** Static Linux binaries for x86_64 and aarch64,
  with SHA256SUMS and build-provenance attestations.

### Security

quai-dash is built to run on node hosts, so this release treats miners,
other local users, the network and web pages as untrusted (see
[SECURITY.md](SECURITY.md)):

- The web dashboard listens on 127.0.0.1, answers only requests addressed
  to this machine, holds at most 16 connections with timeouts, and its
  page escapes everything it shows, loads nothing from other sites (fonts
  are bundled) and carries a Content-Security-Policy.
- Detection trusts only processes and files of the node's own user.
- Every reply from a node, stratum or ip-api.com is bounded in size and
  time; text from miners and nodes is clipped and never reaches a
  terminal as control sequences.
- Nothing quai-dash or its installer runs as root writes in a path
  another user controls.
- Peer IP addresses leave the host only with `--geoip online`.
- A panic ends the process (the service restarts it) rather than leaving
  a frozen dashboard, and both dashboards say STALE or NO SERVER instead
  of showing old data as live.

[0.1.0]: https://github.com/mpoletiek/quai-node-dashboard/releases/tag/v0.1.0
