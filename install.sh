#!/bin/sh
# Installs quai-dash from a GitHub release: the static Linux binary for
# this machine, checked against the release's SHA256SUMS, and optionally
# a service (systemd or OpenRC) through the archive's own
# scripts/install-service.sh.
#
#   curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-node-dashboard/main/install.sh | sh
#   curl -fsSL …/install.sh | sh -s -- --service systemd --run-as node
#
# Run with --help for the options. Nothing is installed unless the
# archive's checksum matches.

set -eu

REPO="mpoletiek/quai-node-dashboard"

usage() {
	cat <<'USAGE'
Usage: install.sh [options]   (piped: curl -fsSL …/install.sh | sh -s -- [options])

Installs the quai-dash binary from a GitHub release, verified against the
release's SHA256SUMS.

Options:
  --version X     release to install, e.g. 0.1.0 (default: the latest)
  --service S     also install a service that runs `quai-dash web`:
                    systemd   system unit, quai-dash@USER (needs root or sudo)
                    openrc    OpenRC service (needs root or sudo)
                    user      systemd user unit, no root
                    auto      systemd or OpenRC, whichever this host runs
                  (default: none, only the binary)
  --run-as USER   user the service runs as: the node's user, so it can read
                  the node's logs and /proc (default: you)
  --prefix DIR    install the binary to DIR/bin (default: /usr/local with a
                  system service or as root, else ~/.local)
  --uninstall     remove the binary (and, with --service, the service)
  --dry-run       print every action without doing it
  -h, --help      this help
USAGE
}

say() { printf 'quai-dash install: %s\n' "$*"; }
die() { printf 'quai-dash install: %s\n' "$*" >&2; exit 1; }

# Prints a command, then runs it unless this is a dry run.
run() {
	printf '+ %s\n' "$*"
	if [ "$DRY_RUN" = 0 ]; then
		"$@"
	fi
}

# Downloads $1 to $2 (curl, else wget).
fetch() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
	elif command -v wget >/dev/null 2>&1; then
		wget -q -O "$2" "$1"
	else
		die "needs curl or wget"
	fi
}

# Whether $1 looks like a release tag: v, a digit, then only what
# versions use (no slashes, spaces or quotes on their way into a URL).
valid_tag() {
	case "$1" in
	v[0-9]*) ;;
	*) return 1 ;;
	esac
	case "$1" in
	*[!0-9A-Za-z.+-]*) return 1 ;;
	esac
}

# The latest release's tag (never a draft or prerelease): where
# github.com/…/releases/latest redirects (no API rate limit), else the
# API's tag_name.
latest_tag() {
	url="https://github.com/$REPO/releases/latest"
	if command -v curl >/dev/null 2>&1; then
		final=$(curl -fsSLI --proto '=https' --tlsv1.2 -o /dev/null -w '%{url_effective}' "$url" 2>/dev/null) || final=""
	else
		final=$(wget -S --spider "$url" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | tail -n 1) || final=""
	fi
	final=$(printf '%s' "$final" | tr -d '\r')
	tag=""
	case "$final" in
	*/releases/tag/*)
		tag="${final##*/releases/tag/}"
		tag="${tag%%[?#]*}"
		;;
	esac
	if ! valid_tag "$tag"; then
		fetch "https://api.github.com/repos/$REPO/releases/latest" "$TMP/latest.json" 2>/dev/null || true
		# One JSON member per line, whether the reply is pretty or minified.
		# shellcheck disable=SC2020
		tag=$(tr ',{}' '\n\n\n' <"$TMP/latest.json" 2>/dev/null |
			sed -n 's/^[[:space:]]*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
	fi
	if ! valid_tag "$tag"; then
		[ -z "$tag" ] || die "the latest release's tag \"$tag\" isn't a version; pass --version X"
		die "could not find the latest release; pass --version X"
	fi
	printf '%s\n' "$tag"
}

# SHA-256 of a file, by whichever tool this system has.
sha256() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | cut -d' ' -f1
	elif command -v openssl >/dev/null 2>&1; then
		openssl dgst -sha256 "$1" | sed 's/.*= *//'
	else
		die "needs sha256sum, shasum or openssl to check the download"
	fi
}

