/**
 * Minimal readers for the two archive formats truss publishes.
 *
 * These parse the container metadata directly instead of shelling out to `tar`
 * / `unzip` / `7z`, because the release verification has to make identical
 * assertions on Linux, macOS and Windows runners, and the three platforms
 * disagree on both which tools exist and how they format their listings.
 * Reading the headers ourselves also lets us assert on fields a listing hides,
 * such as the ustar uid/gid and the ZIP external attributes.
 */

import { gunzipSync, inflateRawSync } from "node:zlib";

const TAR_BLOCK_SIZE = 512;

/**
 * @typedef {object} ArchiveEntry
 * @property {string} name entry path as stored in the archive
 * @property {"file" | "directory" | "other"} type
 * @property {number | null} mode unix permission bits, or null when the format did not record any
 * @property {number | null} uid
 * @property {number | null} gid
 * @property {string | null} uname
 * @property {string | null} gname
 * @property {number} size uncompressed size in bytes
 * @property {Buffer} data uncompressed contents
 */

/**
 * Read every entry of a gzip-compressed ustar archive.
 *
 * @param {Buffer} buffer raw `.tar.gz` bytes
 * @returns {ArchiveEntry[]}
 */
export function readTarGz(buffer) {
  const tar = gunzipSync(buffer);
  const entries = [];
  let offset = 0;

  while (offset + TAR_BLOCK_SIZE <= tar.length) {
    const header = tar.subarray(offset, offset + TAR_BLOCK_SIZE);
    if (header.every((byte) => byte === 0)) {
      break;
    }

    const name = readString(header, 0, 100);
    const mode = readOctal(header, 100, 8);
    const uid = readOctal(header, 108, 8);
    const gid = readOctal(header, 116, 8);
    const size = readOctal(header, 124, 12);
    const typeflag = String.fromCharCode(header[156]);
    const uname = readString(header, 265, 32);
    const gname = readString(header, 297, 32);
    const prefix = readString(header, 345, 155);

    offset += TAR_BLOCK_SIZE;
    const data = tar.subarray(offset, offset + size);
    offset += Math.ceil(size / TAR_BLOCK_SIZE) * TAR_BLOCK_SIZE;

    if (typeflag === "x" || typeflag === "g" || typeflag === "L" || typeflag === "K") {
      // pax/GNU extended headers describe the *next* entry. truss archives are
      // written as plain ustar so these should never appear; surface them
      // rather than silently skipping, because they carry the very metadata
      // (uid, mtime) this check exists to pin down.
      throw new Error(`unexpected extended tar header of type ${typeflag}`);
    }

    entries.push({
      name: prefix === "" ? name : `${prefix}/${name}`,
      type: typeflag === "5" ? "directory" : typeflag === "0" || typeflag === "\0" ? "file" : "other",
      mode,
      uid,
      gid,
      uname,
      gname,
      size,
      data: Buffer.from(data),
    });
  }

  return entries;
}

/**
 * Read every entry of a ZIP archive via its central directory.
 *
 * @param {Buffer} buffer raw `.zip` bytes
 * @returns {ArchiveEntry[]}
 */
export function readZip(buffer) {
  const eocd = findEndOfCentralDirectory(buffer);
  const count = buffer.readUInt16LE(eocd + 10);
  let offset = buffer.readUInt32LE(eocd + 16);

  if (offset === 0xffffffff) {
    throw new Error("zip64 archives are not supported by this reader");
  }

  const entries = [];
  for (let index = 0; index < count; index += 1) {
    if (buffer.readUInt32LE(offset) !== 0x02014b50) {
      throw new Error(`malformed zip central directory at offset ${offset}`);
    }

    const versionMadeBy = buffer.readUInt16LE(offset + 4);
    const method = buffer.readUInt16LE(offset + 10);
    const compressedSize = buffer.readUInt32LE(offset + 20);
    const size = buffer.readUInt32LE(offset + 24);
    const nameLength = buffer.readUInt16LE(offset + 28);
    const extraLength = buffer.readUInt16LE(offset + 30);
    const commentLength = buffer.readUInt16LE(offset + 32);
    const externalAttributes = buffer.readUInt32LE(offset + 38);
    const localHeaderOffset = buffer.readUInt32LE(offset + 42);
    const name = buffer.toString("utf8", offset + 46, offset + 46 + nameLength);

    // The high byte of "version made by" is the host system; 3 means Unix, and
    // only then do the top 16 bits of the external attributes hold st_mode.
    const madeByUnix = versionMadeBy >> 8 === 3;
    const mode = madeByUnix ? (externalAttributes >>> 16) & 0o7777 : null;

    entries.push({
      name,
      type: name.endsWith("/") ? "directory" : "file",
      mode,
      uid: null,
      gid: null,
      uname: null,
      gname: null,
      size,
      data: readZipData(buffer, localHeaderOffset, method, compressedSize, size),
    });

    offset += 46 + nameLength + extraLength + commentLength;
  }

  return entries;
}

/**
 * Read an archive by extension.
 *
 * @param {Buffer} buffer
 * @param {string} format one of `tar.gz`, `zip`
 * @returns {ArchiveEntry[]}
 */
export function readArchive(buffer, format) {
  switch (format) {
    case "tar.gz":
      return readTarGz(buffer);
    case "zip":
      return readZip(buffer);
    default:
      throw new Error(`unsupported archive format: ${format}`);
  }
}

/**
 * Infer the archive format from a file name.
 *
 * @param {string} fileName
 * @returns {"tar.gz" | "zip"}
 */
export function formatFromName(fileName) {
  if (fileName.endsWith(".tar.gz")) {
    return "tar.gz";
  }
  if (fileName.endsWith(".zip")) {
    return "zip";
  }
  throw new Error(`cannot infer archive format from ${fileName}`);
}

function readZipData(buffer, localHeaderOffset, method, compressedSize, size) {
  if (buffer.readUInt32LE(localHeaderOffset) !== 0x04034b50) {
    throw new Error(`malformed zip local header at offset ${localHeaderOffset}`);
  }

  const nameLength = buffer.readUInt16LE(localHeaderOffset + 26);
  const extraLength = buffer.readUInt16LE(localHeaderOffset + 28);
  const start = localHeaderOffset + 30 + nameLength + extraLength;
  const compressed = buffer.subarray(start, start + compressedSize);

  if (method === 0) {
    return Buffer.from(compressed);
  }
  if (method === 8) {
    const inflated = inflateRawSync(compressed);
    if (inflated.length !== size) {
      throw new Error("zip entry size does not match its deflated contents");
    }
    return inflated;
  }

  throw new Error(`unsupported zip compression method: ${method}`);
}

function findEndOfCentralDirectory(buffer) {
  // The EOCD record is at the very end unless a trailing comment pushes it up;
  // a comment is at most 0xffff bytes, so this bounded backwards scan is exact.
  const earliest = Math.max(0, buffer.length - 0xffff - 22);
  for (let offset = buffer.length - 22; offset >= earliest; offset -= 1) {
    if (buffer.readUInt32LE(offset) === 0x06054b50) {
      return offset;
    }
  }
  throw new Error("not a zip archive: end of central directory not found");
}

function readString(header, start, length) {
  const raw = header.subarray(start, start + length);
  const end = raw.indexOf(0);
  return raw.toString("utf8", 0, end === -1 ? raw.length : end);
}

function readOctal(header, start, length) {
  const text = readString(header, start, length).trim();
  return text === "" ? 0 : Number.parseInt(text, 8);
}
