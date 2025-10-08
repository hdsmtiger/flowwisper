#!npx tsx

import fs from "fs";
import process from "process";
import { Octokit } from "@octokit/rest";

const main = async () => {
  const targetFile = "../cmake/package-lock.cmake";

  const GITHUB_TOKEN = process.env.GITHUB_TOKEN || "";
  const octokit = new Octokit(GITHUB_TOKEN ? { auth: GITHUB_TOKEN } : {});

  const lines = fs
    .readFileSync(targetFile, {
      encoding: "utf8",
    })
    .trim()
    .split(/\r?\n/);

  const pattern =
    /^CPMDeclarePackage\(\s*(\S+)\s+NAME\s+(\S+)\s+GITHUB_REPOSITORY\s+(\S+)\s+GIT_TAG\s+(\S+)\s+DOWNLOAD_ONLY\s+(\S+)\s*\)$/;

  let newText = "";

  for (const l of lines) {
    const m = l.match(pattern);
    if (m) {
      let [, alias, name, repository, tag, downloadOnly] = m;
      const [owner, repo] = repository.split("/");

      console.log(`Updating ${repository}...`);

      if (tag.length === 40) {
        //If the tag is a Git object ID, retrieve the latest commit instead.
        const repoInfo = await octokit.rest.repos.get({ owner, repo });
        const { data } = await octokit.rest.repos.listCommits({
          owner,
          repo,
          sha: `heads/${repoInfo.data.default_branch}`,
          per_page: 1,
        });
        tag = data[0].sha;
      } else {
        const tags = await octokit.repos.listTags({ owner, repo, per_page: 1 });
        tag = tags.data[0].name;
      }

      console.log(`    New GIT_TAG: ${tag}`);

      newText +=
        `CPMDeclarePackage(${alias}` +
        `    NAME ${name}` +
        `    GITHUB_REPOSITORY ${repository}` +
        `    GIT_TAG ${tag}` +
        `    DOWNLOAD_ONLY ${downloadOnly}` +
        `)\n`;
    } else {
      newText += `${l}\n`;
    }
  }

  fs.writeFileSync(targetFile, newText);
};

main();
