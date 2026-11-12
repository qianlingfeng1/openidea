/** Settings › Agents › Coding Agents. */
import { useEffect, useState } from "react";
import { call, commands } from "@/platform/ipc";
import { useRemote } from "@/platform/remote";
import { Checkbox, T } from "@/ui";
import { AgentList } from "./AgentList";
import { tauriCommands, tauriRegistry, type CommandsBackend, type RegistryBackend } from "./registry";
import styles from "./AgentRegistry.module.css";

export function AgentRegistryPage({
  onChanged,
  backend = tauriRegistry,
  commandBackend = tauriCommands,
}: {
  /** An agent was installed, updated or removed (the agent list changed). */
  onChanged?: () => void;
  backend?: RegistryBackend;
  commandBackend?: CommandsBackend;
}) {
  const session = useRemote((s) => s.session);
  const [networkRevision, setNetworkRevision] = useState(0);
  return (
    <div className={styles.body}>
      {session?.commands.includes("agent_configure_network") && (
        <AgentNetworkOption key={session.root} onChanged={() => setNetworkRevision((revision) => revision + 1)} />
      )}
      <AgentList backend={commandBackend} registry={backend} networkRevision={networkRevision} onChanged={onChanged} />
    </div>
  );
}

function AgentNetworkOption({ onChanged }: { onChanged: () => void }) {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [changing, setChanging] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void call(commands.remoteAgentNetwork(null)).then(
      (value) => live && setEnabled(value),
      (e: unknown) => live && setError(e instanceof Error ? e.message : String(e)),
    );
    return () => {
      live = false;
    };
  }, []);

  async function change(value: boolean) {
    setChanging(true);
    setError(null);
    try {
      setEnabled(await call(commands.remoteAgentNetwork(value)));
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setEnabled(await call(commands.remoteAgentNetwork(null)).catch(() => null));
    } finally {
      setChanging(false);
    }
  }

  return (
    <div className={styles.network}>
      <Checkbox
        label="Use this computer's network for agents"
        checked={enabled === true}
        disabled={enabled === null || changing}
        onChange={(value) => void change(value)}
      />
      <p className={styles.hint}>
        <T>
          Routes API requests from newly started agents and agent downloads through this computer for the current remote
          connection.
        </T>
      </p>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
