/**
 * code-graph plugin for OpenCode.ai
 *
 * Registers the code-graph MCP server (a stdio binary) and the skills directory
 * shipped with the package. OpenCode discovers the plugin via the `plugin`
 * array in `opencode.json`; the function exported here is
 * called once at startup with the live client + directory and returns a config
 * hook that mutates OpenCode's resolved config in place.
 *
 * Exported as both the default and a named export so OpenCode picks it up
 * regardless of which convention its plugin loader resolves first.
 */

import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Published package layout is flat (skills/ and commands/ sit next to this file);
// running straight from a repo checkout they live under ../plugin/. Prefer the
// bundled copies so an installed package never reaches outside itself.
function resolveDir(name) {
  const bundled = path.join(__dirname, name);
  if (fs.existsSync(bundled)) return bundled;
  const repo = path.join(__dirname, "..", "plugin", name);
  return fs.existsSync(repo) ? repo : null;
}

const skillsDir = resolveDir("skills");

// The server is a plain stdio binary. Honour an explicit override, otherwise
// expect `code-graph-mcp` on PATH.
const serverCommand = process.env.CODE_GRAPH_MCP_BIN || "code-graph-mcp";

const hooks = ({ client, directory } = {}) => {
  const root = directory || process.cwd();

  const surface = async (message) => {
    try {
      if (client?.tui?.showToast) {
        await client.tui.showToast({ body: { message, variant: "info" } });
        return;
      }
    } catch {}
    // eslint-disable-next-line no-console
    console.error(`[code-graph] ${message}`);
  };

  return {
    config: async (config) => {
      if (skillsDir) {
        config.skills = config.skills || {};
        config.skills.paths = config.skills.paths || [];
        if (!config.skills.paths.includes(skillsDir)) {
          config.skills.paths.push(skillsDir);
        }
      }

      config.mcp = config.mcp || {};
      if (!config.mcp["code-graph"]) {
        config.mcp["code-graph"] = {
          type: "local",
          command: [serverCommand],
          enabled: true,
        };
      }
    },

    event: async ({ event } = {}) => {
      if (event?.type !== "session.created") {
        return;
      }
      // One-shot orientation: tell the agent whether this repo already has a
      // graph on disk, so it reaches for analyze_codebase before a query that
      // would otherwise come back empty for the wrong-looking reason.
      const cache = path.join(root, ".code-graph-cache.db");
      if (fs.existsSync(cache)) {
        return;
      }
      await surface(
        "code-graph: no index found for this repo. Run analyze_codebase (or analyze_codebase_async on a large tree) before asking structural questions.",
      );
    },
  };
};

export const CodeGraphPlugin = async (input) => hooks(input);
export default async (input) => hooks(input);
