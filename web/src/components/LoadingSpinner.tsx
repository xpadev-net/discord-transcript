export function LoadingSpinner({ text }: { text: string }) {
  return (
    <div className="loading" role="status" aria-live="polite">
      <div className="loading-spinner" />
      <span>{text}</span>
    </div>
  );
}
