const escapeSequenceEnd = (
  input: string,
  start: number,
): number | undefined => {
  const introducer = input[start + 1];
  if (introducer === undefined) return undefined;
  if (introducer === "[") {
    for (let index = start + 2; index < input.length; index++) {
      const code = input.charCodeAt(index);
      if (code >= 0x40 && code <= 0x7e) return index + 1;
    }
    return undefined;
  }
  if (["]", "P", "X", "^", "_"].includes(introducer)) {
    for (let index = start + 2; index < input.length; index++) {
      if (introducer === "]" && input.charCodeAt(index) === 0x07)
        return index + 1;
      if (input[index] === "\u001b" && input[index + 1] === "\\")
        return index + 2;
    }
    return undefined;
  }
  return start + 2;
};

const isTerminalQuery = (sequence: string): boolean => {
  if (
    sequence.startsWith("\u001b]10;?") ||
    sequence.startsWith("\u001b]11;?") ||
    sequence.startsWith("\u001b]99;") ||
    sequence.startsWith("\u001b]1337;Capabilities") ||
    sequence.startsWith("\u001b]66;") ||
    sequence.startsWith("\u001b_Gi=31337,")
  )
    return true;
  if (
    sequence.startsWith("\u001b[?") &&
    sequence.endsWith("$p") &&
    /^[0-9;]*$/.test(sequence.slice(3, -2))
  )
    return true;
  return [
    "\u001b[>0q",
    "\u001bP+q4d73\u001b\\",
    "\u001bP+q4D73\u001b\\",
    "\u001b[?u",
    "\u001b[c",
    "\u001b[6n",
    "\u001b[14t",
    "\u001b[?2031h",
  ].includes(sequence);
};

export class TerminalQueryFilter {
  private pending = "";

  write(chunk: string | Uint8Array): string {
    const input = this.pending + Buffer.from(chunk).toString("utf8");
    this.pending = "";
    let output = "";
    for (let index = 0; index < input.length; ) {
      if (input[index] !== "\u001b") {
        output += input[index];
        index += 1;
        continue;
      }
      const end = escapeSequenceEnd(input, index);
      if (end === undefined) {
        this.pending = input.slice(index);
        break;
      }
      const sequence = input.slice(index, end);
      if (!isTerminalQuery(sequence)) output += sequence;
      index = end;
    }
    return output;
  }
}

export const queryFilteredStdout = (
  target: NodeJS.WriteStream = process.stdout,
): NodeJS.WriteStream => {
  const filter = new TerminalQueryFilter();
  const filteredWrite = ((
    chunk: string | Uint8Array,
    ...arguments_: ReadonlyArray<unknown>
  ): boolean => {
    const output = filter.write(chunk);
    if (output.length > 0)
      return Reflect.apply(target.write, target, [output, ...arguments_]);
    const callback = arguments_.findLast(
      (argument) => typeof argument === "function",
    );
    if (typeof callback === "function") queueMicrotask(() => callback());
    return true;
  }) as typeof target.write;
  return {
    columns: target.columns,
    isTTY: target.isTTY,
    rows: target.rows,
    write: filteredWrite,
  } as NodeJS.WriteStream;
};
