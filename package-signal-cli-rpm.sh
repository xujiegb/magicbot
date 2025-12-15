#!/usr/bin/env bash
set -euo pipefail

# ========================
# Config
# ========================
VERSION="${VERSION:-0.13.22}"
RELEASE="${RELEASE:-1}"
ARCH="$(uname -m)"

PKGNAME="signal-cli"
TOP="${HOME}/rpmbuild"

INSTALL_PREFIX="/opt"
INSTALL_DIR="${INSTALL_PREFIX}/${PKGNAME}-${VERSION}"

BASE_URL="https://github.com/AsamK/signal-cli/releases/download/v${VERSION}"
JAR="signal-cli-${VERSION}.jar"

case "$ARCH" in
  x86_64)   LIB_ARCH="linux-x86_64" ;;
  aarch64)  LIB_ARCH="linux-aarch64" ;;
  *) echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

LIB_TAR="libsignal-client-${VERSION}-${LIB_ARCH}.tar.gz"

# ========================
need() { command -v "$1" >/dev/null || { echo "missing $1"; exit 1; }; }
need rpmbuild
need curl
need tar
need java

# ========================
# Prepare rpmbuild dirs
# ========================
for d in BUILD BUILDROOT RPMS SOURCES SPECS SRPMS; do
  mkdir -p "${TOP}/${d}"
done

WORK="${TOP}/BUILD/${PKGNAME}-${VERSION}"
rm -rf "$WORK"
mkdir -p "$WORK"

# ========================
# Download artifacts
# ========================
echo "[*] Downloading signal-cli jar"
curl -fL -o "${WORK}/${JAR}" "${BASE_URL}/${JAR}"

echo "[*] Downloading libsignal-client"
curl -fL -o "${WORK}/${LIB_TAR}" "${BASE_URL}/${LIB_TAR}"

# ========================
# Extract native libs
# ========================
mkdir -p "${WORK}/lib"
tar -xf "${WORK}/${LIB_TAR}" -C "${WORK}"

# 官方 tar 一般带 lib/
if [[ -d "${WORK}/lib" ]]; then
  :
elif [[ -d "${WORK}/libsignal-client" ]]; then
  mv "${WORK}/libsignal-client/"* "${WORK}/lib/"
else
  find "${WORK}" -name "libsignal*.so" -exec mv {} "${WORK}/lib/" \;
fi

# ========================
# Wrapper
# ========================
cat > "${WORK}/signal-cli" <<EOF
#!/usr/bin/env bash
DIR="\$(cd "\$(dirname "\$0")" && pwd)"
export LD_LIBRARY_PATH="\$DIR/lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
exec java -jar "\$DIR/${JAR}" "\$@"
EOF
chmod 0755 "${WORK}/signal-cli"

# ========================
# Source tarball
# ========================
tar -C "${TOP}/BUILD" -czf "${TOP}/SOURCES/${PKGNAME}-${VERSION}.tar.gz" "${PKGNAME}-${VERSION}"

# ========================
# Spec file
# ========================
SPEC="${TOP}/SPECS/${PKGNAME}.spec"

cat > "$SPEC" <<EOF
Name:           ${PKGNAME}
Version:        ${VERSION}
Release:        ${RELEASE}%{?dist}
Summary:        Signal CLI with bundled libsignal-client

License:        GPL-3.0
URL:            https://github.com/AsamK/signal-cli
Source0:        %{name}-%{version}.tar.gz

BuildArch:      %{_arch}
Requires:       java-17-openjdk-headless

%global debug_package %{nil}

%description
signal-cli packaged with matching libsignal-client native library.

This RPM is self-contained and does not rely on system libsignal-client.

%prep
%autosetup -n %{name}-%{version}

%build
# nothing

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}${INSTALL_DIR}

cp -a signal-cli %{buildroot}${INSTALL_DIR}/
cp -a ${JAR} %{buildroot}${INSTALL_DIR}/
cp -a lib %{buildroot}${INSTALL_DIR}/

ln -sf ${INSTALL_DIR}/signal-cli %{buildroot}%{_bindir}/signal-cli

%files
${INSTALL_DIR}
/usr/bin/signal-cli

%changelog
* $(date "+%a %b %d %Y") packager <packager@localhost> - ${VERSION}-${RELEASE}
- Bundle signal-cli with libsignal-client
EOF

# ========================
# Build
# ========================
rpmbuild -ba "$SPEC"

echo "[OK] Done"
find "${TOP}/RPMS" -name "signal-cli-${VERSION}-${RELEASE}*.rpm"
