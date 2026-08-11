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

await run("npm", ["run", "build"], packageDir);
await rm(outputDir, { recursive: true, force: true });
await mkdir(outputDir, { recursive: true });
const packed = await run("npm", ["pack", "--pack-destination", outputDir, "--ignore-scripts"], packageDir);
const archiveName = packed.stdout.trim().split("\n").at(-1);
if (!archiveName || !/^krw-agent-host-0\.1\.0\.tgz$/.test(archiveName)) {
  throw new Error("unexpected_host_package_archive");
}
const archivePath = join(outputDir, archiveName);
const digest = createHash("sha256").update(await readFile(archivePath)).digest("hex");
console.log(JSON.stringify({ archive: archivePath, sha256: `sha256:${digest}` }));

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
