#!/usr/bin/env node
/**
 * Pack one executable into a ZIP archive that is the same bytes every time.
 *
 * `pack-release-archive.sh` used 7-Zip, or Info-ZIP where 7-Zip was missing, and both write
 * more into the archive than the entry itself: an extended timestamp field carrying times the
 * script cannot pin, and a modification time read through the machine's own zone. Neither is
 * something a reader of a release archive wants, and both made two archives packed from one
 * binary differ. The ZIP truss publishes holds one file, so writing the container directly is
 * shorter than normalizing what a packer produced.
 *
 * usage: node scripts/pack-zip.mjs <source> <archive> <name-in-archive>
 */

import { readFileSync, writeFileSync } from "node:fs";

import { buildZip } from "./lib/archive.mjs";

const [source, archive, name] = process.argv.slice(2);

if (!source || !archive || !name) {
  throw new Error("usage: node scripts/pack-zip.mjs <source> <archive> <name-in-archive>");
}

writeFileSync(
  archive,
  buildZip({ name, data: readFileSync(source), mode: 0o755 }),
);
