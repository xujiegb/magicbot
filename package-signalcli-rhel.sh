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
need zip

# NOTE: do NOT require java for packaging; keep it only as RPM runtime dependency.
# need java

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
# CHANGED: download Linux-native tarball
echo "[*] Downloading signal-cli Linux-native tarball"
curl -fL -o "${WORK}/${NATIVE_TAR}" "${BASE_URL}/${NATIVE_TAR}"

# ========================
# Extract upstream bundle
# ========================
TMPDIR="$(mktemp -d)"
tar -xf "${WORK}/${NATIVE_TAR}" -C "${TMPDIR}"

# Find extracted top dir (usually signal-cli or signal-cli-${VERSION})
SRC_DIR="$(find "${TMPDIR}" -maxdepth 2 -type d \( -name "signal-cli-${VERSION}*" -o -name "signal-cli" \) | head -n1)"
test -n "${SRC_DIR}"

# Ensure target dirs
mkdir -p "${WORK}/lib"

# CHANGED: DO NOT use upstream /opt/signal-cli ELF (may be wrong arch). Always use JVM launcher.
# We will package our own wrapper later.
# Still keep upstream bin script if present for reference (not installed into PATH).
if [[ -f "${SRC_DIR}/bin/signal-cli" ]]; then
  cp -a "${SRC_DIR}/bin/signal-cli" "${WORK}/upstream-signal-cli"
  chmod 0755 "${WORK}/upstream-signal-cli" || true
fi

# CHANGED: collect jar(s) into WORK/
# Linux-native usually contains lots of jars under lib/
find "${SRC_DIR}" -maxdepth 4 -type f -name "*.jar" -print0 \
  | while IFS= read -r -d '' f; do
      cp -a "$f" "${WORK}/"
    done

