// Where Letronna keeps its data, and how its terminal names a file, worked
// out the way Letronna itself does, so the two never disagree.

import * as os from "os";
import * as path from "path";

/** Letronna's data folder, as core's `chat::base_dir` finds it. */
export function dataFolder(
  env: NodeJS.ProcessEnv = process.env,
  platform: NodeJS.Platform = process.platform,
): string {
  if (env.AGENT_DATA_DIR) {
    return env.AGENT_DATA_DIR;
  }
  const under = platform === "darwin" ? "Library/Application Support" : ".local/share";
  const base = env.LOCALAPPDATA ?? path.join(env.HOME ?? os.homedir(), under);
  return path.join(base, "Letronna");
}

/**
 * The `@` reference the terminal takes for `file`, as core's
 * `references::reference` writes it: from the folder it is in, a folder
 * ending in a slash, spaces as `\ `, and the lines after `#L`.
 */
export function reference(
  file: string,
  folders: string[],
  lines?: [number, number],
  folder = false,
): string {
  const inside = folders
    .map((root) => path.relative(root, file))
    .find((rest) => rest !== "" && !rest.startsWith("..") && !path.isAbsolute(rest));
  let named = inside ?? file;
  if (folder && !/[\\/]$/.test(named)) {
    named += "/";
  }
  let out = "@" + named.replace(/ /g, "\\ ");
  if (lines) {
    out += lines[0] === lines[1] ? `#L${lines[0]}` : `#L${lines[0]}-${lines[1]}`;
  }
  return out;
}
