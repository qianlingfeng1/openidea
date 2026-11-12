import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/react";
import type { ProjectInfo, RemoteSession, ToolInfo } from "@/bindings";
import { useProject } from "@/platform/project";
import { useRemote } from "@/platform/remote";
import { provideNotificationService } from "@/workbench/contributions";
import { queueTask, useTasks } from "@/workbench/tasks";
import { listLocalTools, MemoryToolsBackend, sampleTools, setToolsBackend } from "../backend";
import { activateRemoteLanguageServerSync } from "../remoteSync";
import { DOWNLOAD_QUEUE, useTools } from "../store";

vi.mock("@/platform/ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/platform/ipc")>()),
  isTauri: () => true,
}));

vi.mock("../backend", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../backend")>()),
  listLocalTools: vi.fn(),
}));

const session: RemoteSession = {
  target: { kind: "ssh", config: "dev" },
  root: "/workspace",
  host: "dev",
  os: "linux",
  commands: ["tools_list", "tools_install", "cancel_request"],
};

function project(trusted: boolean): ProjectInfo {
  return { root: session.root, trusted } as ProjectInfo;
}

function localInventory(statuses: Partial<Record<string, ToolInfo["status"]>>): ToolInfo[] {
  return sampleTools().map((tool) => ({ ...tool, status: statuses[tool.name] ?? "missing" }));
}

let backend: MemoryToolsBackend;
let dispose: () => void;

beforeEach(() => {
  useRemote.setState({ session: null });
  useProject.setState({ project: null, opening: false });
  useTools.setState({ tools: [], loaded: false, loading: false, error: null, jobs: {}, logs: {} });
  useTasks.setState({ tasks: [] });
  provideNotificationService({ notify: () => {} });
  backend = new MemoryToolsBackend(sampleTools(), 1);
  setToolsBackend(backend);
  vi.mocked(listLocalTools).mockReset();
  dispose = activateRemoteLanguageServerSync();
});

afterEach(() => {
  dispose();
  setToolsBackend(null);
});

it("installs only available local language servers that are missing and installable on the trusted remote", async () => {
  vi.mocked(listLocalTools).mockResolvedValue(
    localInventory({
      gopls: "installed",
      vtsls: "onPath",
      "rust-analyzer": "installed",
      zls: "onPath",
      delve: "installed",
    }),
  );
  const install = vi.spyOn(backend, "install");

  useRemote.setState({ session });
  useProject.setState({ project: project(false) });
  await Promise.resolve();
  expect(listLocalTools).not.toHaveBeenCalled();
  useProject.setState({ project: project(true), opening: true });
  await Promise.resolve();
  expect(listLocalTools).not.toHaveBeenCalled();

  useProject.setState({ opening: false });
  await waitFor(() => expect(backend.restarted).toEqual(["vtsls", "gopls"]));
  expect(install.mock.calls.map(([name]) => name)).toEqual(["vtsls", "gopls"]);
  expect(listLocalTools).toHaveBeenCalledOnce();
});

it("skips a server that became available while a preceding tool was installed", async () => {
  vi.mocked(listLocalTools).mockResolvedValue(localInventory({ vtsls: "installed", gopls: "installed" }));
  const list = vi.spyOn(backend, "list");
  const install = backend.install.bind(backend);
  const installs = vi.spyOn(backend, "install").mockImplementation(async (...args) => {
    const result = await install(...args);
    backend.tools = backend.tools.map((tool) => (tool.name === "gopls" ? { ...tool, status: "installed" } : tool));
    return result;
  });

  useRemote.setState({ session });
  useProject.setState({ project: project(true) });
  await waitFor(() => {
    expect(list.mock.calls.length).toBeGreaterThanOrEqual(4);
    expect(useTools.getState().jobs["gopls"]).toBeUndefined();
  });
  expect(installs.mock.calls.map(([name]) => name)).toEqual(["vtsls"]);
});

it("cancels a queued install when the remote window starts changing projects", async () => {
  vi.mocked(listLocalTools).mockResolvedValue(localInventory({ basedpyright: "installed" }));
  const install = vi.spyOn(backend, "install");
  let unblock!: () => void;
  const blocker = queueTask(DOWNLOAD_QUEUE, { id: "test-download", title: "Test download" }, () => {
    return new Promise<void>((resolve) => {
      unblock = resolve;
    });
  });

  try {
    useRemote.setState({ session });
    useProject.setState({ project: project(true) });
    await waitFor(() =>
      expect(useTasks.getState().tasks.find((task) => task.id === "tools:basedpyright")).toMatchObject({
        queued: true,
      }),
    );
    useProject.setState({ opening: true });
    useRemote.setState({ session: null });
  } finally {
    unblock();
    await blocker;
  }
  await waitFor(() => expect(useTools.getState().jobs["basedpyright"]).toBeUndefined());
  expect(install).not.toHaveBeenCalled();
});
