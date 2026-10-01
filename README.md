# quai-dash

A live monitor for a Quai node, in the browser or the terminal, with two
looks: **GHOST** (cyan cyberbrain HUD, rotating dot globe) and **ANGEL**
(orange command center: seven-segment block timer, a three-panel vote on
the PoW algorithms, title cards for prime blocks).

It works the same against **rs-quai** and **go-quai**. Everything comes
from what both nodes already offer: their JSON-RPC, their `nodelogs`
directory, and the node process's TCP connections (for the peer map).

```sh
cargo build --release -p rsq-dash          # target/release/quai-dash

# on the node's host
quai-dash web --logs ~/node/nodelogs --listen 127.0.0.1:8095
quai-dash tui --logs ~/node/nodelogs --theme angel
```

## What it shows

- Zone height, seconds since the last block, block intervals, and the
  last block's transactions, workshares and gas.
- Prime, region and zone heads. A prime or region block shows up as a
  flash, banner or title card. Press `C` in the browser to turn these off.
- Hashrate, share time and pending workshares for KawPoW, SHA and Scrypt
  (`quai_getMiningInfo`, `quai_getPendingWorkShares`).
- Peers in and out, and a world map of connected peers.
- Base fee, exchange rate, block and workshare rewards, fees and supply.
- A block tape of the last few minutes, colored by tier. Bar height is gas
  used; dots are workshares.
- The node's log, colored by level, with a WARN+ filter and pause.
- Events: prime and region blocks, reorgs, stalls, RPC loss and recovery,
  and mismatches against a comparison node.
- With `--compare`, a second node (for example go-quai next to rs-quai)
  is checked block by block. The result is shown as a sync ratio.

## Options

| Option | Default | |
|---|---|---|
| `--rpc URL` | `http://127.0.0.1:9200` | zone JSON-RPC (also `QUAI_DASH_RPC`) |
| `--region URL`, `--prime URL` | same host, ports 9002 / 9001 | region and prime RPC |
| `--label NAME` | `QUAI NODE` | name on the dashboard |
| `--logs PATH` | none | a log file, or a `nodelogs` directory (go-quai: `zone-0-0.log`, rs-quai: `global.log`) |
| `--compare [LABEL=]URL` | none | a second node's zone RPC to compare blocks with |
| `--geoip-db FILE` | none | MaxMind GeoLite2/GeoIP2 City database for peer locations |
| `--geoip-online` | off | locate peers with ip-api.com instead |
| `--here LAT,LON` | none | this node's position on the map |
| `--stall-secs N` | 60 | seconds without a zone block before a stall event |
| `web --listen ADDR` | `127.0.0.1:8090` | where the web dashboard listens |
| `tui --theme ghost\|angel` | `ghost` | starting look |

## The peer map

Neither node reports peer addresses over RPC. quai-dash finds the process
listening on the zone RPC port and reads that process's established TCP
connections from `/proc`. The same code works for both nodes, with no
changes to the node.

- It needs Linux, and must run on the node's host as the node's user
  (or root).
- It sees TCP peers only; QUIC peers share one UDP socket and can't be
  told apart. So the map usually shows fewer peers than `net_peerCount`.
- Locations need a geolocation source:
  - `--geoip-db` looks peers up offline. GeoLite2-City is free from
    MaxMind with an account.
  - `--geoip-online` sends the peers' IP addresses to ip-api.com
    (free tier: HTTP, 15 requests a minute, 100 addresses each). It is
    off unless you ask for it.
  - Without either, the globe shows the peer count as an orbit.

## Watching a remote node

Run quai-dash on the node's host and forward its port:

```sh
ssh -L 8095:127.0.0.1:8095 user@node-host 'quai-dash web --logs ~/node/nodelogs --listen 127.0.0.1:8095'
# then open http://127.0.0.1:8095/
```

Pointing `--rpc` at a remote node works for everything except the peer
map.

## Keys

- **Web:** `T` switches the look; `C` turns full-screen moments on or
  off. The map can be dragged to rotate. A link ending in `#angel` or
  `#ghost` opens that look.
- **TUI:**
  - `t` switches the look; `m` shows the map full screen; `l` shows the
    log full height.
  - `?` opens help; `q` or Esc quits.
  - Below 100×28 the layout keeps only the essentials.

## Demo mode

`web/index.html` is a self-contained page. When no quai-dash server
answers (for example, opened on its own), it runs on clearly labeled
demo data, and keeps checking for a server every 10 s.

The world map is Natural Earth 110m land (public domain, via world-atlas
2.0.2), rasterized to a 240×120 bit mask in `assets/land-240x120.bin`.
