import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emitLocalIdeEvent, type IdeEvent, type RemoteEntry, type Settings } from "@/platform/ipc";
import { useProject } from "@/platform/project";
import { useRemote, useRemoteProgress } from "@/platform/remote";
import { ConnectHost } from "./ConnectDialog";
import { closeConnectDialog, openConnectDialog, useConnectDialog } from "./model";

const { list, containers, folders, devConfigs, distros, globalSettings, openRemote, executeAction } = vi.hoisted(
  () => ({
    list: vi.fn(),
    containers: vi.fn(),
    folders: vi.fn(),
    devConfigs: vi.fn(),
    distros: vi.fn(),
    globalSettings: vi.fn(),
    openRemote: vi.fn(),
    executeAction: vi.fn(),
  }),
);

vi.mock("@/platform/ipc", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/platform/ipc")>();
  return {
    ...real,
    commands: {
      ...real.commands,
      remoteBootstrapList: list,
      remoteContainerList: containers,
      remoteContainerFolders: folders,
      remoteDevcontainerConfigs: devConfigs,
      wslDistros: distros,
      settingsGetGlobal: globalSettings,
    },
  };
});
vi.mock("@/workbench/projectWindow", () => ({ openRemoteProject: openRemote }));
vi.mock("@/platform/actions", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/platform/actions")>()),
  executeAction,
}));

const entry = (path: string, isDir = true): RemoteEntry => ({
  name: path.slice(path.lastIndexOf("/") + 1),
  path,
  isDir,
  isSymlink: false,
  size: 0,
  mtime: null,
  permissions: "rwxr-xr-x",
});

beforeEach(() => {
  vi.resetAllMocks();
  distros.mockResolvedValue({ status: "ok", data: [] });
  // A remote project's window: its settings are the remote host's, without this machine's SSH configurations.
  useProject.setState({ project: null, opening: false, settings: { ssh: { configs: [] } } as unknown as Settings });
  globalSettings.mockResolvedValue({
    ssh: {
      configs: [
        { id: "first", name: "First host", host: "first.example", user: "dev" },
        { id: "selected", name: "Selected host", host: "selected.example", user: "dev" },
      ],
    },
  } as Settings);
  useRemoteProgress.setState({ progress: null, location: null });
  openConnectDialog();
});

afterEach(() => {
  closeConnectDialog();
  useRemote.setState({ session: null });
});

