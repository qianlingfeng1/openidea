/** Picks an SSH configuration and a folder on that host, then opens it in this or a new window. */
import { useCallback, useEffect, useRef, useState } from "react";
import { executeAction } from "@/platform/actions";
import { useTx } from "@/platform/i18n";
import {
  call,
  commands,
  isCancelled,
  nextRequestId,
  onIdeEvent,
  type ContainerFolders,
  type RemoteEntry,
  type RemoteTarget,
  type RequestId,
  type SshContainers,
} from "@/platform/ipc";
import { useProject } from "@/platform/project";
import { cancelRemoteConnection, cancelRemoteRequest, useRemote, useRemoteProgress } from "@/platform/remote";
import { sshLabel, type SshConfigValid, validateRemote } from "@/platform/remoteSettings";
import { Button, Dialog, Icon, Input, ProgressBar, Select, Spinner } from "@/ui";
import { openRemoteProject } from "@/workbench/projectWindow";
import {
  browsableFolders,
  closeConnectDialog,
  listedDirectory,
  remoteParent,
  stageLabel,
  useConnectDialog,
} from "./model";
import styles from "./RemoteDev.module.css";

export function ConnectHost() {
  const { open, mode } = useConnectDialog();
  if (!open) return null;
  if (mode === "reopen") return <ReopenContainerDialog />;
  if (mode === "attach") return <AttachContainerDialog />;
  return <ConnectDialog />;
}

const SSH_SETTINGS_PAGE = "tools.sshConfigurations";

/**
 * This machine's SSH configurations: the connection is made from here. A remote project's
 * window has the remote host's settings, which don't list them.
 */
function useLocalSshConfigs(): SshConfigValid[] {
  const [configs, setConfigs] = useState<SshConfigValid[]>([]);
  useEffect(() => {
    let live = true;
    const load = () =>
      void commands.settingsGetGlobal().then(
        (s) => live && setConfigs(validateRemote(s).ssh.configs),
        () => {},
      );
    load();
    const off = onIdeEvent("settingsChanged", load);
    return () => {
      live = false;
      off();
    };
  }, []);
  return configs;
}

