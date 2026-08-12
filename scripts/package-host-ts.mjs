import { mkdir, readFile, rm } from "node:fs/promises";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageDir = join(root, "packages", "host-ts");
const outputDir = process.env.KRW_AGENT_HOST_PACKAGE_OUTPUT
  ? resolve(process.env.KRW_AGENT_HOST_PACKAGE_OUTPUT)
  : join(root, "dist", "host-packages");
const allowDirty = process.env.KRW_AGENT_HOST_ALLOW_DIRTY === "1";

const packageJson = JSON.parse(await readFile(join(packageDir, "package.json"), "utf8"));
if (packageJson.name !== "@krw-agent/host" || typeof packageJson.version !== "string") {
  throw new Error("invalid_host_package_identity");
}

const gitCommit = (await run("git", ["rev-parse", "HEAD"], root)).stdout.trim();
const gitTree = (await run("git", ["rev-parse", "HEAD^{tree}"], root)).stdout.trim();
const gitStatus = (await run("git", ["status", "--porcelain=v1", "--untracked-files=normal"], root)).stdout;
if (!/^[0-9a-f]{40}$|^[0-9a-f]{64}$/.test(gitCommit) || !/^[0-9a-f]{40}$|^[0-9a-f]{64}$/.test(gitTree)) {
  throw new Error("invalid_agent_git_identity");
}
if (gitStatus && !allowDirty) {
  throw new Error("host_package_requires_clean_agent_tree");
}

await run("npm", ["run", "build"], packageDir);
await rm(outputDir, { recursive: true, force: true });
await mkdir(outputDir, { recursive: true });
const packed = await run(
  "npm",
  ["pack", "--pack-destination", outputDir, "--ignore-scripts"],
  packageDir,
);
const archiveName = packed.stdout.trim().split("\n").at(-1);
const expectedArchive = `${packageJson.name.slice(1).replaceAll("/", "-")}-${packageJson.version}.tgz`;
if (!archiveName || archiveName !== expectedArchive || !/^krw-agent-host-[0-9A-Za-z._-]+\.tgz$/.test(archiveName)) {
  throw new Error("unexpected_host_package_archive");
}
const archivePath = join(outputDir, archiveName);
const archiveBytes = await readFile(archivePath);
const digest = createHash("sha256").update(archiveBytes).digest("hex");
console.log(JSON.stringify({
  package: packageJson.name,
  version: packageJson.version,
  agent_git_commit: gitCommit,
  agent_git_tree: gitTree,
  dirty_source: Boolean(gitStatus),
  archive: archiveName,
  archive_path: archivePath,
  sha256: `sha256:${digest}`,
}));

function run(command, args, cwd) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, { cwd, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) return resolvePromise({ stdout, stderr });
      reject(new Error(`${command} ${args.join(" ")} failed with ${code}: ${stderr}`));
    });
  });
}
