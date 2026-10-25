#!/bin/sh
# Start-up check for the Linux app on one distribution, run inside that
# distro's container by .github/workflows/linux-distros.yml. Each subcommand
# is its own workflow step, so a ❌ says which stage failed:
#
#   install <apt|dnf|pacman|zypper>  install the libraries a user would need
#   glibc <version>                  is this distro's glibc new enough?
#   libs                             does the app find every library it needs?
#   launch                           does it open and stay open?
#
# Expects tenshu-linux.tar.gz (from the build job) in the current directory.
set -u

APP=tenshu/coincube
OUT=result
mkdir -p "$OUT"

# Pretty name of this distro, for the summary and the screenshot file name.
distro() {
    # shellcheck disable=SC1091 # only exists on the target distro
    . /etc/os-release 2>/dev/null
    echo "${PRETTY_NAME:-unknown}"
}

summary() {
    echo "$1" >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
    echo "$1"
}

# Install one package at a time and keep going, so a package this distro does
# not have shows up as a named "not available" line rather than hiding the
# rest. The libs check below decides whether anything that matters is missing.
install_each() {
    for pkg in "$@"; do
        if ! sh -c "$INSTALL $pkg" > /dev/null 2>&1; then
            summary "- package not available: \`$pkg\`"
        fi
    done
}

# Test helpers (virtual screen, screenshot tool, software graphics). Their
# package names drift between releases; a miss here is not a user problem.
install_quiet() {
    for pkg in "$@"; do
        sh -c "$INSTALL $pkg" > /dev/null 2>&1 || echo "helper not available: $pkg"
    done
}

install() {
    case "$1" in
    apt)
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -q || exit 1
        apt-get install -y -q tar gzip || exit 1
        INSTALL="apt-get install -y -q --no-install-recommends"
        install_each libwebkit2gtk-4.1-0 libdbus-1-3 libudev1 ca-certificates
        install_quiet xvfb xauth dbus dbus-x11 libvulkan1 mesa-vulkan-drivers \
            libgl1-mesa-dri x11-apps procps fonts-dejavu-core
        ;;
    dnf)
        dnf install -y tar gzip || exit 1
        INSTALL="dnf install -y --setopt=install_weak_deps=False"
        install_each webkit2gtk4.1 dbus-libs systemd-libs ca-certificates
        install_quiet xorg-x11-server-Xvfb xorg-x11-xauth dbus-daemon dbus-x11 \
            vulkan-loader mesa-vulkan-drivers mesa-dri-drivers xwd xorg-x11-apps procps-ng \
            dejavu-sans-fonts
        ;;
    pacman)
        pacman -Syu --noconfirm tar gzip || exit 1
        INSTALL="pacman -S --noconfirm --needed"
        install_each webkit2gtk-4.1 dbus systemd-libs ca-certificates
        install_quiet xorg-server-xvfb xorg-xauth vulkan-icd-loader vulkan-swrast mesa \
            xorg-xwd procps-ng ttf-dejavu
        ;;
    zypper)
        zypper --non-interactive refresh || exit 1
        zypper --non-interactive install tar gzip || exit 1
        INSTALL="zypper --non-interactive install --no-recommends"
        install_each libwebkit2gtk-4_1-0 libdbus-1-3 libudev1 ca-certificates
        install_quiet xorg-x11-server-Xvfb xauth dbus-1 dbus-1-x11 libvulkan1 \
            libvulkan_lvp Mesa-dri xwd procps dejavu-fonts
        ;;
    *)
        echo "unknown package family: $1"
        exit 1
        ;;
    esac
    tar xzf tenshu-linux.tar.gz || exit 1
    distro > "$OUT/distro.txt"
}

glibc() {
    need="$1"
    have=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')
    lowest=$(printf '%s\n%s\n' "$need" "$have" | sort -V | head -1)
    if [ -n "$have" ] && [ "$lowest" = "$need" ]; then
        summary "✅ $(distro): glibc $have (app needs $need)"
    else
        summary "❌ $(distro): glibc ${have:-unknown} is too old, the app needs $need. Users see a \"GLIBC_$need not found\" error and the app does not open."
        exit 1
    fi
}

libs() {
    missing=$(ldd "$APP" 2>&1 | grep 'not found')
    if [ -z "$missing" ]; then
        summary "✅ $(distro): every library the app needs is installed"
    else
        summary "❌ $(distro): the app cannot find these libraries, so it will not open:"
        echo "$missing" | sed 's/^/    /' | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
        exit 1
    fi
}

# Open the app on a virtual screen. Staying open for 45 seconds counts as a
# pass; a crash or early exit is a fail. A screenshot is taken at 35 seconds.
launch() {
    export XDG_RUNTIME_DIR
    XDG_RUNTIME_DIR=$(mktemp -d)
    chmod 700 "$XDG_RUNTIME_DIR"
    Xvfb :99 -screen 0 1280x800x24 > "$OUT/xvfb.log" 2>&1 &
    sleep 3
    export DISPLAY=:99

    datadir=$(mktemp -d)
    if command -v dbus-run-session > /dev/null; then
        run="dbus-run-session --"
    else
        run=""
    fi
    # shellcheck disable=SC2086 # $run is deliberately split into words
    $run timeout 45 "$APP" --datadir "$datadir" --testnet > "$OUT/app.log" 2>&1 &
    app=$!

    sleep 35
    if command -v xwd > /dev/null; then
        xwd -root -display :99 -silent > "$OUT/screen.xwd" 2>/dev/null
    fi
    wait "$app"
    status=$?

    # timeout(1) exits 124 when it had to stop the app: it was still running.
    if [ "$status" -eq 124 ]; then
        summary "✅ $(distro): the app opened and kept running"
    else
        summary "❌ $(distro): the app closed by itself (exit code $status). Last lines of its output:"
        tail -n 25 "$OUT/app.log" | sed 's/^/    /' | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
        exit 1
    fi
}

case "${1:-}" in
install) install "${2:-}" ;;
glibc) glibc "${2:-}" ;;
libs) libs ;;
launch) launch ;;
*)
    echo "usage: $0 install <family> | glibc <version> | libs | launch"
    exit 1
    ;;
esac