# Sanity: must have at least one jar
if ! ls -1 "${WORK}"/*.jar >/dev/null 2>&1; then
  echo "ERROR: no jars found in extracted ${NATIVE_TAR}" >&2
  find "${SRC_DIR}" -maxdepth 4 -type f | head -n 200 >&2 || true
  exit 1
fi

# ========================
# Detect required libsignal-client version from jar filename
# ========================
LIBJAR_PATH="$(ls -1 "${WORK}"/libsignal-client-*.jar 2>/dev/null | head -n1 || true)"
if [[ -z "${LIBJAR_PATH}" ]]; then
  echo "ERROR: cannot find libsignal-client-*.jar in extracted bundle" >&2
  ls -la "${WORK}" >&2 || true
  exit 1
fi
LIBJAR="$(basename "${LIBJAR_PATH}")"
LIBVER="$(echo "${LIBJAR}" | sed -E 's/^libsignal-client-([0-9.]+)\.jar$/\1/')"
if [[ -z "${LIBVER}" || "${LIBVER}" == "${LIBJAR}" ]]; then
  echo "ERROR: failed to parse libsignal-client version from ${LIBJAR}" >&2
  exit 1
fi
echo "[*] Detected libsignal-client version: ${LIBVER} (from ${LIBJAR})"

# ========================
# Replace JNI inside libsignal-client jar (x64 + arm64)
# ========================
# CHANGED: always replace JNI for both x86_64 and aarch64 using exquo/signal-libs-build
# This avoids:
# - Linux-native tarball being x86_64-only ELF
# - bundled amd64-only JNI inside jar
# - stale / wrong JNI versions
case "$ARCH" in
  x86_64)  EXQUO_ASSET="libsignal_jni.so-v${LIBVER}-x86_64-unknown-linux-gnu.tar.gz" ;;
  aarch64) EXQUO_ASSET="libsignal_jni.so-v${LIBVER}-aarch64-unknown-linux-gnu.tar.gz" ;;
  *) echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

EXQUO_URL="https://github.com/exquo/signal-libs-build/releases/download/libsignal_v${LIBVER}/${EXQUO_ASSET}"
echo "[*] Downloading prebuilt JNI from: ${EXQUO_URL}"
curl -fL -o "${WORK}/${EXQUO_ASSET}" "${EXQUO_URL}"

echo "[*] Extracting JNI tarball"
JNI_TMP="$(mktemp -d)"
tar -xf "${WORK}/${EXQUO_ASSET}" -C "${JNI_TMP}"

JNI_SO="$(find "${JNI_TMP}" -maxdepth 3 -type f -name "libsignal_jni.so" | head -n1 || true)"
if [[ -z "${JNI_SO}" ]]; then
  echo "ERROR: libsignal_jni.so not found after extracting ${EXQUO_ASSET}" >&2
  find "${JNI_TMP}" -maxdepth 3 -type f >&2 || true
  rm -rf "${JNI_TMP}"
  exit 1
fi

echo "[*] Replacing JNI inside ${LIBJAR}"
# Remove common old entries (Linux/Windows/macOS variants) if present; ignore errors.
zip -d "${LIBJAR_PATH}" \
  'libsignal_jni.so' \
  'libsignal_jni_amd64.so' \
  'libsignal_jni_aarch64.so' \
  'libsignal_jni_amd64.dylib' \
  'libsignal_jni_aarch64.dylib' \
  'signal_jni_amd64.dll' \
  'signal_jni_aarch64.dll' \
  '*signal_jni*' \
  2>/dev/null || true

# Add new one into jar root (no path)
cp -f "${JNI_SO}" "${WORK}/libsignal_jni.so"
zip -uj "${LIBJAR_PATH}" "${WORK}/libsignal_jni.so" >/dev/null

rm -rf "${JNI_TMP}"
rm -f "${WORK}/libsignal_jni.so"

# Verify jar contains the so
if ! jar tf "${LIBJAR_PATH}" | grep -qi 'libsignal_jni\.so'; then
  echo "ERROR: JNI injection failed; libsignal_jni.so not present in ${LIBJAR}" >&2
  exit 1
fi
echo "[OK] JNI injected into ${LIBJAR}"

# ========================
# Wrapper (always JVM)
# ========================
# CHANGED: run signal-cli main jar; Linux-native layout varies, so pick signal-cli-*.jar if present, else first jar that looks like signal-cli
JAR=""
if [[ -f "${WORK}/signal-cli-${VERSION}.jar" ]]; then
  JAR="signal-cli-${VERSION}.jar"
else
  CAND="$(ls -1 "${WORK}"/signal-cli-*.jar 2>/dev/null | head -n1 || true)"
  if [[ -n "${CAND}" ]]; then
    JAR="$(basename "${CAND}")"
  else
    # last resort: first jar (but this is unlikely for Linux-native)
    JAR="$(ls -1 "${WORK}"/*.jar | head -n1 | xargs -n1 basename)"
  fi
fi

if [[ -z "${JAR}" || ! -f "${WORK}/${JAR}" ]]; then
  echo "ERROR: cannot find signal-cli jar to launch" >&2
  ls -la "${WORK}" >&2 || true
  exit 1
fi
echo "[*] Using launcher jar: ${JAR}"

cat > "${WORK}/signal-cli" <<EOF
#!/usr/bin/env bash
set -euo pipefail
DIR="\$(cd "\$(dirname "\$0")" && pwd)"
# Keep for any extra native deps; JNI itself is inside libsignal-client jar after injection.
export LD_LIBRARY_PATH="\$DIR/lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
exec java -jar "\$DIR/${JAR}" "\$@"
EOF
chmod 0755 "${WORK}/signal-cli"

# ========================
# Optional: collect extra native libs from Linux-native (non-JNI)
# ========================
# CHANGED: keep any libsignal*.so* found in extracted tree (rarely needed after jar injection, but safe)
find "${SRC_DIR}" -maxdepth 4 -type f -name "libsignal*.so*" -exec cp -a {} "${WORK}/lib/" \; || true
if [[ -d "${SRC_DIR}/lib" ]]; then
  find "${SRC_DIR}/lib" -maxdepth 2 -type f -name "libsignal*.so*" -exec cp -a {} "${WORK}/lib/" \; || true
fi

rm -rf "${TMPDIR}"

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
Summary:        Signal CLI with bundled libsignal-client (JNI injected per-arch)

License:        GPL-3.0
URL:            https://github.com/AsamK/signal-cli
Source0:        %{name}-%{version}.tar.gz

BuildArch:      %{_arch}
# Runtime: Java only. (Build does not require Java.)
Requires:       java-17-openjdk-headless

%global debug_package %{nil}

%description
signal-cli packaged with libsignal-client.

This RPM injects a matching libsignal_jni.so (per-arch) into the bundled
libsignal-client-*.jar, avoiding wrong-arch Linux-native launchers and ensuring
aarch64 works out of the box.

%prep
%autosetup -n %{name}-%{version}

%build
# nothing

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}${INSTALL_DIR}
mkdir -p %{buildroot}%{_bindir}

# install wrapper + jars + libs into /opt
cp -a signal-cli %{buildroot}${INSTALL_DIR}/
cp -a *.jar %{buildroot}${INSTALL_DIR}/
if [ -d lib ]; then
  cp -a lib %{buildroot}${INSTALL_DIR}/
fi

# provide stable entrypoint
install -m 0755 -D ${INSTALL_DIR}/signal-cli %{buildroot}%{_bindir}/signal-cli

%files
${INSTALL_DIR}
%{_bindir}/signal-cli

%changelog
* $(date "+%a %b %d %Y") packager <packager@localhost> - ${VERSION}-${RELEASE}
- Bundle signal-cli Linux-native jars and inject per-arch libsignal_jni.so into libsignal-client jar
EOF

# ========================
# Build
# ========================
rpmbuild -ba "$SPEC"

echo "[OK] Done"
find "${TOP}/RPMS" -name "signal-cli-${VERSION}-${RELEASE}*.rpm"