it("browses the selected SSH host, navigates folders and opens one without a current project", async () => {
  let finish!: (result: { status: "ok"; data: RemoteEntry[] }) => void;
  list.mockImplementationOnce(() => new Promise((resolve) => (finish = resolve)));
  openRemote.mockResolvedValue(true);
  render(<ConnectHost />);
  await waitFor(() => expect(screen.getByRole("button", { name: "Browse" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "SSH configuration" }));
  fireEvent.click(screen.getByRole("option", { name: /Selected host/ }));
  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  expect(list).toHaveBeenCalledWith("selected", "~");
  expect(screen.getByRole("button", { name: "Browse" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Open" })).toBeDisabled();
  finish({
    status: "ok",
    data: [entry("/home/dev/.cache"), entry("/home/dev/projects"), entry("/home/dev/README.md", false)],
  });
  const folders = await screen.findByRole("list", { name: "Remote folders" });
  expect(
    within(folders)
      .getAllByRole("button")
      .map((button) => button.textContent),
  ).toEqual(["..", "projects", ".cache"]);
  expect(screen.getByLabelText("Project directory:")).toHaveValue("/home/dev");
  list.mockResolvedValueOnce({ status: "ok", data: [entry("/home/dev/projects/demo")] });
  fireEvent.click(within(folders).getByRole("button", { name: "projects" }));
  await screen.findByRole("button", { name: "demo" });
  expect(list).toHaveBeenLastCalledWith("selected", "/home/dev/projects");
  list.mockResolvedValueOnce({ status: "ok", data: [entry("/home/dev/projects")] });
  fireEvent.click(within(folders).getByRole("button", { name: ".." }));
  await waitFor(() => expect(screen.getByLabelText("Project directory:")).toHaveValue("/home/dev"));
  expect(list).toHaveBeenLastCalledWith("selected", "/home/dev");
  list.mockResolvedValueOnce({ status: "ok", data: [] });
  fireEvent.click(within(folders).getByRole("button", { name: "projects" }));
  await waitFor(() => expect(screen.getByLabelText("Project directory:")).toHaveValue("/home/dev/projects"));
  expect(within(folders).getAllByRole("button")).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "Open" }));
  await waitFor(() =>
    expect(openRemote).toHaveBeenCalledWith({ kind: "ssh", config: "selected" }, "/home/dev/projects"),
  );
  expect(useConnectDialog.getState().open).toBe(false);
  expect(useProject.getState().project).toBeNull();
});

it("shows a connection error and lets the welcome window retry Browse", async () => {
  list.mockRejectedValueOnce(new Error("Connection refused"));
  render(<ConnectHost />);
  await waitFor(() => expect(screen.getByRole("button", { name: "Browse" })).toBeEnabled());
  fireEvent.change(screen.getByLabelText("Project directory:"), { target: { value: "  /srv/projects  " } });
  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Connection refused");
  expect(list).toHaveBeenCalledWith("first", "/srv/projects");
  expect(screen.getByRole("button", { name: "Browse" })).toBeEnabled();
  list.mockResolvedValueOnce({ status: "ok", data: [] });
  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByRole("list", { name: "Remote folders" });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(openRemote).not.toHaveBeenCalled();
});

it("offers SSH configuration and disables Browse until one exists, then lists the new one", async () => {
  globalSettings.mockResolvedValue({ ssh: { configs: [] } } as unknown as Settings);
  render(<ConnectHost />);
  await waitFor(() => expect(globalSettings).toHaveBeenCalled());
  expect(screen.getByRole("button", { name: "Browse" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Open" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Configure…" }));
  expect(executeAction).toHaveBeenCalledWith("file.settings", "tools.sshConfigurations");
  expect(list).not.toHaveBeenCalled();

  globalSettings.mockResolvedValue({
    ssh: { configs: [{ id: "dev", name: "dev", host: "192.168.1.2", user: "robin" }] },
  } as Settings);
  act(() => emitLocalIdeEvent({ type: "settingsChanged", key: "ssh.configs" } as IdeEvent));
  await waitFor(() => expect(screen.getByRole("button", { name: "Browse" })).toBeEnabled());
  expect(screen.getByRole("button", { name: "SSH configuration" })).toHaveTextContent("dev");
});

it("hides the WSL choice when no distribution is installed", async () => {
  render(<ConnectHost />);
  await waitFor(() => expect(distros).toHaveBeenCalled());
  expect(screen.queryByRole("button", { name: "Connect through" })).toBeNull();
});

it("opens a folder of the chosen WSL distribution, without browsing over SFTP", async () => {
  const installed = ["Ubuntu-24.04", "Debian"];
  distros.mockResolvedValue({ status: "ok", data: installed });
  openRemote.mockResolvedValue(true);
  render(<ConnectHost />);
  fireEvent.click(await screen.findByRole("button", { name: "Connect through" }));
  fireEvent.click(screen.getByRole("option", { name: "WSL" }));
  expect(screen.queryByRole("button", { name: "Browse" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "WSL distribution" }));
  fireEvent.click(screen.getByRole("option", { name: installed[1] }));
  fireEvent.click(screen.getByRole("button", { name: "Open" }));
  await waitFor(() => expect(openRemote).toHaveBeenCalledWith({ kind: "wsl", distro: installed[1] }, "~"));
  expect(list).not.toHaveBeenCalled();
});

it("offers a built-in action for connecting to a running container", async () => {
  render(<ConnectHost />);
  fireEvent.click(await screen.findByRole("button", { name: "Connect to Running Container…" }));
  expect(executeAction).toHaveBeenCalledWith("remote.attachContainer");
});

it("opens a directory of the full container ID on the existing SSH connection", async () => {
  const id = "ab".repeat(32);
  const target = { kind: "sshContainer", config: "selected", daemon: "engine-a", container: id };
  useRemote.setState({
    session: {
      target: { kind: "ssh", config: "selected" },
      root: "/srv/app",
      host: "Selected host",
      os: "linux",
      commands: [],
    },
  });
  containers.mockResolvedValue({
    status: "ok",
    data: { daemon: "engine-a", containers: [{ id, name: "api-dev", image: "ubuntu:24.04", status: "Up" }] },
  });
  folders.mockResolvedValueOnce({
    status: "ok",
    data: { directory: "/workspaces/app", folders: ["src"], user: "dev" },
  });
  folders.mockResolvedValueOnce({ status: "ok", data: { directory: "/workspaces/app/src", folders: [], user: "dev" } });
  openRemote.mockResolvedValue(true);
  openConnectDialog("attach");
  render(<ConnectHost />);
  fireEvent.click(await screen.findByRole("button", { name: /api-dev/ }));
  expect(containers).toHaveBeenCalledWith("selected", expect.any(Number));
  fireEvent.click(screen.getByRole("button", { name: "Select Container" }));
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  await waitFor(() => expect(screen.getByLabelText("Container project directory")).toHaveValue("/workspaces/app/src"));
  expect(folders).toHaveBeenCalledWith(target, null, expect.any(Number));
  expect(folders).toHaveBeenCalledWith(target, "/workspaces/app/src", expect.any(Number));
  fireEvent.click(screen.getByRole("button", { name: "Open Container Project" }));
  await waitFor(() => expect(openRemote).toHaveBeenCalledWith(target, "/workspaces/app/src"));
  expect(list).not.toHaveBeenCalled();
});

it("reopens a trusted SSH project with the selected host configuration file", async () => {
  const root = "/srv/my project";
  const file = ".devcontainer/compose/devcontainer.json";
  useProject.setState({ project: { root, trusted: true } as never });
  useRemote.setState({
    session: { target: { kind: "ssh", config: "selected" }, root, host: "Selected host", os: "linux", commands: [] },
  });
  devConfigs.mockResolvedValue({ status: "ok", data: [".devcontainer/devcontainer.json", file] });
  openRemote.mockResolvedValue(true);
  openConnectDialog("reopen");
  render(<ConnectHost />);
  fireEvent.click(await screen.findByRole("button", { name: "Dev Container configuration" }));
  fireEvent.click(screen.getByRole("option", { name: file }));
  fireEvent.click(screen.getByRole("button", { name: "Reopen in Container" }));
  await waitFor(() =>
    expect(openRemote).toHaveBeenCalledWith({ kind: "sshDevContainer", config: "selected", folder: root, file }, root),
  );
  expect(devConfigs).toHaveBeenCalledWith(expect.any(Number));
  expect(containers).not.toHaveBeenCalled();
});
