import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { orcConfigSchema } from "./config.ts";
import { providerManifestSchema } from "./provider.ts";

const replaceSection = (
  source: string,
  name: string,
  content: string,
): string => {
  const begin = `<!-- BEGIN GENERATED:${name} -->`;
  const end = `<!-- END GENERATED:${name} -->`;
  const start = source.indexOf(begin);
  const finish = source.indexOf(end);
  if (start < 0 || finish < start)
    throw new Error(`README is missing generated section ${name}`);
  return `${source.slice(0, start + begin.length)}\n${content.trim()}\n${source.slice(finish)}`;
};

const writeOrCheck = async (
  path: string,
  content: string,
  check: boolean,
): Promise<void> => {
  if (check) {
    const current = await readFile(path, "utf8");
    if (current !== content)
      throw new Error(`${path} is stale; run bun run generate`);
    return;
  }
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, content);
};

const formatJson = async (path: string, content: string): Promise<string> => {
  const biome = Bun.which("biome");
  if (!biome) throw new Error("biome is required to generate Orc artifacts");
  const child = Bun.spawn([biome, "format", "--stdin-file-path", path], {
    stderr: "pipe",
    stdin: "pipe",
    stdout: "pipe",
  });
  child.stdin.write(content);
  child.stdin.end();
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  if (code !== 0) throw new Error(stderr.trim() || "biome format failed");
  return stdout;
};

export const generateArtifacts = async (
  root: string,
  commandReference: string,
  check: boolean,
): Promise<void> => {
  const readmePath = join(root, "README.md");
  const readme = replaceSection(
    await readFile(readmePath, "utf8"),
    "commands",
    commandReference,
  );
  await writeOrCheck(readmePath, readme, check);
  await writeOrCheck(
    join(root, "schema", "orc.schema.json"),
    await formatJson(
      join(root, "schema", "orc.schema.json"),
      `${JSON.stringify(orcConfigSchema(), null, 2)}\n`,
    ),
    check,
  );
  await writeOrCheck(
    join(root, "schema", "provider.schema.json"),
    await formatJson(
      join(root, "schema", "provider.schema.json"),
      `${JSON.stringify(providerManifestSchema(), null, 2)}\n`,
    ),
    check,
  );
};
