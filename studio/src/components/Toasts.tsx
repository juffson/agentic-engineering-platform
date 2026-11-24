import { AlertCircle, CheckCircle2, X } from "lucide-react";
import { useStudio } from "../state/store";

export function Toasts() {
  const { toasts, dismiss } = useStudio();
  if (toasts.length === 0) return null;
  return (
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`toast toast-${t.tone}`}>
          {t.tone === "error" ? <AlertCircle size={15} /> : <CheckCircle2 size={15} />}
          <span className="toast-text">{t.text}</span>
          <button type="button" className="icon-btn" onClick={() => dismiss(t.id)} aria-label="关闭提示">
            <X size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}
