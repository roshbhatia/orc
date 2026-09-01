import {
  RGBA,
  StyledText,
  TextAttributes,
  type TextChunk,
} from "@opentui/core";

interface AnsiState {
  attributes: number;
  background: RGBA | undefined;
  foreground: RGBA | undefined;
}

const colorIndex = (code: number, background: boolean): number | undefined => {
  const base = background ? 40 : 30;
  const bright = background ? 100 : 90;
  if (code >= base && code <= base + 7) return code - base;
  if (code >= bright && code <= bright + 7) return code - bright + 8;
  return undefined;
};

const setAttribute = (
  attributes: number,
  attribute: number,
  enabled: boolean,
): number => (enabled ? attributes | attribute : attributes & ~attribute);

const applySgr = (state: AnsiState, values: ReadonlyArray<number>): void => {
  const codes = values.length === 0 ? [0] : values;
  for (let index = 0; index < codes.length; index++) {
    const code = codes[index] ?? 0;
    if (code === 0) {
      state.attributes = TextAttributes.NONE;
      state.background = undefined;
      state.foreground = undefined;
      continue;
    }
    if (code === 1)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.BOLD,
        true,
      );
    else if (code === 2)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.DIM,
        true,
      );
    else if (code === 4)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.UNDERLINE,
        true,
      );
    else if (code === 22) {
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.BOLD,
        false,
      );
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.DIM,
        false,
      );
    } else if (code === 24)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.UNDERLINE,
        false,
      );
    else if (code === 39) state.foreground = undefined;
    else if (code === 49) state.background = undefined;
    else if (code === 38 || code === 48) {
      const target = code === 38 ? "foreground" : "background";
      const mode = codes[index + 1];
      if (mode === 5 && codes[index + 2] !== undefined) {
        state[target] = RGBA.fromIndex(codes[index + 2] ?? 0);
        index += 2;
      } else if (
        mode === 2 &&
        codes[index + 2] !== undefined &&
        codes[index + 3] !== undefined &&
        codes[index + 4] !== undefined
      ) {
        state[target] = RGBA.fromInts(
          codes[index + 2] ?? 0,
          codes[index + 3] ?? 0,
          codes[index + 4] ?? 0,
        );
        index += 4;
      }
    } else {
      const foreground = colorIndex(code, false);
      const background = colorIndex(code, true);
      if (foreground !== undefined)
        state.foreground = RGBA.fromIndex(foreground);
      if (background !== undefined)
        state.background = RGBA.fromIndex(background);
    }
  }
};

export const ansiToStyledText = (input: string): StyledText => {
  const chunks: TextChunk[] = [];
  const state: AnsiState = {
    attributes: TextAttributes.NONE,
    background: undefined,
    foreground: undefined,
  };
  const escapeCharacter = String.fromCharCode(27);
  const expression = new RegExp(`${escapeCharacter}\\[([0-9;]*)m`, "g");
  let start = 0;
  for (const match of input.matchAll(expression)) {
    const index = match.index;
    if (index > start) {
      chunks.push({
        __isChunk: true,
        attributes: state.attributes,
        ...(state.background ? { bg: state.background } : {}),
        ...(state.foreground ? { fg: state.foreground } : {}),
        text: input.slice(start, index),
      });
    }
    applySgr(
      state,
      (match[1] ?? "")
        .split(";")
        .filter((value) => value.length > 0)
        .map(Number),
    );
    start = index + match[0].length;
  }
  if (start < input.length || chunks.length === 0) {
    chunks.push({
      __isChunk: true,
      attributes: state.attributes,
      ...(state.background ? { bg: state.background } : {}),
      ...(state.foreground ? { fg: state.foreground } : {}),
      text: input.slice(start),
    });
  }
  return new StyledText(chunks);
};
