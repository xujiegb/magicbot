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

# CHANGED: use upstream Linux-native tarball (includes matching libsignal-client)
NATIVE_TAR="signal-cli-${VERSION}-Linux-native.tar.gz"

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
# CHANGED: download Linux-native tarball instead of jar + libsignal-client tar
echo "[*] Downloading signal-cli Linux-native tarball"
curl -fL -o "${WORK}/${NATIVE_TAR}" "${BASE_URL}/${NATIVE_TAR}"

# ========================
# Extract upstream bundle
# ========================
# CHANGED: extract, then normalize into our WORK layout: signal-cli, jar(s), lib/
TMPDIR="$(mktemp -d)"
tar -xf "${WORK}/${NATIVE_TAR}" -C "${TMPDIR}"

# Find extracted top dir (usually signal-cli-${VERSION})
SRC_DIR="$(find "${TMPDIR}" -maxdepth 2 -type d -name "signal-cli-${VERSION}*" | head -n1)"
test -n "${SRC_DIR}"

# Ensure target dirs
mkdir -p "${WORK}/lib"

# CHANGED: pick wrapper/binary
# Prefer the upstream executable named "signal-cli" if present.
if [[ -f "${SRC_DIR}/bin/signal-cli" ]]; then
  cp -a "${SRC_DIR}/bin/signal-cli" "${WORK}/signal-cli"
elif [[ -f "${SRC_DIR}/signal-cli" ]]; then
  cp -a "${SRC_DIR}/signal-cli" "${WORK}/signal-cli"
elif [[ -f "${SRC_DIR}/bin/signal-cli.sh" ]]; then
  cp -a "${SRC_DIR}/bin/signal-cli.sh" "${WORK}/signal-cli"
else
  echo "ERROR: cannot find upstream signal-cli launcher in ${SRC_DIR}" >&2
  exit 1
fi
chmod 0755 "${WORK}/signal-cli"

# CHANGED: collect jar(s) (we keep original filenames)
# Common locations: lib/, share/, or root.
find "${SRC_DIR}" -maxdepth 3 -type f \( -name "signal-cli*.jar" -o -name "*.jar" \) -print0 \
  | while IFS= read -r -d '' f; do
      cp -a "$f" "${WORK}/"
    done

# CHANGED: collect native libs into WORK/lib
# Move any libsignal*.so* found in extracted tree.
find "${SRC_DIR}" -maxdepth 4 -type f -name "libsignal*.so*" -exec cp -a {} "${WORK}/lib/" \; || true

# Fallback: some releases might place libs under "lib/"
if [[ -d "${SRC_DIR}/lib" ]]; then
  find "${SRC_DIR}/lib" -maxdepth 2 -type f -name "libsignal*.so*" -exec cp -a {} "${WORK}/lib/" \; || true
fi

rm -rf "${TMPDIR}"

# ========================
# Wrapper
# ========================
# CHANGED: generate our own wrapper that runs the jar we just copied (signal-cli-${VERSION}.jar if present)
JAR=""
if [[ -f "${WORK}/signal-cli-${VERSION}.jar" ]]; then
  JAR="signal-cli-${VERSION}.jar"
else
  # pick first jar that looks like signal-cli
  JAR="$(ls -1 "${WORK}"/signal-cli*.jar 2>/dev/null | head -n1 || true)"
  JAR="${JAR##*/}"
fi

if [[ -z "${JAR}" || ! -f "${WORK}/${JAR}" ]]; then
  echo "ERROR: cannot find signal-cli jar after extracting ${NATIVE_TAR}" >&2
  ls -la "${WORK}" >&2 || true
  exit 1
fi

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