function ConnectDialog() {
  const tx = useTx();
  const configs = useLocalSshConfigs();
  const targetInWindow = useRemote((s) => s.session?.target);
  const canReconnect = targetInWindow?.kind === "sshContainer" || targetInWindow?.kind === "sshDevContainer";
  const [picked, setSsh] = useState("");
  const ssh = configs.some((c) => c.id === picked) ? picked : (configs[0]?.id ?? "");
  const [kind, setKind] = useState<RemoteTarget["kind"]>("ssh");
  // Installed WSL distributions; none (or no WSL at all) hides the WSL choice.
  const [distros, setDistros] = useState<string[]>([]);
  const [distro, setDistro] = useState("");
  const [path, setPath] = useState("~");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [browse, setBrowse] = useState<{ dir: string; folders: RemoteEntry[] } | null>(null);

  useEffect(() => {
    let live = true;
    // Detection is a convenience: any failure just leaves SSH as the only choice.
    void call(commands.wslDistros())
      .then((found) => {
        if (!live) return;
        setDistros(found);
        setDistro((d) => d || (found[0] ?? ""));
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);

  const wsl = kind === "wsl";
  const name = wsl ? distro : ssh;
  const target: RemoteTarget = wsl ? { kind: "wsl", distro } : { kind: "ssh", config: ssh };

  const list = async (dir: string) => {
    setError(null);
    setBusy(true);
    try {
      const entries = await call(commands.remoteBootstrapList(ssh, dir));
      const shown = listedDirectory(dir, entries);
      setBrowse({ dir: shown, folders: browsableFolders(entries) });
      setPath(shown);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const connect = async () => {
    setError(null);
    setBusy(true);
    try {
      if (await openRemoteProject(target, path.trim() || "~")) closeConnectDialog();
      else setBusy(false);
    } catch (e) {
      if (!isCancelled(e)) setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const options = configs.map((c) => ({ value: c.id, label: c.name.trim() || sshLabel(c), detail: sshLabel(c) }));
  const ready = !!name && !busy && !!path.trim();
  const connecting = useRemoteProgress((s) => s.progress !== null);

  return (
    <Dialog
      open
      title={tx("Remote Development")}
      onClose={closeConnectDialog}
      width={560}
      buttons={[
        {
          label: connecting ? "Connecting…" : "Open",
          role: "default",
          disabled: !ready,
          onClick: () => void connect(),
        },
        { label: "Cancel", role: "cancel" },
      ]}
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          if (ready) void connect();
        }}
      >
        <p className={styles.hint}>
          {tx("The project runs on the remote host; the IDE backend is installed there on first use.")}
        </p>
        {distros.length > 0 && (
          <>
            <label className={styles.label}>{tx("Connect through:")}</label>
            <Select
              value={kind}
              options={[
                { value: "ssh", label: "SSH" },
                { value: "wsl", label: "WSL" },
              ]}
              onChange={(k) => {
                setKind(k as RemoteTarget["kind"]);
                setBrowse(null);
              }}
              aria-label={tx("Connect through")}
            />
          </>
        )}
        {wsl ? (
          <>
            <label className={styles.label}>{tx("WSL distribution:")}</label>
            <Select
              value={distro || null}
              options={distros.map((d) => ({ value: d, label: d }))}
              onChange={setDistro}
              aria-label={tx("WSL distribution")}
            />
          </>
        ) : (
          <>
            <label className={styles.label}>{tx("SSH configuration:")}</label>
            <div className={styles.row}>
              <Select
                className={styles.grow}
                value={ssh || null}
                options={options}
                placeholder={tx("No SSH configurations")}
                onChange={(id) => {
                  setSsh(id);
                  setBrowse(null);
                }}
                aria-label={tx("SSH configuration")}
              />
              <Button onClick={() => void executeAction("file.settings", SSH_SETTINGS_PAGE)}>{tx("Configure…")}</Button>
            </div>
          </>
        )}
        <label className={styles.label} htmlFor="remote-dev-path">
          {tx("Project directory:")}
        </label>
        <div className={styles.row}>
          <Input
            id="remote-dev-path"
            className={styles.grow}
            data-autofocus
            value={path}
            spellCheck={false}
            onChange={(e) => setPath(e.target.value)}
          />
          {/* Browsing lists over SFTP; a WSL distribution has no such session before connecting. */}
          {!wsl && (
            <Button disabled={!ssh || busy} onClick={() => void list(path.trim() || "~")}>
              {tx("Browse")}
            </Button>
          )}
        </div>
        {browse && (
          <ul className={styles.folders} aria-label={tx("Remote folders")}>
            {browse.dir !== "/" && (
              <li>
                <button type="button" className={styles.folder} onClick={() => void list(remoteParent(browse.dir))}>
                  <Icon name="folder" />
                  ..
                </button>
              </li>
            )}
            {browse.folders.map((f) => (
              <li key={f.path}>
                <button type="button" className={styles.folder} onClick={() => void list(f.path)}>
                  <Icon name="folder" />
                  {f.name}
                </button>
              </li>
            ))}
          </ul>
        )}
        <ConnectProgress />
        {error && (
          <div className={styles.error} role="alert">
            {error}
          </div>
        )}
        <div className={styles.containerActions}>
          <Button disabled={busy} onClick={() => void executeAction("remote.attachContainer")}>
            <Icon name="file-docker" />
            {tx("Connect to Running Container…")}
          </Button>
          {targetInWindow?.kind === "ssh" && (
            <Button disabled={busy} onClick={() => void executeAction("remote.reopenInContainer")}>
              {tx("Reopen in Container")}
            </Button>
          )}
          {canReconnect && (
            <Button disabled={busy} onClick={() => void executeAction("remote.reconnectContainer")}>
              {tx("Reconnect to Container")}
            </Button>
          )}
        </div>
      </form>
    </Dialog>
  );
}

function useRemoteLookup() {
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const request = useRef<RequestId | null>(null);

  const cancelLookup = useCallback(() => {
    const id = request.current;
    request.current = null;
    if (id !== null) void cancelRemoteRequest(id);
  }, []);

  const load = useCallback(
    async <T,>(work: (id: RequestId) => Promise<T>, accept: (value: T) => void) => {
      cancelLookup();
      const id = nextRequestId();
      request.current = id;
      setLoading(true);
      setError(null);
      try {
        const value = await work(id);
        if (request.current === id) accept(value);
      } catch (e) {
        if (request.current === id && !isCancelled(e)) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (request.current === id) {
          request.current = null;
          setLoading(false);
        }
      }
    },
    [cancelLookup],
  );
  return { error, setError, loading, setLoading, cancelLookup, load };
}

function ReopenContainerDialog() {
  const tx = useTx();
  const source = useRemote((s) => (s.session?.target.kind === "ssh" ? s.session : null));
  const trusted = useProject((s) => s.project?.trusted === true);
  const [files, setFiles] = useState<string[] | null>(null);
  const [picked, setPicked] = useState("");
  const [opening, setOpening] = useState(false);
  const { loading, error, setError, load, cancelLookup } = useRemoteLookup();
  const file = files?.includes(picked) ? picked : (files?.[0] ?? "");

  useEffect(() => {
    if (!source) return;
    const scheduled = setTimeout(
      () => void load((request) => call(commands.remoteDevcontainerConfigs(request)), setFiles),
      0,
    );
    return () => {
      clearTimeout(scheduled);
      cancelLookup();
    };
  }, [cancelLookup, load, source]);

  const open = async () => {
    if (source?.target.kind !== "ssh" || !file || !trusted || loading || opening) return;
    setOpening(true);
    setError(null);
    try {
      const target: RemoteTarget = {
        kind: "sshDevContainer",
        config: source.target.config,
        folder: source.root,
        file,
      };
      if (await openRemoteProject(target, source.root)) closeConnectDialog();
    } catch (e) {
      if (!isCancelled(e)) setError(e instanceof Error ? e.message : String(e));
    } finally {
      setOpening(false);
    }
  };

  const close = () => {
    cancelLookup();
    if (opening) cancelRemoteConnection();
    closeConnectDialog();
  };

  return (
    <Dialog
      open
      title={tx("Reopen in Container")}
      onClose={close}
      width={560}
      buttons={[
        {
          label: "Reopen in Container",
          role: "default",
          disabled: !source || !file || !trusted || loading || opening,
          onClick: () => void open(),
        },
        { label: "Cancel", role: "cancel" },
      ]}
    >
      <form
        className={styles.form}
        onSubmit={(event) => {
          event.preventDefault();
          void open();
        }}
      >
        {source ? (
          <>
            <p className={styles.hint}>{tx("Build or start this project's development container on the SSH host.")}</p>
            <label className={styles.label} htmlFor="ssh-devcontainer-host">
              {tx("SSH configuration:")}
            </label>
            <Input id="ssh-devcontainer-host" value={source.host} readOnly />
            <label className={styles.label} htmlFor="ssh-devcontainer-root">
              {tx("Project directory:")}
            </label>
            <Input id="ssh-devcontainer-root" value={source.root} readOnly />
            {!!files?.length && (
              <>
                <label className={styles.label}>{tx("Dev Container configuration")}</label>
                <Select
                  value={file}
                  options={files.map((path) => ({ value: path, label: path }))}
                  onChange={setPicked}
                  disabled={opening}
                  aria-label={tx("Dev Container configuration")}
                />
              </>
            )}
            {loading && (
              <div className={styles.progressLine} role="status">
                <Spinner size={12} />
                {tx("Looking for devcontainer.json on the SSH host…")}
              </div>
            )}
            {files?.length === 0 && (
              <p className={styles.hint}>{tx("No devcontainer.json was found in this SSH project.")}</p>
            )}
            {!trusted && !opening && (
              <div className={styles.row}>
                <p className={styles.hint}>{tx("Trust the SSH project before reopening it in a container")}</p>
                <Button onClick={() => void executeAction("project.trust")}>{tx("Trust Project")}</Button>
              </div>
            )}
          </>
        ) : (
          !opening && <p className={styles.hint}>{tx("Open a project on an SSH host first")}</p>
        )}
        <ConnectProgress />
        {error && (
          <div className={styles.error} role="alert">
            {tx(error)}
          </div>
        )}
      </form>
    </Dialog>
  );
}

function AttachContainerDialog() {
  const tx = useTx();
  const configs = useLocalSshConfigs();
  const current = useRemote((s) => s.session?.target);
  const [picked, setPicked] = useState(
    current?.kind === "ssh" || current?.kind === "sshContainer" || current?.kind === "sshDevContainer"
      ? current.config
      : "",
  );
  const config = configs.some((c) => c.id === picked) ? picked : (configs[0]?.id ?? "");
  const [listing, setListing] = useState<SshContainers | null>(null);
  const [containerId, setContainerId] = useState("");
  const [query, setQuery] = useState("");
  const [folders, setFolders] = useState<ContainerFolders | null>(null);
  const [path, setPath] = useState("");
  const [opening, setOpening] = useState(false);
  const { error, setError, loading, setLoading, cancelLookup, load } = useRemoteLookup();

  const refresh = useCallback(() => {
    setListing(null);
    setContainerId("");
    setFolders(null);
    if (config) {
      void load((id) => call(commands.remoteContainerList(config, id)), setListing);
    } else {
      setLoading(false);
      setError(null);
    }
  }, [config, load, setError, setLoading]);

  useEffect(() => {
    const scheduled = setTimeout(refresh, 0);
    return () => {
      clearTimeout(scheduled);
      cancelLookup();
    };
  }, [cancelLookup, refresh]);

  const search = query.trim().toLowerCase();
  const visible = listing?.containers.filter((c) =>
    [c.name, c.image, c.id].some((v) => v.toLowerCase().includes(search)),
  );
  const container = (folders ? listing?.containers : visible)?.find((c) => c.id === containerId);
  const target: RemoteTarget | null =
    listing && container ? { kind: "sshContainer", config, daemon: listing.daemon, container: container.id } : null;
  const busy = loading || opening;

  const lookup = (directory: string | null, accept?: (folder: string) => void) => {
    if (!target) return;
    void load(
      (id) => call(commands.remoteContainerFolders(target, directory, id)),
      (result) => {
        setFolders(result);
        setPath(result.directory);
        accept?.(result.directory);
      },
    );
  };

  const open = async (directory: string) => {
    if (!target) return;
    setOpening(true);
    setError(null);
    try {
      if (await openRemoteProject(target, directory)) closeConnectDialog();
    } catch (e) {
      if (!isCancelled(e)) setError(e instanceof Error ? e.message : String(e));
    } finally {
      setOpening(false);
    }
  };

  const submit = () => {
    if (busy || !target) return;
    if (!folders) lookup(null);
    else if (path.trim() === folders.directory) void open(folders.directory);
    else if (path.trim()) lookup(path.trim(), (directory) => void open(directory));
  };

  const close = () => {
    cancelLookup();
    if (opening) cancelRemoteConnection();
    closeConnectDialog();
  };

  return (
    <Dialog
      open
      title={tx("Connect to Running Container…")}
      onClose={close}
      width={560}
      buttons={[
        {
          label: folders ? "Open Container Project" : "Select Container",
          role: "default",
          disabled: !target || busy || (!!folders && !path.trim()),
          onClick: submit,
        },
        ...(folders
          ? [
              {
                label: "Back",
                disabled: busy,
                onClick: () => {
                  setFolders(null);
                  setError(null);
                },
              },
            ]
          : []),
        { label: "Cancel", role: "cancel" },
      ]}
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        {folders && container ? (
          <>
            <div className={styles.row}>
              <Icon name="file-docker" />
              <strong>{container.name}</strong>
              <span className={styles.hint}>{configs.find((c) => c.id === config)?.name || config}</span>
            </div>
            {folders.user && <p className={styles.hint}>{tx`Container user: ${folders.user}`}</p>}
            <label className={styles.label} htmlFor="remote-container-path">
              {tx("Container project directory")}
            </label>
            <div className={styles.row}>
              <Input
                id="remote-container-path"
                className={styles.grow}
                data-autofocus
                value={path}
                spellCheck={false}
                onChange={(e) => setPath(e.target.value)}
              />
              <Button disabled={busy || !path.trim()} onClick={() => lookup(path.trim())}>
                {tx("Browse")}
              </Button>
            </div>
            <ul className={styles.folders} aria-label={tx("Remote folders")}>
              {folders.directory !== "/" && (
                <li>
                  <button
                    type="button"
                    className={styles.folder}
                    disabled={busy}
                    onClick={() => lookup(remoteParent(folders.directory))}
                  >
                    <Icon name="folder" />
                    ..
                  </button>
                </li>
              )}
              {folders.folders.map((folder) => (
                <li key={folder}>
                  <button
                    type="button"
                    className={styles.folder}
                    disabled={busy}
                    onClick={() => lookup(`${folders.directory.replace(/\/$/, "")}/${folder}`)}
                  >
                    <Icon name="folder" />
                    {folder}
                  </button>
                </li>
              ))}
            </ul>
          </>
        ) : (
          <>
            <label className={styles.label}>{tx("SSH configuration:")}</label>
            <div className={styles.row}>
              <Select
                className={styles.grow}
                value={config || null}
                options={configs.map((c) => ({
                  value: c.id,
                  label: c.name.trim() || sshLabel(c),
                  detail: sshLabel(c),
                }))}
                placeholder={tx("No SSH configurations")}
                onChange={(id) => {
                  setListing(null);
                  setPicked(id);
                }}
                aria-label={tx("SSH configuration")}
              />
              <Button onClick={() => void executeAction("file.settings", SSH_SETTINGS_PAGE)}>{tx("Configure…")}</Button>
            </div>
            <label className={styles.label} htmlFor="remote-container-search">
              {tx("Search containers")}
            </label>
            <div className={styles.row}>
              <Input
                id="remote-container-search"
                className={styles.grow}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
              <Button disabled={!config || busy} onClick={refresh}>
                {tx("Refresh")}
              </Button>
            </div>
            {!!visible?.length && (
              <ul className={styles.folders} aria-label={tx("Running containers")}>
                {visible.map((c) => (
                  <li key={c.id}>
                    <button
                      type="button"
                      className={styles.container}
                      aria-pressed={c.id === containerId}
                      disabled={busy}
                      onClick={() => setContainerId(c.id)}
                    >
                      <strong>{c.name}</strong>
                      <span>{c.status}</span>
                      <small>
                        {c.image} · {c.id.slice(0, 12)}
                      </small>
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {visible?.length === 0 && (
              <p className={styles.hint}>{search ? tx("No matching containers") : tx("No running containers")}</p>
            )}
          </>
        )}
        {loading && (
          <div className={styles.progressLine} role="status">
            <Spinner size={12} />
            {tx("Checking Docker on the SSH host…")}
          </div>
        )}
        <ConnectProgress />
        {error && (
          <div className={styles.error} role="alert">
            {tx(error)}
          </div>
        )}
      </form>
    </Dialog>
  );
}

/** The stage of the connection in progress (and how far a download / upload is). */
function ConnectProgress() {
  const tx = useTx();
  const progress = useRemoteProgress((s) => s.progress);
  if (!progress) return null;
  const percent = progress.fraction == null ? null : Math.round(progress.fraction * 100);
  return (
    <div className={styles.progress} role="status">
      <div className={styles.progressLine}>
        <Spinner size={12} />
        <span>{tx(stageLabel(progress.stage))}</span>
        {percent != null && <span className={styles.percent}>{percent}%</span>}
      </div>
      <ProgressBar value={progress.fraction} aria-label={tx(stageLabel(progress.stage))} />
    </div>
  );
}
