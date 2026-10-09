#!/usr/bin/env bash
# Installs quai-dash and runs `quai-dash web` as a service (systemd or
# OpenRC). The service needs no flags: quai-dash detects the node and
# reads its config file; extra settings go in /etc/default/quai-dash
# (systemd) or /etc/conf.d/quai-dash (OpenRC).
#
#   scripts/install-service.sh [options]
#
# Run `scripts/install-service.sh --help` for the options. It prints what
# it will do first, uses sudo only when not already root, and never
# overwrites a config or environment file without --force.

set -euo pipefail

usage() {
	cat <<'USAGE'
Usage: scripts/install-service.sh [options]

Installs the quai-dash binary and a service that runs `quai-dash web`.

Options:
  --init systemd|openrc  init system (default: detected)
  --run-as USER          user the service runs as (default: you; root only
                         when named). It must
                         be the node's user to read the node's logs and its
                         /proc entries for the peer map. `--run-as quai-dash`
                         creates that system user if it is missing.
  --user                 systemd only: a user unit (~/.config/systemd/user),
                         no root needed; binary in ~/.local/bin
  --prefix DIR           install the binary to DIR/bin
                         (default: /usr/local, or ~/.local with --user)
  --binary FILE          the quai-dash binary to install
                         (default: the one in a release archive, else
                         target/release/quai-dash, else the one on PATH)
  --uninstall            stop and remove the service and binary (config and
                         environment files are kept)
  --force                overwrite existing config and environment files
  --dry-run              print every action without doing it
  -h, --help             this help
USAGE
}

INIT=""
RUN_AS=""
USER_MODE=0
PREFIX=""
BINARY=""
UNINSTALL=0
FORCE=0
DRY_RUN=0

need_value() {
	if [ $# -lt 2 ] || [ -z "$2" ]; then
		echo "install-service: $1 needs a value" >&2
		exit 2
	fi
}

while [ $# -gt 0 ]; do
	case "$1" in
	--init) need_value "$@"; INIT="$2"; shift 2 ;;
	--init=*) INIT="${1#*=}"; shift ;;
	--run-as) need_value "$@"; RUN_AS="$2"; shift 2 ;;
	--run-as=*) RUN_AS="${1#*=}"; shift ;;
	--user) USER_MODE=1; shift ;;
	--prefix) need_value "$@"; PREFIX="$2"; shift 2 ;;
	--prefix=*) PREFIX="${1#*=}"; shift ;;
	--binary) need_value "$@"; BINARY="$2"; shift 2 ;;
	--binary=*) BINARY="${1#*=}"; shift ;;
	--uninstall) UNINSTALL=1; shift ;;
	--force) FORCE=1; shift ;;
	--dry-run) DRY_RUN=1; shift ;;
	-h | --help) usage; exit 0 ;;
	*) echo "install-service: unknown option $1 (see --help)" >&2; exit 2 ;;
	esac
done

die() {
	echo "install-service: $*" >&2
	exit 1
}

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONTRIB="$REPO/contrib"
ME="$(id -un)"

# --- What and where -------------------------------------------------------

if [ -z "$INIT" ]; then
	if [ -d /run/systemd/system ]; then
		INIT=systemd
	elif [ -d /run/openrc ] || command -v openrc-run >/dev/null 2>&1; then
		INIT=openrc
	else
		die "cannot tell the init system (no /run/systemd/system or /run/openrc); pass --init systemd|openrc"
	fi
fi
case "$INIT" in
systemd | openrc) ;;
*) die "--init must be systemd or openrc, not '$INIT'" ;;
esac

if [ "$USER_MODE" = 1 ]; then
	[ "$INIT" = systemd ] || die "--user is systemd only: OpenRC user services are not supported by this installer. Install the system service with --run-as $ME instead, or start \`quai-dash web\` from your own session (tmux, or a crontab @reboot line)."
	[ -z "$RUN_AS" ] || [ "$RUN_AS" = "$ME" ] || die "--user runs as you; --run-as $RUN_AS does not apply"
	[ "$(id -u)" != 0 ] || die "--user installs for the invoking user; don't run it as root"
	RUN_AS="$ME"
	: "${PREFIX:=$HOME/.local}"
