// Guard looks at every command the agent is about to run and blocks the
// ones that are hard to take back. Each rule has a switch on the Plugins
// page (or /plugins settings guard in the terminal).
const rules = [
  { setting: "force_push", what: "force push", match: /\bgit\s+push\b.*\s(--force(-with-lease)?|-f)\b/ },
  { setting: "reset_hard", what: "hard reset", match: /\bgit\s+reset\s+--hard\b/ },
  { setting: "delete_home", what: "delete of / or ~", match: /\brm\s+-\w*r\w*\s+(\/|~)\/?(\s|$)/ },
];

let looked = 0;
let blocked = 0;

letronna.on("tool.before", (call) => {
  if (call.tool !== "run_command") return;
  looked += 1;
  const command = String(call.args?.command ?? "");
  for (const rule of rules) {
    if (letronna.settings.get(rule.setting) && rule.match.test(command)) {
      blocked += 1;
      letronna.status(`guard · ${blocked} blocked`);
      letronna.toast(`Blocked a ${rule.what}`, "warning");
      return {
        block: `Guard does not let the agent run a ${rule.what}. If it is really wanted, ask the user to run it themselves.`,
      };
    }
  }
});

letronna.action("guard", () => `Guard looked at ${looked} commands and blocked ${blocked}.`);
