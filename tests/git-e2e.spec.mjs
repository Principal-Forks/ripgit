import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import {
  addRemote,
  appendLineAndCommit,
  cleanupTempDirs,
  cloneFixture,
  git,
  gitStdout,
  makeTempDir,
  pushAsOwner,
} from "./helpers/git.mjs";
import { createTestServer, uniqueId } from "./helpers/mf.mjs";

let server;
const tempDirs = [];

function uniqueToken(prefix) {
  return `${prefix}${uniqueId("token")}`.replace(/[^a-zA-Z0-9]/g, "");
}

async function cloneRemote(remoteUrl) {
  const workDir = await makeTempDir("ripgit-clone");
  const repoDir = join(workDir, "repo");
  tempDirs.push(workDir);
  await git(undefined, ["clone", remoteUrl, repoDir]);
  return { workDir, repoDir };
}

beforeAll(async () => {
  server = await createTestServer();
});

afterAll(async () => {
  await cleanupTempDirs(tempDirs);
  await server.mf.dispose();
});

describe("git CLI e2e", () => {
  test("pushes a real-world fixture, clones it, and force-pushes rewritten history", async () => {
    const owner = uniqueId("owner");
    const repo = uniqueId("repo");
    const remoteUrl = new URL(`/${owner}/${repo}`, server.url).toString();

    const source = await cloneFixture();
    tempDirs.push(source.workDir);
    await addRemote(source.repoDir, "ripgit", remoteUrl);

    const initialHead = await gitStdout(source.repoDir, ["rev-parse", "HEAD"]);
    await pushAsOwner(source.repoDir, owner, "push", "ripgit", "HEAD:refs/heads/main");

    const firstClone = await cloneRemote(remoteUrl);
    expect(await gitStdout(firstClone.repoDir, ["rev-parse", "HEAD"])).toBe(initialHead);

    const fastForwardToken = uniqueToken("fastforward");
    await appendLineAndCommit(
      source.repoDir,
      "README.md",
      `ripgit e2e token ${fastForwardToken}`,
      "e2e fast-forward change",
    );
    await pushAsOwner(source.repoDir, owner, "push", "ripgit", "HEAD:refs/heads/main");

    let response = await server.dispatch(`/${owner}/${repo}/search?q=${fastForwardToken}&scope=code`);
    expect(response.status).toBe(200);
    let payload = await response.json();
    expect(payload.total_matches).toBeGreaterThan(0);

    const rewrittenSource = await cloneFixture();
    tempDirs.push(rewrittenSource.workDir);
    await addRemote(rewrittenSource.repoDir, "ripgit", remoteUrl);

    const forcePushToken = uniqueToken("forcepush");
    const forceHead = await appendLineAndCommit(
      rewrittenSource.repoDir,
      "README.md",
      `ripgit e2e token ${forcePushToken}`,
      "e2e rewritten change",
    );
    await pushAsOwner(
      rewrittenSource.repoDir,
      owner,
      "push",
      "--force",
      "ripgit",
      "HEAD:refs/heads/main",
    );

    const secondClone = await cloneRemote(remoteUrl);
    expect(await gitStdout(secondClone.repoDir, ["rev-parse", "HEAD"])).toBe(forceHead);

    const readme = await readFile(join(secondClone.repoDir, "README.md"), "utf8");
    expect(readme).toContain(forcePushToken);
    expect(readme).not.toContain(fastForwardToken);

    response = await server.dispatch(`/${owner}/${repo}/search?q=${forcePushToken}&scope=code`);
    expect(response.status).toBe(200);
    payload = await response.json();
    expect(payload.total_matches).toBeGreaterThan(0);

    response = await server.dispatch(`/${owner}/${repo}/search?q=${fastForwardToken}&scope=code`);
    expect(response.status).toBe(200);
    payload = await response.json();
    expect(payload.total_matches).toBe(0);

    await git(firstClone.repoDir, ["fetch", "origin"]);
    expect(await gitStdout(firstClone.repoDir, ["rev-parse", "origin/main"])).toBe(forceHead);
  });
});
