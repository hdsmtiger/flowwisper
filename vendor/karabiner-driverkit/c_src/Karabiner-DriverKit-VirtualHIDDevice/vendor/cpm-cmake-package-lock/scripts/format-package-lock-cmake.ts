#!npx tsx

import fs from "fs";

const targetFile = "../cmake/package-lock.cmake";

const lines = fs
  .readFileSync(targetFile, {
    encoding: "utf8",
  })
  .trim()
  .split(/\r?\n/);

const rows = [
  {
    alias: "",
    name: "",
    repo: "",
    tag: "",
    downloadOnly: "",
  },
];
const pattern =
  /^CPMDeclarePackage\(\s*(\S+)\s+NAME\s+(\S+)\s+GITHUB_REPOSITORY\s+(\S+)\s+GIT_TAG\s+(\S+)\s+DOWNLOAD_ONLY\s+(\S+)\s*\)$/;

for (const l of lines) {
  const m = l.match(pattern);
  if (m) {
    const [, alias, name, repo, tag, downloadOnly] = m;
    rows.push({ alias, name, repo, tag, downloadOnly });
  }
}

const widths = {
  alias: Math.max(...rows.map((r) => r.alias.length)),
  name: Math.max(...rows.map((r) => r.name.length)),
  repo: Math.max(...rows.map((r) => r.repo.length)),
  tag: Math.max(...rows.map((r) => r.tag.length)),
  downloadOnly: Math.max(...rows.map((r) => r.downloadOnly.length)),
};

let formattedText = "";
for (const l of lines) {
  const m = l.match(pattern);
  if (m) {
    const [, alias, name, repo, tag, downloadOnly] = m;
    formattedText +=
      `CPMDeclarePackage(${alias.padEnd(widths.alias)}` +
      `    NAME ${name.padEnd(widths.name)}` +
      `    GITHUB_REPOSITORY ${repo.padEnd(widths.repo)}` +
      `    GIT_TAG ${tag.padEnd(widths.tag)}` +
      `    DOWNLOAD_ONLY ${downloadOnly}` +
      `)\n`;
  } else {
    formattedText += `${l}\n`;
  }
}

fs.writeFileSync(targetFile, formattedText);
