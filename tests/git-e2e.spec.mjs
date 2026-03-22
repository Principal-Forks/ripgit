import { Buffer } from "node:buffer";
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
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

function uniqueToken(prefix) {
  return `${prefix}${uniqueId("token")}`.replace(/[^a-zA-Z0-9]/g, "");
}

function pktLine(text) {
  const payload = textEncoder.encode(text);
  return Buffer.concat([
    Buffer.from((payload.length + 4).toString(16).padStart(4, "0")),
    Buffer.from(payload),
  ]);
}

function buildUploadPackRequest(want, capabilities) {
  return Buffer.concat([
    pktLine(`want ${want} ${capabilities.join(" ")}\n`),
    Buffer.from("0000"),
    pktLine("done\n"),
  ]);
}

function buildReceivePackRequest(oldHash, newHash, refName, capabilities) {
  return Buffer.concat([
    pktLine(`${oldHash} ${newHash} ${refName}\0${capabilities.join(" ")}\n`),
    Buffer.from("0000"),
  ]);
}

function readPktLine(bytes, offset) {
  const length = Number.parseInt(textDecoder.decode(bytes.slice(offset, offset + 4)), 16);
  if (length === 0) {
    return { payload: null, nextOffset: offset + 4 };
  }

  return {
    payload: bytes.slice(offset + 4, offset + length),
    nextOffset: offset + length,
  };
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

  test("includes upload-pack sideband progress and honors no-progress", async () => {
    const owner = uniqueId("owner");
    const repo = uniqueId("repo");
    const remotePath = `/${owner}/${repo}`;
    const remoteUrl = new URL(remotePath, server.url).toString();

    const source = await cloneFixture();
    tempDirs.push(source.workDir);
    await addRemote(source.repoDir, "ripgit", remoteUrl);
    await pushAsOwner(source.repoDir, owner, "push", "ripgit", "HEAD:refs/heads/main");

    const head = await gitStdout(source.repoDir, ["rev-parse", "HEAD"]);

    let response = await server.dispatch(`${remotePath}/git-upload-pack`, {
      method: "POST",
      headers: {
        "Content-Type": "application/x-git-upload-pack-request",
      },
      body: buildUploadPackRequest(head, ["multi_ack_detailed", "side-band-64k", "ofs-delta"]),
    });
    expect(response.status).toBe(200);
    let bytes = new Uint8Array(await response.arrayBuffer());

    let first = readPktLine(bytes, 0);
    expect(textDecoder.decode(first.payload)).toBe("NAK\n");

    let second = readPktLine(bytes, first.nextOffset);
    expect(second.payload[0]).toBe(2);
    expect(textDecoder.decode(second.payload.slice(1))).toContain("Enumerating objects:");

    let sawPackData = false;
    let offset = second.nextOffset;
    while (offset < bytes.length) {
      const pkt = readPktLine(bytes, offset);
      offset = pkt.nextOffset;
      if (!pkt.payload) {
        break;
      }
      if (pkt.payload[0] === 1) {
        sawPackData = true;
        expect(textDecoder.decode(pkt.payload.slice(1, 5))).toBe("PACK");
        break;
      }
    }
    expect(sawPackData).toBe(true);

    response = await server.dispatch(`${remotePath}/git-upload-pack`, {
      method: "POST",
      headers: {
        "Content-Type": "application/x-git-upload-pack-request",
      },
      body: buildUploadPackRequest(head, [
        "multi_ack_detailed",
        "side-band-64k",
        "no-progress",
        "ofs-delta",
      ]),
    });
    expect(response.status).toBe(200);
    bytes = new Uint8Array(await response.arrayBuffer());

    first = readPktLine(bytes, 0);
    expect(textDecoder.decode(first.payload)).toBe("NAK\n");

    second = readPktLine(bytes, first.nextOffset);
    expect(second.payload[0]).toBe(1);
    expect(textDecoder.decode(second.payload.slice(1, 5))).toBe("PACK");
  });

  test("returns channel 3 fatal sideband messages for protocol errors", async () => {
    const owner = uniqueId("owner");
    const repo = uniqueId("repo");
    const remotePath = `/${owner}/${repo}`;
    const remoteUrl = new URL(remotePath, server.url).toString();

    const source = await cloneFixture();
    tempDirs.push(source.workDir);
    await addRemote(source.repoDir, "ripgit", remoteUrl);
    await pushAsOwner(source.repoDir, owner, "push", "ripgit", "HEAD:refs/heads/main");

    const head = await gitStdout(source.repoDir, ["rev-parse", "HEAD"]);

    let response = await server.dispatch(`${remotePath}/git-upload-pack`, {
      method: "POST",
      headers: {
        "Content-Type": "application/x-git-upload-pack-request",
      },
      body: buildUploadPackRequest(head, [
        "multi_ack_detailed",
        "side-band",
        "side-band-64k",
        "ofs-delta",
      ]),
    });
    expect(response.status).toBe(200);
    let bytes = new Uint8Array(await response.arrayBuffer());
    let first = readPktLine(bytes, 0);
    expect(first.payload[0]).toBe(3);
    expect(textDecoder.decode(first.payload.slice(1))).toContain(
      "fatal: upload-pack capabilities: client requested both side-band and side-band-64k",
    );

    response = await server.dispatch(`${remotePath}/git-receive-pack`, {
      method: "POST",
      headers: {
        ...actorHeaders(owner),
        "Content-Type": "application/x-git-receive-pack-request",
      },
      body: buildReceivePackRequest(
        "0000000000000000000000000000000000000000",
        head,
        "refs/heads/main",
        ["report-status", "side-band", "side-band-64k"],
      ),
    });
    expect(response.status).toBe(200);
    bytes = new Uint8Array(await response.arrayBuffer());
    first = readPktLine(bytes, 0);
    expect(first.payload[0]).toBe(3);
    expect(textDecoder.decode(first.payload.slice(1))).toContain(
      "fatal: receive-pack capabilities: client requested both side-band and side-band-64k",
    );
  });

  test("rejects oversize pushes as git protocol status, not handler failure", async () => {
    const owner = uniqueId("owner");
    const repo = uniqueId("repo");
    const remotePath = `/${owner}/${repo}`;
    const requestBody = Buffer.concat([
      buildReceivePackRequest(
        "0000000000000000000000000000000000000000",
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        ["report-status", "side-band-64k", "quiet"],
      ),
      Buffer.allocUnsafe(50_000_001),
    ]);

    const response = await server.dispatch(`${remotePath}/git-receive-pack`, {
      method: "POST",
      headers: {
        ...actorHeaders(owner),
        "Content-Type": "application/x-git-receive-pack-request",
      },
      body: requestBody,
    });

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("application/x-git-receive-pack-result");

    const bytes = new Uint8Array(await response.arrayBuffer());
    const first = readPktLine(bytes, 0);
    expect(first.payload[0]).toBe(1);

    const status = textDecoder.decode(first.payload.slice(1));
    expect(status).toContain("unpack pack too large");
    expect(status).toContain("ng refs/heads/main pack too large");

    const flush = readPktLine(bytes, first.nextOffset);
    expect(flush.payload).toBeNull();
  });

  test("includes receive-pack progress and honors quiet", async () => {
    const owner = uniqueId("owner");
    const loudRepo = uniqueId("repo");
    const quietRepo = uniqueId("repo");
    const loudUrl = new URL(`/${owner}/${loudRepo}`, server.url).toString();
    const quietUrl = new URL(`/${owner}/${quietRepo}`, server.url).toString();

    const loudSource = await cloneFixture();
    tempDirs.push(loudSource.workDir);
    await addRemote(loudSource.repoDir, "ripgit", loudUrl);
    const loudPush = await pushAsOwner(
      loudSource.repoDir,
      owner,
      "push",
      "--progress",
      "ripgit",
      "HEAD:refs/heads/main",
    );
    expect(loudPush.stderr).toContain("remote: Processing 1 ref update(s).");
    expect(loudPush.stderr).toContain("remote: Received pack:");
    expect(loudPush.stderr).toContain("remote: Updated refs: 1 succeeded, 0 failed.");

    const quietSource = await cloneFixture();
    tempDirs.push(quietSource.workDir);
    await addRemote(quietSource.repoDir, "ripgit", quietUrl);
    const quietPush = await pushAsOwner(
      quietSource.repoDir,
      owner,
      "push",
      "--quiet",
      "ripgit",
      "HEAD:refs/heads/main",
    );
    expect(quietPush.stderr).not.toContain("remote: Processing");
    expect(quietPush.stderr).not.toContain("remote: Received pack:");
    expect(quietPush.stderr).toBe("");
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
