import { isTauri, type RemoteSession } from "@/platform/ipc";
import { tx } from "@/platform/i18n";
import { useProject } from "@/platform/project";
import { useRemote } from "@/platform/remote";
import { notifications } from "@/workbench/contributions";
import { listLocalTools, toolsBackend } from "./backend";
import { installTool } from "./store";

async function syncLanguageServers(signal: AbortSignal): Promise<void> {
  const local = await listLocalTools();
  if (signal.aborted) return;
  const available = new Set(
    local.filter((tool) => tool.kind === "languageServer" && tool.status !== "missing").map((tool) => tool.name),
  );
  if (!available.size) return;

  const remote = await toolsBackend().list();
  for (const tool of remote) {
    if (signal.aborted) return;
    if (tool.kind === "languageServer" && tool.status === "missing" && tool.canInstall && available.has(tool.name)) {
      await installTool(tool.name, { onlyIfMissing: true, signal });
    }
  }
}

/** Installs missing servers on the remote platform after the project is ready. */
export function activateRemoteLanguageServerSync(): () => void {
  if (!isTauri()) return () => {};
  let active: { session: RemoteSession; controller: AbortController } | null = null;

  const update = () => {
    const { session } = useRemote.getState();
    const { project, opening } = useProject.getState();
    const ready = session && project?.trusted && project.root === session.root && !opening;
    if (active && (!ready || active.session !== session)) {
      active.controller.abort();
      active = null;
    }
    if (active || !ready || !session.commands.includes("tools_list") || !session.commands.includes("tools_install")) {
      return;
    }

    const controller = new AbortController();
    const { signal } = controller;
    active = { session, controller };
    // The session store is updated just before IPC routing is enabled.
    queueMicrotask(() => {
      if (signal.aborted) return;
      void syncLanguageServers(signal).catch((error: unknown) => {
        if (signal.aborted) return;
        const message =
          error && typeof error === "object" && "message" in error ? String(error.message) : String(error);
        notifications().notify({
          group: "tools",
          level: "warning",
          title: tx("Could not sync language servers"),
          body: message,
        });
      });
    });
  };

  const stopRemote = useRemote.subscribe(update);
  const stopProject = useProject.subscribe(update);
  update();
  return () => {
    stopRemote();
    stopProject();
    active?.controller.abort();
  };
}