else
	if [ -z "$RUN_AS" ]; then
		RUN_AS="${SUDO_USER:-$ME}"
		[ "$RUN_AS" != root ] || die "the service would run as root: pass --run-as USER (the node's user), or --run-as root if you mean it"
	fi
	: "${PREFIX:=/usr/local}"
fi
case "$PREFIX" in
/*) ;;
*) die "--prefix must be an absolute path" ;;
esac
BIN="$PREFIX/bin/quai-dash"

if [ "$UNINSTALL" = 0 ] && [ -z "$BINARY" ]; then
	if [ -x "$REPO/quai-dash" ]; then
		# A release archive: the binary sits next to scripts/.
		BINARY="$REPO/quai-dash"
	elif [ -x "$REPO/target/release/quai-dash" ]; then
		BINARY="$REPO/target/release/quai-dash"
	elif command -v quai-dash >/dev/null 2>&1; then
		BINARY="$(command -v quai-dash)"
	else
		die "no quai-dash binary: run \`cargo build --release\` first, or pass --binary FILE"
	fi
fi
if [ "$UNINSTALL" = 0 ] && [ "$DRY_RUN" = 0 ]; then
	[ -x "$BINARY" ] || die "$BINARY is not an executable file"
fi

SUDO=""
if [ "$USER_MODE" = 0 ] && [ "$(id -u)" != 0 ]; then
	if [ "$DRY_RUN" = 1 ]; then
		SUDO="sudo"
	else
		command -v sudo >/dev/null 2>&1 || die "not root and no sudo: run this as root"
		SUDO="sudo"
	fi
fi

CREATE_USER=0
if [ "$USER_MODE" = 0 ] && [ "$UNINSTALL" = 0 ] && ! id -u "$RUN_AS" >/dev/null 2>&1; then
	[ "$RUN_AS" = quai-dash ] || die "no user '$RUN_AS' (only the dedicated 'quai-dash' user is created automatically)"
	CREATE_USER=1
fi

if [ "$USER_MODE" = 1 ]; then
	CFG_DIR="$HOME/.config/quai-dash"
	UNIT_DIR="$HOME/.config/systemd/user"
	UNIT="$UNIT_DIR/quai-dash.service"
	ENV_FILE="$CFG_DIR/env"
	SERVICE="quai-dash.service"
	SYSTEMCTL=(systemctl --user)
else
	CFG_DIR="/etc/quai-dash"
	SYSTEMCTL=($SUDO systemctl)
	if [ "$INIT" = systemd ]; then
		UNIT="/etc/systemd/system/quai-dash@.service"
		ENV_FILE="/etc/default/quai-dash"
		SERVICE="quai-dash@$RUN_AS.service"
	else
		UNIT="/etc/init.d/quai-dash"
		ENV_FILE="/etc/conf.d/quai-dash"
		SERVICE="quai-dash"
	fi
fi
CONFIG="$CFG_DIR/config.toml"

# Output: a system unit logs to the journal and OpenRC to
# /var/log/quai-dash/ (both opened by root or as the user in root-owned
# directories); only the --user unit, which runs entirely as you, logs to
# your hidden state directory.
LOG_DIR=""
LOG_FILE=""
if [ "$USER_MODE" = 1 ]; then
	LOG_DIR="$HOME/.local/state/quai-dash"
	LOG_FILE="$LOG_DIR/quai-dash.log"
fi
# Older installs gave each systemd instance a drop-in that sent output to
# the run-as user's home; systemd opens that path as root, so it goes.
OLD_DROPIN=""
if [ "$INIT" = systemd ] && [ "$USER_MODE" = 0 ]; then
	OLD_DROPIN="/etc/systemd/system/quai-dash@$RUN_AS.service.d/log.conf"
fi

# --- Actions ----------------------------------------------------------------

# Prints a command, then runs it unless this is a dry run.
run() {
	echo "+ $*"
	if [ "$DRY_RUN" = 0 ]; then
		"$@"
	fi
}

# Rendered templates go here (made in this shell: render runs in $(…)).
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Renders a template with sed expressions into a temp file and prints its path.
render() {
	local src="$1"
	shift
	local out
	out="$TMP/$(basename "$src").$RANDOM"
	sed "$@" "$src" >"$out"
	echo "$out"
}

# Installs a file; a config or environment file (keep=1) is left alone
# when it exists, unless --force. A rendered template names its source.
place() {
	local src="$1" dest="$2" mode="$3" keep="${4:-0}" from="${5:-}" sudo="${SUDO}"
	[ "$USER_MODE" = 1 ] && sudo=""
	if [ "$keep" = 1 ] && [ -e "$dest" ] && [ "$FORCE" = 0 ]; then
		echo "= keep $dest (exists; --force overwrites it)"
		return
	fi
	[ -z "$from" ] || echo "# $dest: rendered from ${from#"$REPO"/}"
	run $sudo install -D -m "$mode" "$src" "$dest"
}

# The --user unit's log directory (made as you: no root involved).
make_log_dir() {
	[ -d "$LOG_DIR" ] && [ "$DRY_RUN" = 0 ] && return
	run mkdir -p -m 0700 "$LOG_DIR"
}

remove() {
	local sudo="${SUDO}"
	[ "$USER_MODE" = 1 ] && sudo=""
	if [ -e "$1" ] || [ "$DRY_RUN" = 1 ]; then
		run $sudo rm -f "$1"
	fi
}

ESC_BIN="$(printf '%s' "$BIN" | sed 's/[&|\\]/\\&/g')"

plan() {
	echo "quai-dash service: $([ "$UNINSTALL" = 1 ] && echo uninstall || echo install)"
	echo "  init system  $INIT$([ "$USER_MODE" = 1 ] && echo ' (user unit)')"
	echo "  service      $SERVICE, runs as $RUN_AS$([ "$CREATE_USER" = 1 ] && echo ' (created)')"
	echo "  binary       $BIN$([ "$UNINSTALL" = 0 ] && echo " (from $BINARY)")"
	echo "  unit/init    $UNIT"
	echo "  environment  $ENV_FILE$([ "$UNINSTALL" = 1 ] && echo ' (kept)')"
	echo "  config       $CONFIG$([ "$UNINSTALL" = 1 ] && echo ' (kept)')"
	if [ -n "$LOG_FILE" ]; then
		echo "  logs         $LOG_FILE$([ "$UNINSTALL" = 1 ] && echo ' (kept)')"
	elif [ "$INIT" = systemd ]; then
		echo "  logs         the journal (journalctl -u $SERVICE)"
	else
		echo "  logs         /var/log/quai-dash/quai-dash.log"
	fi
	if [ -n "$SUDO" ] && [ "$USER_MODE" = 0 ]; then
		echo "  privileges   commands marked sudo run as root"
	fi
	[ "$DRY_RUN" = 1 ] && echo "  dry run: nothing is changed"
	echo
}

install_service() {
	if [ "$CREATE_USER" = 1 ]; then
		run $SUDO useradd --system --no-create-home --home-dir /nonexistent --shell /sbin/nologin quai-dash
	fi
	place "$BINARY" "$BIN" 0755
	place "$CONTRIB/config.toml" "$CONFIG" 0644 1
	[ -z "$LOG_DIR" ] || make_log_dir
	if [ -n "$OLD_DROPIN" ] && [ -e "$OLD_DROPIN" ]; then
		remove "$OLD_DROPIN"
		run $SUDO rmdir "${OLD_DROPIN%/*}" || true
	fi
	if [ "$INIT" = systemd ]; then
		if [ "$USER_MODE" = 1 ]; then
			place "$(render "$CONTRIB/systemd/quai-dash.user.service" -e "s|^ExecStart=%h/.local/bin/quai-dash|ExecStart=$ESC_BIN|")" \
				"$UNIT" 0644 0 "$CONTRIB/systemd/quai-dash.user.service (ExecStart=$BIN)"
		else
			place "$(render "$CONTRIB/systemd/quai-dash@.service" -e "s|^ExecStart=/usr/local/bin/quai-dash|ExecStart=$ESC_BIN|")" \
				"$UNIT" 0644 0 "$CONTRIB/systemd/quai-dash@.service (ExecStart=$BIN)"
		fi
		place "$CONTRIB/systemd/quai-dash.default" "$ENV_FILE" 0644 1
		run "${SYSTEMCTL[@]}" daemon-reload
		run "${SYSTEMCTL[@]}" enable --now "$SERVICE"
	else
		place "$(render "$CONTRIB/openrc/quai-dash" -e "s|^: \"\${QUAI_DASH_BIN:=/usr/local/bin/quai-dash}\"|: \"\${QUAI_DASH_BIN:=$ESC_BIN}\"|")" \
			"$UNIT" 0755 0 "$CONTRIB/openrc/quai-dash (QUAI_DASH_BIN=$BIN)"
		place "$(render "$CONTRIB/openrc/quai-dash.confd" -e "s|^QUAI_DASH_USER=.*|QUAI_DASH_USER=\"$RUN_AS\"|")" \
			"$ENV_FILE" 0644 1 "$CONTRIB/openrc/quai-dash.confd (QUAI_DASH_USER=$RUN_AS)"
		run $SUDO rc-update add quai-dash default
		run $SUDO rc-service quai-dash restart
	fi
	echo
	echo "quai-dash runs as $RUN_AS. It reads the node's logs and /proc (peer map)"
	echo "only when that is the node's user; otherwise it shows what RPC offers."
	echo "Check what it found with:"
	if [ "$RUN_AS" = "$ME" ]; then
		echo "  $BIN config"
	else
		echo "  sudo -u $RUN_AS $BIN config"
	fi
	if [ "$USER_MODE" = 1 ]; then
		echo "To keep it running after you log out: loginctl enable-linger $ME"
	fi
	echo "Dashboard: http://127.0.0.1:8090/ (an SSH tunnel reaches it from elsewhere)"
}

uninstall_service() {
	if [ "$INIT" = systemd ]; then
		if [ "$USER_MODE" = 1 ]; then
			run "${SYSTEMCTL[@]}" disable --now "$SERVICE" || true
		else
			# Every instance of the template: enabled ones and running ones.
			local units u
			units="$(
				{
					for u in /etc/systemd/system/*.wants/quai-dash@*.service; do
						[ -e "$u" ] && basename "$u"
					done
					systemctl list-units --all --plain --no-legend 'quai-dash@*.service' 2>/dev/null | awk '{print $1}' || true
				} | sort -u
			)"
			[ -n "$units" ] || units="$SERVICE"
			for u in $units; do
				run "${SYSTEMCTL[@]}" disable --now "$u" || true
			done
		fi
		remove "$UNIT"
		if [ "$USER_MODE" = 0 ]; then
			for d in /etc/systemd/system/quai-dash@*.service.d; do
				[ -e "$d/log.conf" ] && remove "$d/log.conf" && run $SUDO rmdir "$d" || true
			done
		fi
		run "${SYSTEMCTL[@]}" daemon-reload
	else
		run $SUDO rc-service quai-dash stop || true
		run $SUDO rc-update del quai-dash default || true
		remove "$UNIT"
	fi
	remove "$BIN"
	echo
	echo "Kept $CONFIG and $ENV_FILE$([ -n "$LOG_DIR" ] && echo " and $LOG_DIR")$([ "$INIT" = openrc ] && echo " and /var/log/quai-dash"); delete them by hand if you no longer need them."
}

plan
if [ "$UNINSTALL" = 1 ]; then
	uninstall_service
else
	install_service
fi
