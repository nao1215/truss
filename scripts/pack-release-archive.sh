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
#     store its own timestamp or the input file name.
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
    # ZIP stores a local time with no zone, so the packing machine's own zone is
    # part of the archive unless it is pinned: the same binary packed in Tokyo
    # and in UTC differed by nine hours in the entry's timestamp and therefore
    # in its bytes. The release runs on runners that are all UTC, which is why
    # nothing noticed.
    #
    # 7-Zip on Windows is a native binary that cannot read the POSIX paths Git
    # Bash hands it, so translate the destination when cygpath is present.
    if command -v cygpath > /dev/null; then
      archive_arg="$(cygpath -w "${archive_abs}")"
    else
      archive_arg="${archive_abs}"
    fi

    if command -v 7z > /dev/null; then
      # Which tool packed the archive is printed so that a reproducibility failure
      # names its own branch: this arm has two, and the log said nothing about
      # which one ran. `-mtc=off` and `-mta=off`, which would keep the creation
      # and last-access times out of the entry, are rejected as E_INVALIDARG by
      # the 7-Zip on the macOS and Windows runners, so the entry carries whatever
      # those two are; only the modification time is pinned.
      echo "pack-release-archive: zip via 7z" >&2
      (cd "${staging}" && TZ=UTC0 7z a -tzip -mx=9 -bso0 -bsp0 "${archive_arg}" "${binary_name}" > /dev/null)
    elif command -v zip > /dev/null; then
      echo "pack-release-archive: zip via Info-ZIP" >&2
      # -X drops the extra attribute fields (uid/gid and high-resolution
      # timestamps) that would otherwise carry runner state into the archive.
      (cd "${staging}" && TZ=UTC0 zip -q -9 -X "${archive_arg}" "${binary_name}")
    else
      echo "error: neither 7z nor zip is available" >&2
      exit 1
    fi
    ;;
esac

echo "${archive_abs}"
