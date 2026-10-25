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

# The app loads these X11 libraries itself at start-up instead of linking
# them, so `ldd` never lists them. Without any one of them the app closes
# straight away with no message. Report which ones this distro lacks out of
# the box, then install them so the start-up test can get further.
x11_runtime() {
    found=$( (ldconfig -p 2>/dev/null || /sbin/ldconfig -p) )
    for soname in libX11.so.6 libX11-xcb.so.1 libXcursor.so.1 libXi.so.6 libxkbcommon-x11.so.0; do
        echo "$found" | grep -q "$soname" ||
            summary "- ⚠️ \`$soname\` was not installed with the app's other libraries. Without it the app closes silently on start."
    done
    install_each "$@"
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
        x11_runtime libx11-6 libx11-xcb1 libxcursor1 libxi6 libxkbcommon-x11-0
        install_quiet xvfb xauth dbus dbus-x11 libvulkan1 mesa-vulkan-drivers \
            libgl1-mesa-dri x11-apps procps fonts-dejavu-core
        ;;
    dnf)
        dnf install -y tar gzip || exit 1
        INSTALL="dnf install -y --setopt=install_weak_deps=False"
        # RHEL-family: some libraries only exist in the EPEL/CRB add-on repos.
        # Turn them on so we learn whether the app can run at all there; a
        # user would have to do the same.
        if [ -f /etc/almalinux-release ] || [ -f /etc/rocky-release ]; then
            dnf install -y epel-release > /dev/null 2>&1 && command -v crb > /dev/null &&
                crb enable > /dev/null 2>&1 &&
                summary "- note: EPEL and CRB repositories enabled; users need these too"
        fi
        install_each webkit2gtk4.1 dbus-libs systemd-libs ca-certificates
        x11_runtime libX11 libX11-xcb libXcursor libXi libxkbcommon-x11
        install_quiet xorg-x11-server-Xvfb xorg-x11-xauth dbus-daemon dbus-x11 \
            vulkan-loader mesa-vulkan-drivers mesa-dri-drivers xwd xorg-x11-apps procps-ng \
            dejavu-sans-fonts
        ;;
    pacman)
        pacman -Syu --noconfirm tar gzip || exit 1
        INSTALL="pacman -S --noconfirm --needed"
        install_each webkit2gtk-4.1 dbus systemd-libs ca-certificates
        x11_runtime libx11 libxcursor libxi libxkbcommon-x11
        install_quiet xorg-server-xvfb xorg-xauth vulkan-icd-loader vulkan-swrast mesa \
            xorg-xwd procps-ng ttf-dejavu
        ;;
    zypper)
        zypper --non-interactive refresh || exit 1
        zypper --non-interactive install tar gzip || exit 1
        INSTALL="zypper --non-interactive install --no-recommends"
        install_each libwebkit2gtk-4_1-0 libdbus-1-3 libudev1 ca-certificates
        x11_runtime libX11-6 libX11-xcb1 libXcursor1 libXi6 libxkbcommon-x11-0
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
    # Some images (openSUSE Tumbleweed) ship no getconf; ldd reports it too.
    [ -n "$have" ] || have=$(ldd --version 2>/dev/null | head -1 | grep -o '[0-9][0-9.]*$')
    lowest=$(printf '%s\n%s\n' "$need" "$have" | sort -V | head -1)
    if [ -z "$have" ]; then
        summary "❌ $(distro): could not read the glibc version (test problem, not an app problem)"
        exit 1
    elif [ "$lowest" = "$need" ]; then
        summary "✅ $(distro): glibc $have (app needs $need)"
    else
        summary "❌ $(distro): glibc $have is too old, the app needs $need. Users see a \"GLIBC_$need not found\" error and the app does not open."
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
        # The app's crash handler writes to its own log file, not the terminal.
        find "$datadir" -name '*.log' -exec tail -n 40 {} \; 2>/dev/null |
            sed 's/^/    /' | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
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
