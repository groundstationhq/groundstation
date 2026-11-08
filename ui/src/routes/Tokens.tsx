import { useEffect, useRef, useState, type FormEvent } from "react";
import { StatusDot } from "@/components/ui/primitives";
import { Empty } from "@/components/Shell";
import { createToken, revokeToken, tokens } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { ago, cx } from "@/lib/format";
import type { CreatedToken, IngestToken } from "@/lib/types";

const ACTION = "rounded-md border border-line-2 bg-bg-2 px-2.5 py-1 text-[12px] text-fg-2 transition-colors hover:bg-bg-3 hover:text-fg disabled:opacity-50";

export function Tokens() {
  const [version, setVersion] = useState(0);
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const st = useAsync((s) => tokens(s), [version]);
  const refresh = () => setVersion((n) => n + 1);
  return (
    <div className="space-y-5">
      <div>
        <h1 className="text-[18px] font-semibold tracking-tight text-fg">Ingest tokens</h1>
        <p className="mt-0.5 max-w-[640px] text-[12.5px] leading-[1.55] text-fg-3">
          A token lets <span className="mono text-fg-2">gsd</span> on one computer upload events here. It can't read anything. The easiest way to get one is <span className="mono text-fg-2">groundstation cloud login --endpoint {location.origin}</span> on that computer.
        </p>
      </div>
      <NewToken onCreated={(t) => { setCreated(t); refresh(); }} />
      {created && <Created key={created.id} token={created} onDone={() => setCreated(null)} />}
      {st.status === "loading" && <div className="label py-10 text-center">loading…</div>}
      {st.status === "error" && <Empty title="Couldn't load tokens" body={st.error} />}
      {st.status === "ok" && st.data.length === 0 && <Empty title="No tokens yet" body={<>Create one above, or run <span className="mono text-fg-2">groundstation cloud login --endpoint {location.origin}</span> on the computer that runs your agents.</>} />}
      {st.status === "ok" && st.data.length > 0 && <TokenTable rows={st.data} onRevoked={refresh} />}
    </div>
  );
}

function NewToken({ onCreated }: { onCreated: (t: CreatedToken) => void }) {
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    createToken(label)
      .then((t) => { setLabel(""); onCreated(t); })
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() => setBusy(false));
  };
  return (
    <form onSubmit={submit} className="flex flex-wrap items-center gap-2">
      <label htmlFor="token-label" className="sr-only">Label</label>
      <input id="token-label" value={label} onChange={(e) => setLabel(e.target.value)} maxLength={100} required placeholder="Which computer, e.g. work laptop" className="h-8 w-full max-w-[320px] rounded-md border border-line-2 bg-bg-1 px-2.5 text-[13px] text-fg placeholder:text-fg-4" />
      <button type="submit" className={ACTION} disabled={busy || !label.trim()}>Create token</button>
      {error != null && <span role="alert" className="text-[12px] text-err">{error || "That didn't work."}</span>}
    </form>
  );
}

function Created({ token, onDone }: { token: CreatedToken; onDone: () => void }) {
  const [copied, setCopied] = useState(false);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => heading.current?.focus(), []);
  const copy = () => navigator.clipboard.writeText(token.secret).then(() => setCopied(true), () => setCopied(false));
  // Only through the CLI: it says what gsd will upload, the events it holds
  // already included, before it turns uploading on.
  return (
    <section className="rounded-lg border border-line-2 bg-bg-1 px-4 py-3.5">
      <h2 ref={heading} tabIndex={-1} className="text-[14px] font-medium text-fg outline-none">Token for “{token.label}”</h2>
      <p className="mt-1 text-[12.5px] leading-[1.55] text-fg-3">This is the only time it's shown. On that computer, run <span className="mono text-fg-2">groundstation cloud login --endpoint {location.origin} --token-stdin</span> and paste it. Before it starts uploading, it says what will leave the computer.</p>
      <pre className="mono mt-3 overflow-x-auto rounded-md border border-line bg-bg px-3 py-2 text-[12px] text-fg scroll-thin">{token.secret}</pre>
      <div className="mt-3 flex gap-2">
        <button type="button" className={ACTION} onClick={copy}>{copied ? "Copied" : "Copy token"}</button>
        <button type="button" className={ACTION} onClick={onDone}>Done</button>
      </div>
    </section>
  );
}

function TokenTable({ rows, onRevoked }: { rows: IngestToken[]; onRevoked: () => void }) {
  return (
    <div className="overflow-x-auto rounded-lg border border-line bg-bg-1 scroll-thin">
      <table className="w-full min-w-[340px]">
        <thead>
          <tr className="border-b border-line">
            {["Label", "Created", "Last used", "Status", ""].map((h, i) => (
              <th key={i} className={cx("label whitespace-nowrap px-3 py-2 text-left text-[10px] font-normal first:pl-4 last:pr-4", i === 1 || i === 2 ? "hidden sm:table-cell" : "")}>{h}</th>
            ))}
          </tr>
        </thead>
        <tbody>{rows.map((t) => <TokenRow key={t.id} t={t} onRevoked={onRevoked} />)}</tbody>
      </table>
    </div>
  );
}

function TokenRow({ t, onRevoked }: { t: IngestToken; onRevoked: () => void }) {
  const [confirming, setConfirming] = useState(false);
  // Back on "Revoke…" after Keep, so the keyboard doesn't lose its place.
  const ask = useRef<HTMLButtonElement>(null);
  const kept = useRef(false);
  useEffect(() => {
    if (!confirming && kept.current) ask.current?.focus();
    kept.current = false;
  }, [confirming]);
  const [error, setError] = useState<string | null>(null);
  const revoke = () => revokeToken(t.id).then(onRevoked, (e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  return (
    <tr className="border-b border-line last:border-0">
      <td className="max-w-0 truncate px-3 py-2.5 pl-4 text-[13px] text-fg">{t.label}</td>
      <td className="mono hidden whitespace-nowrap px-3 py-2.5 text-[12px] text-fg-3 sm:table-cell">{ago(t.created_at)}</td>
      <td className="mono hidden whitespace-nowrap px-3 py-2.5 text-[12px] text-fg-3 sm:table-cell">{t.last_used_at ? ago(t.last_used_at) : "never"}</td>
      <td className="whitespace-nowrap px-3 py-2.5">
        {t.revoked_at ? (
          <span className="mono inline-flex items-center gap-1.5 text-[11.5px] text-fg-3"><StatusDot status="idle" />Revoked</span>
        ) : (
          <span className="mono inline-flex items-center gap-1.5 text-[11.5px] text-ok"><StatusDot status="ok" />Active</span>
        )}
      </td>
      <td className="whitespace-nowrap px-3 py-2.5 pr-4 text-right">
        {!t.revoked_at && (confirming ? (
          <span className="inline-flex gap-2">
            <button type="button" autoFocus className={cx(ACTION, "text-err")} onClick={revoke}>Revoke</button>
            <button type="button" className={ACTION} onClick={() => { kept.current = true; setConfirming(false); }}>Keep</button>
          </span>
        ) : (
          <button ref={ask} type="button" className={ACTION} onClick={() => setConfirming(true)}>Revoke…</button>
        ))}
        {error != null && <div role="alert" className="text-[12px] text-err">{error || "That didn't work."}</div>}
      </td>
    </tr>
  );
}
