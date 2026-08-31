import { Effect } from "effect";
import { run } from "./run.ts";

const version = "0.1.0";

const streams = {
  stderr: (value: string) => console.error(value),
  stdout: (value: string) => console.log(value),
};

Effect.runPromise(run(Bun.argv.slice(2), streams, version)).then((code) => {
  process.exitCode = code;
});
