import type { Components } from "react-markdown";
import Markdown from "react-markdown";
import { LoadingSpinner } from "./LoadingSpinner";

interface Props {
  markdown: string | null | undefined;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
}

function safeSummaryHref(href: string | undefined): string | null {
  const trimmed = href?.trim();
  if (!trimmed) {
    return null;
  }
  if (trimmed.startsWith("#")) {
    return trimmed;
  }

  try {
    const currentOrigin =
      typeof window === "undefined"
        ? "http://localhost"
        : window.location.origin;
    const url = new URL(trimmed, currentOrigin);
    if (
      (url.protocol === "http:" || url.protocol === "https:") &&
      url.origin === currentOrigin
    ) {
      return trimmed;
    }
  } catch {
    return null;
  }

  return null;
}

const summaryMarkdownComponents: Components = {
  a({ children, href }) {
    const safeHref = safeSummaryHref(href);
    if (!safeHref) {
      return <span>{children}</span>;
    }

    return (
      <a href={safeHref} rel="noreferrer noopener">
        {children}
      </a>
    );
  },
  img({ alt }) {
    return alt ? <span>{alt}</span> : null;
  },
};

export function SummaryPanel({ markdown, loading, error, onRetry }: Props) {
  return (
    <div className="right-panel">
      <div className="summary-header">{"\u30b5\u30de\u30ea\u30fc"}</div>
      <div>
        {error ? (
          <div className="panel-error" role="alert">
            <div>{error}</div>
            <button type="button" onClick={onRetry}>
              {"\u518d\u8a66\u884c"}
            </button>
          </div>
        ) : loading ? (
          <LoadingSpinner text={"\u8aad\u307f\u8fbc\u307f\u4e2d..."} />
        ) : markdown ? (
          <div className="summary-content">
            <Markdown components={summaryMarkdownComponents}>
              {markdown}
            </Markdown>
          </div>
        ) : (
          <div className="empty-state">
            {
              "\u30b5\u30de\u30ea\u30fc\u306f\u307e\u3060\u5229\u7528\u3067\u304d\u307e\u305b\u3093"
            }
          </div>
        )}
      </div>
    </div>
  );
}
