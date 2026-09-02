import { join } from "node:path";
import { generateArtifacts } from "../src/generate.ts";
import { commandReferenceMarkdown } from "../src/run.ts";

const root = join(import.meta.dir, "..");
await generateArtifacts(
  root,
  commandReferenceMarkdown(),
  process.argv.includes("--check"),
);
