import { useEffect, useState } from "react";
import { invoke, isNative } from "../runtime";
import { platform } from "../api";

type DownloadStatus = { phase: string; name: string; percent: number; bytes: number; message: string };
const active = (phase: string) => phase === "preparing" || phase === "downloading";
export default function DownloadPanel() {
  const [status, setStatus] = useState<DownloadStatus | null>(null);
  const [hidden, setHidden] = useState(false);
  useEffect(() => {
    if (!isNative || platform !== "windows") return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      clearTimeout(timer);
      try {
        const next = await invoke<DownloadStatus>("download_status");
        if (disposed) return;
        setStatus(next);
        if (active(next.phase)) timer = setTimeout(poll, 1000);
      } catch { /* App is exiting or unavailable. */ }
    };
    const refresh = () => { setHidden(false); void poll(); };
    window.addEventListener("hghub-download-change", refresh);
    void poll();
    return () => { disposed = true; clearTimeout(timer); window.removeEventListener("hghub-download-change", refresh); };
  }, []);
  if (!status || status.phase === "idle" || hidden) return null;
  const running = active(status.phase);
  return (
    <aside aria-label="下载任务" aria-live="polite" className="fixed right-4 bottom-20 z-50 w-80 max-w-[calc(100vw-2rem)] rounded-xl border border-white/15 bg-s3 p-4 shadow-xl text-[12px]">
      <div className="flex items-center justify-between gap-3">
        <strong>{status.phase === "preparing" ? "准备下载…" : status.phase === "downloading" ? `下载中 · ${Math.floor(status.percent)}%` : status.phase === "completed" ? "下载完成" : status.phase === "cancelled" ? "下载已取消" : "下载失败"}</strong>
        {running
          ? <button className="hg-link" onClick={() => void invoke("download_cancel")}>取消</button>
          : <button aria-label="关闭下载提示" className="hg-link" onClick={() => setHidden(true)}>关闭</button>}
      </div>
      <p className="mt-2 truncate hg-ink-2" title={status.name}>{status.name}</p>
      {running && <progress aria-label="下载进度" max={100} value={status.percent} className="mt-2 w-full" />}
      {status.bytes > 0 && <p className="mt-1 hg-ink-3">{(status.bytes / 1024 / 1024).toFixed(1)} MB</p>}
      {status.message && <p className="mt-2 hg-ink-2">{status.message}</p>}
      {status.phase === "completed" && <button className="hg-link mt-2" onClick={() => void invoke("download_open_folder")}>打开保存文件夹</button>}
    </aside>
  );
}
