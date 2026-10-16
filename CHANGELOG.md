# Changelog

All notable changes to quai-dash. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/). Each release's section
here becomes its release notes.

## [Unreleased]

### Added

- **A QUAI look, now the default, in the browser and the terminal.**
  Quai Network's own colours (the logo's red, `#E20101`, on warm browns,
  from qu.ai) and the Quai logo, with pieces of its own rather than a
  recoloured GHOST or ANGEL: the block timer is the logo's ring, filling
  red over two average block times with the seconds in the middle; the
  peer map is a globe inside the logo's ring; the proof-of-work panel is
  a table over one chart of the three hashrates, each scaled to its own
  range; the digits of the zone height that changed glow for a moment;
  prime, region and mined blocks get a card in the tier's colour. In the
  terminal the header rule burns red as the zone waits for its next
  block. Red is the brand here, so alerts are a lighter red and always
  carry a word (OFFLINE, ALERT, OVERDUE).

### Changed

- **The default look is QUAI** (was GHOST). `--theme` (`theme`,
  `QUAI_DASH_THEME`) takes `quai`, `ghost` or `angel` and now sets the
  browser's starting look as well as the terminal's. `t` / `T` cycles
  QUAI → GHOST → ANGEL. The browser keeps a look you pick there and no
  longer saves one you didn't, so earlier visitors see QUAI once.
- `quai-dash record` tours all three looks, starting in QUAI (56 s by
  default, was 48 s).

## [0.1.4] - 2026-10-15

### Added

- **A syncing view, in the browser and the terminal.** A node far behind
  the network (its zone head older than 10,000 blocks' worth of the 5 s
  target, about 14 hours) gets a light page instead of the dashboard:
  zone, region and prime heights, the date it has synced to, roughly how
  many blocks behind it is, its pace over the last minute, an ETA, a
  progress bar through the chain's history, peers in and out, and the
  node's last log lines. The block lattice, the peer map, the
  proof-of-work panels and full-screen moments come back once it is
  within 5,000 blocks' worth (the gap keeps it from flapping). In the
  browser the page now takes about 5% of a core on a syncing node; the
  full dashboard took most of one.
- `/api/state` carries the picture both views draw: `sync` (`syncing`,
  `behind_s`, `approx_blocks_behind`, `blocks_per_min`, `eta_s`,
  `progress_bp`), and `chains.zone_start_ts`, the timestamp of the zone
  chain's block 1, read once from the node.

## [0.1.3] - 2026-10-15

### Fixed

- **Web dashboard on a syncing node:** the block lattice's time axis drew
  one label per minute back to its oldest block, so with blocks a year
  old (a node still syncing) it drew hundreds of thousands of labels a
  frame and held the browser's main thread at 100%. The axis now picks
  minutes, hours or days to keep to a handful of ticks.
- While the node is behind (its newest block over two minutes old), the
  lattice ends at that block instead of now, so the blocks being synced
  are laid out rather than piled at the left edge; the timer shows a gap
  of days as `617d 15h` rather than a nine-digit count of seconds; and
  prime and region blocks, being history, get no banners or title cards
  (the events list still shows them).

## [0.1.2] - 2026-10-09

### Added

- **Uninstall:** `curl -fsSL …/uninstall.sh | sh` stops and removes every
  quai-dash service (systemd system and user, OpenRC) and binary the
  installers set up, downloading nothing; `--purge` also removes the
  configuration, logs and the quai-dash system user. Releases carry it
  next to `install.sh`.

### Changed

- The service installer runs the service as the node's user by default,
  found from the running rs-quai or go-quai process, instead of as
  whoever runs the installer; with nodes under several users it asks
  for `--run-as`, and it warns when `--run-as` names someone else (that
  user sees only the node's RPC: no node log, no peer map). The README's
  one-liners no longer need `--run-as`.
- `install.sh --uninstall` points to `uninstall.sh`, which also removes
  services without knowing how they were installed.

## [0.1.1] - 2026-10-09

### Added

- **One-line install:** `curl -fsSL …/install.sh | sh` installs the latest
  release's binary for this machine, only if it matches the release's
  SHA256SUMS; `--service systemd|openrc|user|auto --run-as USER` also
  installs the service. Releases now carry `install.sh` too.

### Changed

- The mining counters say which are since the stratum started (sent to
  node, mined) and which cover only the last N blocks (paid, with the
  time they span), in the web dashboard and the TUI.

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

[Unreleased]: https://github.com/mpoletiek/quai-node-dashboard/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/mpoletiek/quai-node-dashboard/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/mpoletiek/quai-node-dashboard/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/mpoletiek/quai-node-dashboard/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/mpoletiek/quai-node-dashboard/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/mpoletiek/quai-node-dashboard/releases/tag/v0.1.0
