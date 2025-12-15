#!/usr/bin/env bash
set -euo pipefail

# Use:
#   ./package-rhel.sh --version 0.0.1

die() { echo "ERROR: $*" >&2; exit 1; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

VERSION=""
RELEASE="1"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"; shift 2;;
    --release)
      RELEASE="${2:-}"; shift 2;;
    -h|--help)
      cat <<EOF
Usage: $0 --version <x.y.z> [--release <n>]

Examples:
  $0 --version 0.0.1
  $0 --version 0.0.1 --release 2
EOF
      exit 0;;
    *)
      die "unknown arg: $1";;
  esac
done

[[ -n "$VERSION" ]] || die "missing --version, e.g. $0 --version 0.0.1"

if ! [[ "$VERSION" =~ ^[0-9]+(\.[0-9]+){1,3}([\-+][A-Za-z0-9\.\-_]+)?$ ]]; then
  die "invalid version format: $VERSION"
fi

if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  echo "[*] Requesting sudo..."
  sudo -v
  ( while true; do sleep 30; sudo -n true 2>/dev/null || exit; done ) &
  SUDO_KEEPALIVE_PID=$!
  trap 'kill "${SUDO_KEEPALIVE_PID:-0}" 2>/dev/null || true' EXIT
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

[[ -f "Cargo.toml" ]] || die "Cargo.toml not found in $ROOT_DIR"

# ---- adapt here --------------------------------------------------------------
PKGNAME="magicbot"
SERVICE_USER="magicbot"
SERVICE_GROUP="magicbot"

STATE_DIR="/var/lib/magicbot"
RUN_DIR="/run/magicbot"
LOG_DIR="/var/log/magicbot"

# Optional: if your binary name differs, change it here:
BIN_PATH="$ROOT_DIR/target/release/${PKGNAME}"
# ------------------------------------------------------------------------------

echo "[*] Project root: $ROOT_DIR"
echo "[*] Package: $PKGNAME  Version: $VERSION  Release: $RELEASE"

echo "[*] Installing build dependencies via dnf..."
sudo dnf -y makecache
sudo dnf -y install \
  git ca-certificates curl \
  gcc gcc-c++ make \
  pkgconf-pkg-config \
  openssl-devel \
  tar gzip findutils which \
  rpm-build rpmdevtools \
  systemd-rpm-macros \
  shadow-utils \
  || die "dnf install failed"

load_cargo_env() {
  local env1="${CARGO_HOME:-$HOME/.cargo}/env"
  local env2="$HOME/.cargo/env"
  if [[ -f "$env1" ]]; then
    # shellcheck disable=SC1090
    source "$env1"
    return 0
  fi
  if [[ -f "$env2" ]]; then
    # shellcheck disable=SC1090
    source "$env2"
    return 0
  fi
  return 1
}

if ! command -v cargo >/dev/null 2>&1; then
  echo "[*] Installing Rust (rustup) for current user..."
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  load_cargo_env || true
else
  load_cargo_env || true
fi

need_cmd cargo
need_cmd rustc
echo "[*] rustc: $(rustc -V)"
echo "[*] cargo: $(cargo -V)"

echo "[*] Building ${PKGNAME} (release)..."
cargo build --release

[[ -f "$BIN_PATH" ]] || die "build succeeded but binary not found: $BIN_PATH"
chmod +x "$BIN_PATH"

RPMTOP="${HOME}/rpmbuild"
for d in BUILD BUILDROOT RPMS SOURCES SPECS SRPMS; do
  mkdir -p "${RPMTOP}/${d}"
done

TOPBUILD="${RPMTOP}/BUILD/${PKGNAME}-${VERSION}"
rm -rf "$TOPBUILD"
mkdir -p "$TOPBUILD"

# systemd unit
SERVICE_FILE="${TOPBUILD}/${PKGNAME}.service"
cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=MagicBot (Signal) daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_GROUP}
WorkingDirectory=/
ExecStart=/usr/bin/${PKGNAME} --daemon
Restart=always
RestartSec=2
Environment=RUST_BACKTRACE=1

# Hardening (safe defaults; loosen if you hit permission issues)
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true

# Allow writing state/log/run dirs
ReadWritePaths=${STATE_DIR} ${RUN_DIR} ${LOG_DIR}

[Install]
WantedBy=multi-user.target
EOF

# ship binary
cp -f "$BIN_PATH" "${TOPBUILD}/${PKGNAME}"
chmod 0755 "${TOPBUILD}/${PKGNAME}"

# docs/licenses (optional)
if [[ -f "$ROOT_DIR/LICENSE" ]]; then
  cp -f "$ROOT_DIR/LICENSE" "${TOPBUILD}/LICENSE"
fi
for f in README README.md README.txt; do
  if [[ -f "$ROOT_DIR/$f" ]]; then
    cp -f "$ROOT_DIR/$f" "${TOPBUILD}/$f"
  fi
done

