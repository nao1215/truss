#!/usr/bin/env bash
#
# Pack one release binary into a normalized distribution archive.
#
# The release workflow and the archive tests both go through this script so the
# two cannot drift. Everything the archive records about the *container* is
# pinned here:
#
#   * the archive holds exactly one entry, the executable, at the archive root
#     (no leading directory entry, no build-tree path);
#   * the entry name is `truss` (`truss.exe` on Windows);
#   * the mode is 0755, so a Unix extraction always yields an executable file;
#   * owner and group are uid 0 / gid 0 with empty user and group names, so a
#     GitHub Actions runner UID never leaks into the tarball and `tar x` as root
#     does not fail trying to chown to a user that does not exist;
#   * the entry mtime is a fixed 2000-01-01T00:00:00Z, and gzip is told not to
#     store its own timestamp or the input file name;
#   * a ZIP carries no extra fields at all, so no packer's idea of a creation or
#     access time and no local time zone rides along with it.
#
# The archives are therefore byte-for-byte reproducible for a given input
# binary. The binary itself is not: `cargo build --release` embeds absolute
# paths and codegen-unit ordering that vary per runner, and making Rust output
# reproducible is out of scope for the distribution layer. Pinning the container
# metadata is still worth it on its own -- it is what makes extraction behave
# identically everywhere, and it removes the runner identity from the artifact.
#
# usage: pack-release-archive.sh <binary-path> <archive-path>

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <binary-path> <archive-path>" >&2
  exit 2
fi

binary_path="$1"
archive_path="$2"

if [ ! -f "${binary_path}" ]; then
  echo "error: binary not found: ${binary_path}" >&2
  exit 1
fi

binary_name="$(basename "${binary_path}")"

case "${archive_path}" in
  *.tar.gz) format="tar.gz" ;;
  *.zip) format="zip" ;;
  *)
    echo "error: cannot infer archive format from ${archive_path}" >&2
    exit 2
    ;;
esac

# Resolve the output to an absolute path before any `cd`.
archive_dir="$(cd "$(dirname "${archive_path}")" && pwd)"
archive_abs="${archive_dir}/$(basename "${archive_path}")"
rm -f "${archive_abs}"

staging="$(mktemp -d)"
trap 'rm -rf "${staging}"' EXIT

cp "${binary_path}" "${staging}/${binary_name}"
chmod 0755 "${staging}/${binary_name}"
# `touch -t` reads local time, so pin the zone as well. 2000-01-01 is used
# rather than the Unix epoch because ZIP cannot represent timestamps before
# 1980.
TZ=UTC0 touch -t 200001010000 "${staging}/${binary_name}"

case "${format}" in
  tar.gz)
    tar_flags=(--format=ustar)
    if tar --version | head -1 | grep -q "GNU tar"; then
      tar_flags+=(--owner=0 --group=0 --numeric-owner --mtime=@946684800)
    else
      # bsdtar (macOS). It has no --mtime, which is why the staged file's mtime
      # is normalized above instead.
      tar_flags+=(--uid 0 --gid 0 --uname "" --gname "" --numeric-owner)
    fi

    tar "${tar_flags[@]}" -cf - -C "${staging}" "${binary_name}" \
      | gzip -9 -n > "${archive_abs}"
    ;;
  zip)
    # ZIP is written here rather than by 7-Zip or Info-ZIP. Both put more into the
    # archive than the entry: an extended timestamp field carrying times `touch`
    # cannot pin, which made two archives packed a second apart differ, and a
    # modification time read through the machine's own zone, which made the
    # packing machine part of the output. The archive holds one file, so writing
    # the container directly is shorter than normalizing what a packer produced,
    # and it is the same code on all three runners rather than whichever tool
    # each one happens to have.
    #
    # Node on Windows is a native binary that cannot read the POSIX paths Git
    # Bash hands it, so the three paths are translated where cygpath exists.
    zip_script="$(dirname "$0")/pack-zip.mjs"
    zip_source="${staging}/${binary_name}"
    zip_target="${archive_abs}"
    if command -v cygpath > /dev/null; then
      zip_script="$(cygpath -w "${zip_script}")"
      zip_source="$(cygpath -w "${zip_source}")"
      zip_target="$(cygpath -w "${zip_target}")"
    fi

    node "${zip_script}" "${zip_source}" "${zip_target}" "${binary_name}"
    ;;
esac

echo "${archive_abs}"
