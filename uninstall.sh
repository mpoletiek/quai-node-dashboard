#!/bin/sh
# Removes quai-dash: its services (systemd system and user units, OpenRC)
# and the binary wherever install.sh or scripts/install-service.sh put
# it. With --purge, also its configuration, logs and the quai-dash system
# user. Downloads nothing.
#
#   curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-node-dashboard/main/uninstall.sh | sh
#   curl -fsSL …/uninstall.sh | sh -s -- --purge
#
# Run with --help for the options.

set -eu

usage() {
	cat <<'USAGE'
Usage: uninstall.sh [options]   (piped: curl -fsSL …/uninstall.sh | sh -s -- [options])

Stops and removes the quai-dash services and binaries that the installers
set up. Configuration and logs stay unless --purge.

Options:
  --purge         also remove configuration, environment files, logs and the
                  quai-dash system user the service installer may have created
  --prefix DIR    also remove DIR/bin/quai-dash (an install with --prefix)
  --dry-run       print every action without doing it
  -h, --help      this help
USAGE
}

say() { printf 'quai-dash uninstall: %s\n' "$*"; }
die() { printf 'quai-dash uninstall: %s\n' "$*" >&2; exit 1; }

# Prints a command, then runs it unless this is a dry run.
run() {
	printf '+ %s\n' "$*"
	if [ "$DRY_RUN" = 0 ]; then
		"$@"
	fi
}

# Runs a command as root: directly when root, else through sudo.
as_root() {
	if [ "$(id -u)" = 0 ]; then
		run "$@"
	else
		command -v sudo >/dev/null 2>&1 || die "this needs root and there is no sudo: run it as root"
		run sudo "$@"
	fi
}

# Removes paths, as root only when their directory isn't ours to write.
remove() {
	for p in "$@"; do
		if [ -w "$(dirname "$p")" ]; then
			run rm -rf "$p"
		else
			as_root rm -rf "$p"
		fi
	done
}

# Whether $1 exists (a file, a directory, or a symlink, even dangling).
there() { [ -e "$1" ] || [ -L "$1" ]; }

main() {
	PURGE=0
	PREFIX=""
	DRY_RUN=0
	while [ $# -gt 0 ]; do
		case "$1" in
		--purge) PURGE=1; shift ;;
		--prefix) [ $# -ge 2 ] || die "--prefix needs a value"; PREFIX="$2"; shift 2 ;;
		--prefix=*) PREFIX="${1#*=}"; shift ;;
		--dry-run) DRY_RUN=1; shift ;;
		-h | --help) usage; exit 0 ;;
		*) die "unknown option $1 (see --help)" ;;
		esac
	done
	case "$PREFIX" in "" | /*) ;; *) die "--prefix must be an absolute path" ;; esac

	# QUAI_DASH_ROOT: a directory standing in for / (for tests).
	R="${QUAI_DASH_ROOT:-}"
	ROOT_USER=0
	[ "$(id -u)" != 0 ] || ROOT_USER=1
	did=0
	system_service=0

	# systemd system service: every quai-dash@USER instance, the template,
	# and drop-ins (older installs added one per instance).
	sysunit="$R/etc/systemd/system/quai-dash@.service"
	instances=""
	wants=""
	for w in "$R"/etc/systemd/system/*.wants/quai-dash@*.service; do
		there "$w" || continue
		instances="$instances ${w##*/}"
		wants="$wants
