import { Data, Effect } from "effect";

export interface Options {
  readonly json: boolean;
  readonly name: string;
  readonly version: boolean;
}

export class ArgumentError extends Data.TaggedError("ArgumentError")<{
  readonly message: string;
}> {}

export const parseArgs = (
  args: ReadonlyArray<string>,
): Effect.Effect<Options, ArgumentError> =>
  Effect.gen(function* () {
    let json = false;
    let version = false;
    const positional: Array<string> = [];

    for (const arg of args) {
      switch (arg) {
        case "--json":
          json = true;
          break;
        case "--version":
          version = true;
          break;
        default:
          if (arg.startsWith("-")) {
            return yield* new ArgumentError({
              message: `unknown option: ${arg}`,
            });
          }
          positional.push(arg);
      }
    }

    if (positional.length > 1) {
      return yield* new ArgumentError({
        message: "example accepts at most one name",
      });
    }

    return {
      json,
      name: positional[0] ?? "world",
      version,
    };
  });