main() {
	VERSION=""
	SERVICE=none
	RUN_AS=""
	PREFIX=""
	UNINSTALL=0
	DRY_RUN=0
	while [ $# -gt 0 ]; do
		case "$1" in
		--version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift 2 ;;
		--version=*) VERSION="${1#*=}"; shift ;;
		--service) [ $# -ge 2 ] || die "--service needs a value"; SERVICE="$2"; shift 2 ;;
		--service=*) SERVICE="${1#*=}"; shift ;;
		--run-as) [ $# -ge 2 ] || die "--run-as needs a value"; RUN_AS="$2"; shift 2 ;;
		--run-as=*) RUN_AS="${1#*=}"; shift ;;
		--prefix) [ $# -ge 2 ] || die "--prefix needs a value"; PREFIX="$2"; shift 2 ;;
		--prefix=*) PREFIX="${1#*=}"; shift ;;
		--uninstall) UNINSTALL=1; shift ;;
		--dry-run) DRY_RUN=1; shift ;;
		-h | --help) usage; exit 0 ;;
		*) die "unknown option $1 (see --help)" ;;
		esac
	done
	case "$SERVICE" in
	none | systemd | openrc | user | auto) ;;
	*) die "--service must be systemd, openrc, user or auto, not '$SERVICE'" ;;
	esac
	[ -z "$RUN_AS" ] || [ "$SERVICE" != none ] || die "--run-as is for a service: add --service"
	case "$PREFIX" in "" | /*) ;; *) die "--prefix must be an absolute path" ;; esac

	[ "$(uname -s)" = Linux ] || die "quai-dash releases are Linux binaries; this is $(uname -s)"
	case "$(uname -m)" in
	x86_64 | amd64) ARCH=x86_64 ;;
	aarch64 | arm64) ARCH=aarch64 ;;
	*) die "no release binary for $(uname -m) (x86_64 and aarch64 only); build from source instead" ;;
	esac

	TMP=$(mktemp -d)
	trap 'rm -rf "$TMP"' EXIT
	if [ -z "$VERSION" ]; then
		TAG=$(latest_tag)
	else
		TAG="v${VERSION#v}"
		valid_tag "$TAG" || die "--version '$VERSION' doesn't look like a version (e.g. 0.1.0)"
	fi
	VERSION="${TAG#v}"
	NAME="quai-dash-$VERSION-$ARCH-linux"
	BASE="https://github.com/$REPO/releases/download/$TAG"

	say "downloading $NAME.tar.gz ($TAG)"
	fetch "$BASE/$NAME.tar.gz" "$TMP/$NAME.tar.gz" || die "download failed: $BASE/$NAME.tar.gz"
	fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" || die "download failed: $BASE/SHA256SUMS"

	# Never install what doesn't match the release's checksum.
	want=$(sed -n "s/^\([0-9a-f]\{64\}\)  $NAME\.tar\.gz\$/\1/p" "$TMP/SHA256SUMS")
	[ -n "$want" ] || die "SHA256SUMS has no entry for $NAME.tar.gz"
	got=$(sha256 "$TMP/$NAME.tar.gz")
	[ "$got" = "$want" ] || die "checksum mismatch for $NAME.tar.gz (expected $want, got $got): not installing"
	say "checksum OK"

	tar -xzf "$TMP/$NAME.tar.gz" -C "$TMP"
	DIR="$TMP/$NAME"
	[ -x "$DIR/quai-dash" ] || die "the archive has no quai-dash binary"

	# A service: the archive's installer does the rest (binary, unit or
	# init script, settings, enable and start).
	if [ "$SERVICE" != none ]; then
		command -v bash >/dev/null 2>&1 || die "installing a service needs bash (Alpine: apk add bash)"
		set -- --binary "$DIR/quai-dash"
		case "$SERVICE" in
		systemd | openrc) set -- "$@" --init "$SERVICE" ;;
		user) set -- "$@" --user ;;
		esac
		[ -z "$RUN_AS" ] || set -- "$@" --run-as "$RUN_AS"
		[ -z "$PREFIX" ] || set -- "$@" --prefix "$PREFIX"
		[ "$UNINSTALL" = 0 ] || set -- "$@" --uninstall
		[ "$DRY_RUN" = 0 ] || set -- "$@" --dry-run
		bash "$DIR/scripts/install-service.sh" "$@"
		return
	fi

	# The binary only.
	if [ -z "$PREFIX" ]; then
		if [ "$(id -u)" = 0 ]; then PREFIX=/usr/local; else PREFIX="$HOME/.local"; fi
	fi
	BIN="$PREFIX/bin/quai-dash"
	SUDO=""
	if [ "$(id -u)" != 0 ] && ! { mkdir -p "$PREFIX/bin" 2>/dev/null && [ -w "$PREFIX/bin" ]; }; then
		command -v sudo >/dev/null 2>&1 || die "can't write $PREFIX/bin and there is no sudo: run as root, or pick --prefix"
		SUDO=sudo
	fi
	if [ "$UNINSTALL" = 1 ]; then
		if [ -e "$BIN" ] || [ "$DRY_RUN" = 1 ]; then
			run $SUDO rm -f "$BIN"
		else
			say "nothing at $BIN"
		fi
		return
	fi
	run $SUDO install -D -m 0755 "$DIR/quai-dash" "$BIN"
	[ "$DRY_RUN" = 1 ] || "$BIN" --version
	case ":$PATH:" in
	*":$PREFIX/bin:"*) ;;
	*) say "note: $PREFIX/bin is not on your PATH; run $BIN, or add it" ;;
	esac
	cat <<EOF

Installed $BIN. On the node's host, as the node's user:
  quai-dash web      # then open http://127.0.0.1:8090/ (an SSH tunnel reaches it from elsewhere)
  quai-dash tui      # the terminal version
  quai-dash config   # what it found, and why
To run it as a service, run this installer again with --service systemd|openrc|user --run-as USER.
EOF
}

main "$@"
