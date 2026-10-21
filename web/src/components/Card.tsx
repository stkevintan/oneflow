import type { ReactNode } from "react";

interface CardProps {
  title: string;
  badge?: ReactNode;
  hint?: string;
  children: ReactNode;
}

export function Card({ title, badge, hint, children }: CardProps) {
  return (
    <section className="rounded-xl border border-slate-800 bg-slate-900/60 p-5 shadow-lg shadow-black/20">
      <div className="mb-4 flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <h2 className="flex items-center gap-2 text-sm font-semibold uppercase tracking-wider text-slate-200">
          {title}
          {badge}
        </h2>
        {hint ? <span className="text-xs text-slate-500">{hint}</span> : null}
      </div>
      {children}
    </section>
  );
}

interface EmptyStateProps {
  visible: boolean;
  children: ReactNode;
}

export function EmptyState({ visible, children }: EmptyStateProps) {
  if (!visible) {
    return null;
  }
  return <p className="mt-3 text-sm text-slate-500">{children}</p>;
}
