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
import { actorHeaders, createTestServer, uniqueId } from "./helpers/mf.mjs";

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

  test("fetches new branch and tag refs into an existing clone", async () => {
    const owner = uniqueId("owner");
    const repo = uniqueId("repo");
    const featureBranch = uniqueId("feature");
    const tagName = `${uniqueId("tag")}-release`;
    const branchToken = uniqueToken("branchref");
    const remoteUrl = new URL(`/${owner}/${repo}`, server.url).toString();

    const source = await cloneFixture();
    tempDirs.push(source.workDir);
    await addRemote(source.repoDir, "ripgit", remoteUrl);
    await pushAsOwner(source.repoDir, owner, "push", "ripgit", "HEAD:refs/heads/main");

    const existingClone = await cloneRemote(remoteUrl);

    await git(source.repoDir, ["checkout", "-b", featureBranch]);
    const featureHead = await appendLineAndCommit(
      source.repoDir,
      "README.md",
      `ripgit e2e token ${branchToken}`,
      "e2e branch ref change",
    );
    await git(source.repoDir, ["tag", tagName]);
    const tagHead = await gitStdout(source.repoDir, ["rev-parse", tagName]);

    await pushAsOwner(
      source.repoDir,
      owner,
      "push",
      "ripgit",
      `HEAD:refs/heads/${featureBranch}`,
      `refs/tags/${tagName}:refs/tags/${tagName}`,
    );

    let response = await server.dispatch(`/${owner}/${repo}/refs`);
    expect(response.status).toBe(200);
    let refs = await response.json();
    expect(refs.heads.main).toBeTypeOf("string");
    expect(refs.heads[featureBranch]).toBe(featureHead);
    expect(refs.tags[tagName]).toBe(tagHead);

    await git(existingClone.repoDir, ["fetch", "origin", "--tags"]);
    expect(await gitStdout(existingClone.repoDir, ["rev-parse", `refs/remotes/origin/${featureBranch}`])).toBe(featureHead);
    expect(await gitStdout(existingClone.repoDir, ["rev-parse", `refs/tags/${tagName}`])).toBe(tagHead);
  });

  test("deletes branch and tag refs and prunes them from an existing clone", async () => {
    const owner = uniqueId("owner");
    const repo = uniqueId("repo");
    const featureBranch = uniqueId("feature");
    const tagName = `${uniqueId("tag")}-delete`;
    const remoteUrl = new URL(`/${owner}/${repo}`, server.url).toString();

    const source = await cloneFixture();
    tempDirs.push(source.workDir);
    await addRemote(source.repoDir, "ripgit", remoteUrl);
    await pushAsOwner(source.repoDir, owner, "push", "ripgit", "HEAD:refs/heads/main");

    await git(source.repoDir, ["checkout", "-b", featureBranch]);
    await appendLineAndCommit(
      source.repoDir,
      "README.md",
      `ripgit e2e token ${uniqueToken("deleteref")}`,
      "e2e delete ref change",
    );
    await git(source.repoDir, ["tag", tagName]);
    await pushAsOwner(
      source.repoDir,
      owner,
      "push",
      "ripgit",
      `HEAD:refs/heads/${featureBranch}`,
      `refs/tags/${tagName}:refs/tags/${tagName}`,
    );

    const existingClone = await cloneRemote(remoteUrl);
    await git(existingClone.repoDir, ["fetch", "origin", "--tags"]);
    expect(
      await gitStdout(existingClone.repoDir, ["rev-parse", `refs/remotes/origin/${featureBranch}`]),
    ).toHaveLength(40);
    expect(await gitStdout(existingClone.repoDir, ["rev-parse", `refs/tags/${tagName}`])).toHaveLength(40);

    await pushAsOwner(
      source.repoDir,
      owner,
      "push",
      "ripgit",
      `:refs/heads/${featureBranch}`,
      `:refs/tags/${tagName}`,
    );

    let response = await server.dispatch(`/${owner}/${repo}/refs`);
    expect(response.status).toBe(200);
    let refs = await response.json();
    expect(refs.heads[featureBranch]).toBeUndefined();
    expect(refs.tags[tagName]).toBeUndefined();

    await git(existingClone.repoDir, ["fetch", "origin", "--prune", "--prune-tags"]);
    await expect(
      gitStdout(existingClone.repoDir, ["rev-parse", `refs/remotes/origin/${featureBranch}`]),
    ).rejects.toThrow();
    await expect(gitStdout(existingClone.repoDir, ["rev-parse", `refs/tags/${tagName}`])).rejects.toThrow();
  });

  test("lists pushed repositories on the owner profile", async () => {
    const owner = uniqueId("owner");
    const repoOne = uniqueId("repo");
    const repoTwo = uniqueId("repo");
    const remoteOne = new URL(`/${owner}/${repoOne}`, server.url).toString();
    const remoteTwo = new URL(`/${owner}/${repoTwo}`, server.url).toString();

    let response = await server.dispatch(`/${owner}/?format=md`, {
      headers: actorHeaders(owner),
    });
    expect(response.status).toBe(200);
    let markdown = await response.text();
    expect(markdown).toContain("Repositories: `0`");
    expect(markdown).toContain("No repositories yet. Repositories are created on first push.");

    const source = await cloneFixture();
    tempDirs.push(source.workDir);
    await addRemote(source.repoDir, "ripgit-one", remoteOne);
    await addRemote(source.repoDir, "ripgit-two", remoteTwo);

    await pushAsOwner(source.repoDir, owner, "push", "ripgit-one", "HEAD:refs/heads/main");
    await pushAsOwner(source.repoDir, owner, "push", "ripgit-two", "HEAD:refs/heads/main");

    response = await server.dispatch(`/${owner}/?format=md`, {
      headers: actorHeaders(owner),
    });
    expect(response.status).toBe(200);
    markdown = await response.text();
    expect(markdown).toContain("Repositories: `2`");
    expect(markdown).toContain(`- \`${repoOne}\` - \`/${owner}/${repoOne}\``);
    expect(markdown).toContain(`- \`${repoTwo}\` - \`/${owner}/${repoTwo}\``);
    expect(markdown).toContain("## Push a New Repository");
  });
});
