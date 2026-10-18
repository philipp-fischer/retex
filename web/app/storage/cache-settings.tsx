import React, { useEffect, useRef, useState } from "react";
import { PackageCache } from "../../package-cache.js";

const cache = new PackageCache();
type Snapshot = Awaited<ReturnType<typeof cache.snapshot>>;
type PickerWindow = Window & {
  showDirectoryPicker?: (options: {
    mode: "readwrite";
    id?: string;
  }) => Promise<FileSystemDirectoryHandle>;
};
export function formatStorage(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}

export function CacheSettings() {
  const dialog = useRef<HTMLDialogElement>(null);
  const [snapshot, setSnapshot] = useState<Snapshot>();
  const [contents, setContents] =
    useState<Awaited<ReturnType<typeof cache.contents>>>();
  const [limit, setLimit] = useState("1024");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [persistent, setPersistent] = useState(false);
  const picker = (window as PickerWindow).showDirectoryPicker;
  const folderSupported = !!picker && !!navigator.locks;

  useEffect(() => {
    let mounted = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let sequence = 0;
    const refresh = async () => {
      const current = ++sequence;
      try {
        const value = await cache.snapshot();
        const entries =
          dialog.current?.open && value.permission === "granted"
            ? await cache.contents()
            : undefined;
        if (mounted && current === sequence) {
          setSnapshot(value);
          if (entries) setContents(entries);
        }
      } catch (failure) {
        if (mounted) setError(String((failure as Error).message || failure));
      }
    };
    const changed = () => {
      clearTimeout(timer);
      timer = setTimeout(refresh, 250);
    };
    const unsubscribe = cache.subscribe(changed);
    void refresh();
    const interval = setInterval(refresh, 30_000);
    window.addEventListener("focus", changed);
    return () => {
      mounted = false;
      unsubscribe();
      clearTimeout(timer);
      clearInterval(interval);
      window.removeEventListener("focus", changed);
    };
  }, []);

  const refresh = async () => {
    const value = await cache.snapshot();
    setSnapshot(value);
    if (dialog.current?.open)
      setContents(value.permission === "granted" ? await cache.contents() : []);
    return value;
  };
  const perform = async (
    operation: () => Promise<unknown>,
    success: string,
  ) => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await operation();
      await refresh();
      setMessage(success);
    } catch (failure) {
      if ((failure as Error).name !== "AbortError")
        setError(String((failure as Error).message || failure));
    } finally {
      setBusy(false);
    }
  };
  const open = () => {
    setContents(undefined);
    if (snapshot) setLimit(String(snapshot.limit / 1024 ** 2));
    setMessage("");
    dialog.current!.showModal();
    void refresh()
      .then((value) => setLimit(String(value.limit / 1024 ** 2)))
      .catch((failure) => setError(String(failure.message)));
    void navigator.storage
      ?.persisted?.()
      .then(setPersistent)
      .catch(() => {});
  };
  const chooseFolder = () => {
    if (!picker) return;
    // Invoke the picker directly from the click, preserving user activation.
    const selection = picker.call(window, {
      mode: "readwrite",
      id: "retex-package-cache",
    });
    void perform(
      async () => cache.useFolder(await selection),
      "Cached packages moved to the selected folder.",
    );
  };
  const accessible = snapshot?.permission === "granted";
  const label = snapshot
    ? accessible
      ? `${formatStorage(snapshot.used)} / ${formatStorage(snapshot.limit)}`
      : snapshot.mode === "folder"
        ? "Reconnect folder"
        : "Unavailable"
    : error
      ? "Unavailable"
      : "Checking…";
  const warning = error || snapshot?.warning;
  return (
    <>
      <button
        id="storage-settings"
        className={`storage-gauge${warning ? " storage-warning" : ""}`}
        onClick={open}
        aria-label={`Package storage: ${label}. Open storage settings`}
        title={warning || `Package cache: ${label}. Customize storage`}
      >
        <span className="storage-gauge-label">
          Storage <span>{label}</span>
        </span>
        <meter
          aria-label="Package cache usage"
          min={0}
          max={snapshot?.limit || 1}
          value={Math.min(snapshot?.used || 0, snapshot?.limit || 1)}
        />
      </button>
      <dialog
        ref={dialog}
        id="storage-dialog"
        className="storage-dialog"
        aria-labelledby="storage-title"
        onCancel={(event) => {
          if (busy) event.preventDefault();
        }}
      >
        <div className="storage-heading">
          <h2 id="storage-title">Package storage</h2>
          <button
            aria-label="Close storage settings"
            disabled={busy}
            onClick={() => dialog.current!.close()}
          >
            ×
          </button>
        </div>
        <p>
          Downloaded TeX packages are reused across your projects. These
          settings manage the package cache; your project files stay separate.
        </p>
        <div className="storage-summary">
          <strong>
            {accessible && snapshot ? formatStorage(snapshot.used) : "—"}
          </strong>
          <span>
            of {snapshot ? formatStorage(snapshot.limit) : "—"} cache limit
          </span>
          <meter
            aria-label="Package cache usage"
            min={0}
            max={snapshot?.limit || 1}
            value={Math.min(snapshot?.used || 0, snapshot?.limit || 1)}
          />
          <span>{snapshot?.count || 0} cached resources</span>
        </div>
        <section
          className="storage-contents"
          aria-labelledby="cache-contents-title"
        >
          <h3 id="cache-contents-title">Cached packages</h3>
          {!accessible ? (
            <p>Reconnect storage to view cached packages.</p>
          ) : contents === undefined ? (
            <p>Loading cached packages…</p>
          ) : contents.length === 0 ? (
            <p>No downloaded packages are cached.</p>
          ) : (
            <ul aria-label="Cached packages">
              {contents.map((item) => (
                <li key={item.id}>
                  <div>
                    <strong>{item.name}</strong>
                    {item.revision && (
                      <small>
                        TeX Live {item.release} · revision {item.revision}
                      </small>
                    )}
                    {item.packages.length > 1 && (
                      <details>
                        <summary>
                          {item.packages.length} included packages
                        </summary>
                        <span>{item.packages.join(", ")}</span>
                      </details>
                    )}
                  </div>
                  <span>{formatStorage(item.size)}</span>
                  <button
                    type="button"
                    aria-label={`Remove ${item.name} from cache`}
                    title={`Remove ${item.name} from cache`}
                    disabled={busy}
                    onClick={() =>
                      void perform(
                        () => cache.remove(item.id),
                        `${item.name} removed from cache.`,
                      )
                    }
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
          <p className="storage-note">
            Some downloads include dependencies. Removing an entry only clears
            cached data; loaded documents keep working.
          </p>
        </section>
        <div className="storage-location">
          <strong>Location</strong>
          <span>
            {snapshot?.mode === "folder"
              ? snapshot.folderName
              : "Browser storage on this device"}
          </span>
          {snapshot?.mode !== "folder" && (
            <small>
              Browser storage can be cleared by the browser or in its site
              settings.
            </small>
          )}
          {snapshot?.mode === "folder" && (
            <small>
              Packages are ordinary files in this dedicated folder. You may need
              to grant access again when returning.
            </small>
          )}
        </div>
        {!accessible && snapshot?.mode === "folder" && (
          <button
            disabled={busy}
            onClick={() =>
              void perform(() => cache.reconnect(), "Folder access checked.")
            }
          >
            Reconnect folder
          </button>
        )}
        <form
          className="storage-limit"
          onSubmit={(event) => {
            event.preventDefault();
            void perform(
              () => cache.setLimit(Math.round(Number(limit) * 1024 ** 2)),
              "Cache limit updated.",
            );
          }}
        >
          <label htmlFor="cache-limit">
            Cache limit (MiB)
            <input
              id="cache-limit"
              type="number"
              min="16"
              step="1"
              required
              value={limit}
              onChange={(event) => setLimit(event.target.value)}
              disabled={busy || !accessible}
            />
          </label>
          <button type="submit" disabled={busy || !accessible}>
            Apply limit
          </button>
        </form>
        <p className="storage-note">
          Least recently used resources are removed as the cache fills. Removed
          packages can be downloaded again.
        </p>
        <div className="storage-actions">
          <button
            disabled={busy || !snapshot || !accessible || !folderSupported}
            onClick={chooseFolder}
          >
            {snapshot?.mode === "folder"
              ? "Move to another folder…"
              : "Move cache to folder…"}
          </button>
          {snapshot?.mode === "folder" && (
            <button
              disabled={busy}
              onClick={() =>
                void perform(
                  () => cache.useBrowser({ copy: accessible }),
                  accessible
                    ? "Cached packages moved to browser storage."
                    : "Using browser storage. Existing folder files were left in place; missing packages will download again.",
                )
              }
            >
              {accessible
                ? "Move to browser storage"
                : "Use browser storage instead"}
            </button>
          )}
          <button
            disabled={busy || !accessible || !snapshot?.count}
            onClick={() =>
              void perform(
                () => cache.clear(),
                "Package cache cleared. Loaded documents remain available in this session.",
              )
            }
          >
            Clear package cache
          </button>
        </div>
        {!folderSupported && (
          <p className="storage-note">
            Choosing a writable cache folder requires a supported browser such
            as Chrome or Edge. Browser storage works in Firefox.
          </p>
        )}
        {snapshot?.mode === "browser" && navigator.storage?.persist && (
          <button
            disabled={busy || persistent}
            onClick={() =>
              void perform(async () => {
                const granted = await navigator.storage.persist();
                setPersistent(granted);
                if (!granted)
                  throw Error(
                    "The browser did not grant persistent storage. Packages remain cached and can be downloaded again if cleared.",
                  );
              }, "Persistent browser storage enabled.")
            }
          >
            {persistent
              ? "Persistent browser storage enabled"
              : "Keep browser storage"}
          </button>
        )}
        {snapshot?.originQuota !== undefined && (
          <p className="storage-note">
            Website browser storage: {formatStorage(snapshot.originUsage || 0)}{" "}
            used / {formatStorage(snapshot.originQuota)} estimated allowance.
            This includes other site data.
          </p>
        )}
        <div className="storage-feedback" aria-live="polite" role="status">
          {busy ? (
            "Updating package storage…"
          ) : warning ? (
            <span className="storage-error">{warning}</span>
          ) : (
            message
          )}
        </div>
      </dialog>
    </>
  );
}
