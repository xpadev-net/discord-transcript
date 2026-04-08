export function LoadingSpinner({ text }: { text: string }) {
  return (
    <div className="loading">
      <div className="loading-spinner" />
      <span>{text}</span>
    </div>
  );
}