$w"
	done
	if command -v systemctl >/dev/null 2>&1; then
		instances="$instances $(systemctl list-units --all --plain --no-legend 'quai-dash@*.service' 2>/dev/null | awk '{print $1}')"
	fi
	# Unit names (no spaces): one per line, once each.
	instances=$(printf '%s' "$instances" | tr ' ' '\n' | sed '/^$/d' | sort -u)
	dropins=""
	for d in "$R"/etc/systemd/system/quai-dash@*.service.d; do
		there "$d" && dropins="$dropins $d"
	done
	if there "$sysunit" || [ -n "$instances" ] || [ -n "$dropins" ]; then
		say "systemd service"
		for u in $instances; do
			as_root systemctl disable --now "$u" || true
		done
		# Links that a failed disable (unit already gone, say) left behind.
		printf '%s\n' "$wants" | while IFS= read -r w; do
			if [ -n "$w" ] && there "$w"; then remove "$w"; fi
		done
		there "$sysunit" && remove "$sysunit"
		for d in $dropins; do remove "$d"; done
		as_root systemctl daemon-reload || true
		did=1
		system_service=1
	fi

	# systemd user unit: the invoking user's (a user unit is managed by
	# its own user's systemd, not root's).
	userunit="$HOME/.config/systemd/user/quai-dash.service"
	if [ "$ROOT_USER" = 0 ] && there "$userunit"; then
		say "systemd user service"
		run systemctl --user disable --now quai-dash.service || true
		remove "$userunit"
		run systemctl --user daemon-reload || true
		did=1
	elif [ "$ROOT_USER" = 1 ] && [ -n "${SUDO_USER:-}" ]; then
		uhome=$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6 || true)
		if [ -n "$uhome" ] && there "$R$uhome/.config/systemd/user/quai-dash.service"; then
			say "note: $SUDO_USER has a systemd user service; run this as $SUDO_USER (without sudo) to remove it"
		fi
	fi

	# OpenRC service.
	initscript="$R/etc/init.d/quai-dash"
	if there "$initscript"; then
		say "OpenRC service"
		as_root rc-service quai-dash stop || true
		as_root rc-update del quai-dash default || true
		remove "$initscript"
		did=1
		system_service=1
	fi

	# Binaries, where the installers put them (each path once).
	seen=""
	for b in "$R/usr/local/bin/quai-dash" "$HOME/.local/bin/quai-dash" ${PREFIX:+"$R$PREFIX/bin/quai-dash"}; do
		case "$seen" in *"|$b|"*) continue ;; esac
		seen="$seen|$b|"
		if [ -f "$b" ]; then
			say "binary $b"
			remove "$b"
			did=1
		fi
	done

	# Configuration, environment files and logs.
	kept=""
	for d in "$R/etc/quai-dash" "$R/etc/default/quai-dash" "$R/etc/conf.d/quai-dash" \
		"$R/var/log/quai-dash" "$R/var/log/quai-dash.log" \
		"$HOME/.config/quai-dash" "$HOME/.local/state/quai-dash"; do
		there "$d" || continue
		if [ "$PURGE" = 1 ]; then
			say "data $d"
			remove "$d"
			did=1
		else
			kept="$kept
  $d"
		fi
	done

	# The dedicated system user the service installer creates (no home,
	# no shell); only that one.
	if [ -z "$R" ]; then
		account=$(getent passwd quai-dash 2>/dev/null || true)
	else
		account=$(grep '^quai-dash:' "$R/etc/passwd" 2>/dev/null || true)
	fi
	case "$account" in
	*:/nonexistent:*nologin)
		if [ "$PURGE" = 1 ]; then
			say "system user quai-dash"
			as_root userdel quai-dash
			did=1
		else
			kept="$kept
  the system user quai-dash"
		fi
		;;
	esac

	if [ "$did" = 0 ] && [ -z "$kept" ]; then
		say "nothing to remove: no quai-dash service, binary or files found"
	elif [ "$did" = 0 ]; then
		say "no quai-dash service or binary found"
	fi
	if [ -n "$kept" ]; then
		say "kept (--purge removes them):$kept"
	fi
	# Anything else on PATH, e.g. from cargo install.
	other=$(command -v quai-dash 2>/dev/null || true)
	if [ -n "$other" ] && { [ "$DRY_RUN" = 1 ] || there "$other"; }; then
		case "$seen" in
		*"|$other|"*) ;;
		*) say "note: $other is still there (cargo install? then: cargo uninstall quai-node-dashboard)" ;;
		esac
	fi
	if [ "$system_service" = 1 ] && [ "$PURGE" = 1 ]; then
		say "a service user's own ~/.config/quai-dash, if any, stays: remove it as that user"
	fi
}

main "$@"
