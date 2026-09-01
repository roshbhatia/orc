import { Effect } from "effect";
import packageJson from "../package.json" with { type: "json" };
import { run } from "./run.ts";

const version = packageJson.version;

const streams = {
  stderr: (value: string) => console.error(value),
  stdout: (value: string) => console.log(value),
};

Effect.runPromise(run(Bun.argv.slice(2), streams, version)).then((code) => {
  process.exitCode = code;
});