TARBALL="${RPMTOP}/SOURCES/${PKGNAME}-${VERSION}.tar.gz"
echo "[*] Creating source tarball: $TARBALL"
tar -C "${RPMTOP}/BUILD" -czf "$TARBALL" "${PKGNAME}-${VERSION}"

SPEC="${RPMTOP}/SPECS/${PKGNAME}.spec"
echo "[*] Generating spec: $SPEC"

DOC_FILES=()
for f in README README.md README.txt; do
  [[ -f "${TOPBUILD}/${f}" ]] && DOC_FILES+=("$f")
done

LIC_FILES=()
for f in LICENSE LICENSE.txt LICENSE.md COPYING NOTICE; do
  [[ -f "${TOPBUILD}/${f}" ]] && LIC_FILES+=("$f")
done

DOC_LINE=""
if [[ "${#DOC_FILES[@]}" -gt 0 ]]; then
  DOC_LINE="%doc ${DOC_FILES[*]}"
fi

LIC_LINE=""
if [[ "${#LIC_FILES[@]}" -gt 0 ]]; then
  LIC_LINE="%license ${LIC_FILES[*]}"
fi

cat > "$SPEC" <<EOF
Name:           ${PKGNAME}
Version:        ${VERSION}
Release:        ${RELEASE}%{?dist}
Summary:        MagicBot - Signal group guard bot

License:        MIT
URL:            https://github.com/xujiegb/${PKGNAME}
Source0:        %{name}-%{version}.tar.gz

# prebuilt binary in Source0
%global debug_package %{nil}

BuildArch:      %{_arch}

# Runtime deps:
# - signal-cli is required at runtime but usually not in base repos; keep as "Recommends" via comment.
# - qrencode only needed for interactive "link device" flow (menu), not daemon mode.
Requires:       systemd
Requires:       shadow-utils

%description
MagicBot is a Signal group guard bot based on signal-cli.

Notes:
- This RPM ships only the magicbot binary and a systemd unit.
- You must install signal-cli separately and ensure it is available in PATH.
- For first-time setup, run "sudo magicbot" interactively to link/register and configure groups.
- Then enable the daemon service.

%prep
%autosetup -n %{name}-%{version}

%build
# no build here (binary is already built before rpmbuild)

%install
rm -rf %{buildroot}

# binary
install -D -m 0755 %{name} %{buildroot}%{_bindir}/%{name}

# systemd unit
install -D -m 0644 %{name}.service %{buildroot}%{_unitdir}/%{name}.service

# state/log/run dirs (owned by service user)
install -d -m 0755 %{buildroot}%{_localstatedir}/lib/magicbot
install -d -m 0755 %{buildroot}%{_localstatedir}/log/magicbot
install -d -m 0755 %{buildroot}%{_rundir}/magicbot

%pre
# Create magicbot user/group if not exist
getent group ${SERVICE_GROUP} >/dev/null || groupadd -r ${SERVICE_GROUP}
getent passwd ${SERVICE_USER} >/dev/null || useradd -r -g ${SERVICE_GROUP} -d %{_localstatedir}/lib/magicbot -s /sbin/nologin -c "magicbot service user" ${SERVICE_USER}
exit 0

%post
# ensure ownership
chown -R ${SERVICE_USER}:${SERVICE_GROUP} %{_localstatedir}/lib/magicbot || :
chown -R ${SERVICE_USER}:${SERVICE_GROUP} %{_localstatedir}/log/magicbot || :
chown -R ${SERVICE_USER}:${SERVICE_GROUP} %{_rundir}/magicbot || :
%systemd_post %{name}.service

%preun
%systemd_preun %{name}.service

%postun
%systemd_postun_with_restart %{name}.service

%files
${LIC_LINE}
${DOC_LINE}
%{_bindir}/%{name}
%{_unitdir}/%{name}.service
%dir %{_localstatedir}/lib/magicbot
%dir %{_localstatedir}/log/magicbot
%dir %{_rundir}/magicbot

%changelog
* $(date "+%a %b %d %Y") magicbot packager <packager@localhost> - %{version}-%{release}
- Automated build
EOF

echo "[*] Running rpmbuild..."
rpmbuild -ba "$SPEC"

echo
echo "[OK] RPM build finished."
echo "[*] RPMS output directory:"
find "${RPMTOP}/RPMS" -type f -name "${PKGNAME}-${VERSION}-${RELEASE}*.rpm" -print || true
echo
echo "[*] Install & first-time setup:"
cat <<'EOF'
    sudo dnf install -y ~/rpmbuild/RPMS/*/magicbot-*.rpm

    # runtime requirement (install signal-cli yourself, ensure it's in PATH)
    # - If you need QR link flow: install qrencode too.

    # first-time interactive init (login/link device, pick group, configure rules)
    sudo magicbot

    # then enable daemon
    sudo systemctl daemon-reload
    sudo systemctl enable --now magicbot
    sudo systemctl status magicbot --no-pager
EOF
