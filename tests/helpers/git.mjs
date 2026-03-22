import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const fixturesDir = resolve(rootDir, "tests", "fixtures");

export const workersRsBundle = resolve(fixturesDir, "workers-rs-main.bundle");

function run(command, args, options = {}) {
  const { cwd, env, input } = options;

  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, args, {
      cwd,
      env: {
        ...process.env,
        GIT_TERMINAL_PROMPT: "0",
        ...env,
      },
      stdio: "pipe",
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", rejectPromise);
    child.on("close", (code) => {
      if (code === 0) {
        resolvePromise({ stdout, stderr });
        return;
      }

      rejectPromise(
        new Error(
          `${command} ${args.join(" ")} failed with exit code ${code}\nstdout:\n${stdout}\nstderr:\n${stderr}`,
        ),
      );
    });

    if (input !== undefined) {
      child.stdin.end(input);
    }
  });
}

export async function makeTempDir(prefix) {
  return mkdtemp(join(tmpdir(), `${prefix}-`));
}

export async function cleanupTempDirs(dirs) {
  for (const dir of dirs) {
    await rm(dir, { recursive: true, force: true });
  }
}

export async function git(cwd, args, options = {}) {
  return run("git", args, { cwd, ...options });
}

export async function gitStdout(cwd, args, options = {}) {
  const { stdout } = await git(cwd, args, options);
  return stdout.trim();
}

export async function cloneFixture(bundlePath = workersRsBundle) {
  const workDir = await makeTempDir("ripgit-fixture");
  const repoDir = join(workDir, "repo");

  await run("git", ["clone", bundlePath, repoDir]);
  await git(repoDir, ["config", "user.name", "Ripgit E2E"]);
  await git(repoDir, ["config", "user.email", "ripgit-e2e@example.com"]);

  return { workDir, repoDir };
}

export async function appendLineAndCommit(repoDir, relativePath, line, message) {
  const filePath = join(repoDir, relativePath);
  const existing = await readFile(filePath, "utf8");
  const next = existing.endsWith("\n") ? `${existing}${line}\n` : `${existing}\n${line}\n`;

  await writeFile(filePath, next);
  await git(repoDir, ["add", relativePath]);
  await git(repoDir, ["commit", "-m", message]);

  return gitStdout(repoDir, ["rev-parse", "HEAD"]);
}

export async function addRemote(repoDir, name, url) {
  await git(repoDir, ["remote", "add", name, url]);
}

export async function pushAsOwner(repoDir, owner, ...args) {
  return git(repoDir, ["-c", `http.extraHeader=X-Ripgit-Actor-Name: ${owner}`, ...args]);
}
