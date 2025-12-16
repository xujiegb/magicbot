#!/usr/bin/env bash
set -euo pipefail

# ========================
# helpers
# ========================
die() { echo "ERROR: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || die "missing $1"; }

# ========================
# args
# ========================
VERSION=""
RELEASE="${RELEASE:-1}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --release)
      RELEASE="${2:-}"
      shift 2
      ;;
    -h|--help)
      cat <<EOF
Usage: $0 [--version x.y.z] [--release n]

Examples:
  $0 --version 0.13.22
  $0
EOF
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

# ========================
# tools
# ========================
need git
need java
need rpmbuild
need tar

JAVA_VERSION="$(java -version 2>&1 | head -n1 || true)"
echo "[*] Java: $JAVA_VERSION"

# ========================
# resolve latest version if not specified
# ========================
if [[ -z "$VERSION" ]]; then
  echo "[*] Resolving latest stable version from GitHub…"
  VERSION="$(
    curl -fsSL https://api.github.com/repos/AsamK/signal-cli/releases \
      | grep '"tag_name"' \
      | sed -E 's/.*"v([^"]+)".*/\1/' \
      | grep -E '^[0-9]+(\.[0-9]+){1,3}$' \
      | head -n1
  )"
  [[ -n "$VERSION" ]] || die "failed to detect latest version"
fi

[[ "$VERSION" =~ ^[0-9]+(\.[0-9]+){1,3}$ ]] || die "invalid version: $VERSION"

echo "[*] Building signal-cli version: $VERSION"

# ========================
# paths
# ========================
PKGNAME="signal-cli"
TOP="$HOME/rpmbuild"
SRCROOT="$(pwd)"

INSTALL_PREFIX="/opt"
INSTALL_DIR="${INSTALL_PREFIX}/${PKGNAME}-${VERSION}"

# ========================
# prepare rpmbuild tree
# ========================
for d in BUILD BUILDROOT RPMS SOURCES SPECS SRPMS; do
  mkdir -p "$TOP/$d"
done

WORK="$TOP/BUILD/${PKGNAME}-${VERSION}"
rm -rf "$WORK"
mkdir -p "$WORK"

# ========================
# checkout source
# ========================
if [[ ! -d "$WORK/src" ]]; then
  git clone https://github.com/AsamK/signal-cli.git "$WORK/src"
fi

cd "$WORK/src"
git fetch --tags
git checkout "v$VERSION"

# ========================
# gradle build
# ========================
echo "[*] Running Gradle build"
./gradlew clean build

echo "[*] Running Gradle installDist"
./gradlew installDist

DISTDIR="$WORK/src/build/install/signal-cli"
[[ -x "$DISTDIR/bin/signal-cli" ]] || die "installDist failed"

# ========================
# prepare rpm source tarball
# ========================
cd "$TOP/BUILD"
rm -rf "${PKGNAME}-${VERSION}"
mkdir -p "${PKGNAME}-${VERSION}"

cp -a "$DISTDIR"/* "${PKGNAME}-${VERSION}/"

tar czf "$TOP/SOURCES/${PKGNAME}-${VERSION}.tar.gz" "${PKGNAME}-${VERSION}"

# ========================
# spec file
# ========================
SPEC="$TOP/SPECS/${PKGNAME}.spec"

cat > "$SPEC" <<EOF
Name:           ${PKGNAME}
Version:        ${VERSION}
Release:        ${RELEASE}%{?dist}
Summary:        Signal command-line interface

License:        GPL-3.0
URL:            https://github.com/AsamK/signal-cli
Source0:        %{name}-%{version}.tar.gz

BuildArch:      %{_arch}
Requires:       java-17-openjdk-headless

%global debug_package %{nil}

%description
signal-cli is a command-line interface for the Signal messaging service.

This package is built from source using Gradle and includes the
architecture-specific libsignal-client JNI built at compile time.

%prep
%autosetup -n %{name}-%{version}

%build
# built by Gradle before rpmbuild

%install
rm -rf %{buildroot}

mkdir -p %{buildroot}${INSTALL_DIR}
mkdir -p %{buildroot}%{_bindir}

cp -a * %{buildroot}${INSTALL_DIR}/
ln -sf ${INSTALL_DIR}/bin/signal-cli %{buildroot}%{_bindir}/signal-cli

%files
${INSTALL_DIR}
%{_bindir}/signal-cli

%changelog
* $(date "+%a %b %d %Y") packager <packager@localhost> - ${VERSION}-${RELEASE}
- Built from source using Gradle installDist
EOF

# ========================
# build rpm
# ========================
rpmbuild -ba "$SPEC"

echo
echo "[OK] RPM built:"
find "$TOP/RPMS" -name "signal-cli-${VERSION}-${RELEASE}*.rpm"
