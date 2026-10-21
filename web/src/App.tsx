import { useStatus } from "./api/useStatus";
import { Card } from "./components/Card";
import { ConfigEditor } from "./components/ConfigEditor";
import { DaemonTable } from "./components/DaemonTable";
import { TelemetryTable } from "./components/TelemetryTable";
import { Topbar } from "./components/Topbar";

export default function App() {
  const { status, error, connection, refresh } = useStatus();

  return (
    <div className="min-h-screen bg-slate-950">
      <Topbar server={status?.server ?? null} connection={connection} onRefresh={refresh} />

      <main className="mx-auto flex max-w-[1400px] flex-col gap-5 px-7 py-6">
        {error ? (
          <p
            role="alert"
            className="rounded-lg border border-rose-900/60 bg-rose-950/40 px-4 py-3 text-sm text-rose-200"
          >
            Cannot reach the control plane: {error}
          </p>
        ) : null}

        <Card
          title="Connected daemons"
          badge={
            <span className="rounded-full border border-slate-700 bg-slate-800 px-2 py-0.5 text-xs font-medium normal-case tracking-normal text-slate-400">
              {status?.daemons.length ?? 0}
            </span>
          }
        >
          <DaemonTable daemons={status?.daemons ?? []} />
        </Card>

        <Card
          title="Recent telemetry"
          hint="Latest 25 events across all daemons"
        >
          <TelemetryTable events={status?.telemetry ?? []} />
        </Card>

        <ConfigEditor config={status?.config ?? null} onPushed={refresh} />
      </main>
    </div>
  );
}
