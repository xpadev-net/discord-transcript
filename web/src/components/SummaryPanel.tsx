import Markdown from "react-markdown";
import { LoadingSpinner } from "./LoadingSpinner";

interface Props {
  markdown: string | null | undefined;
  loading: boolean;
}

export function SummaryPanel({ markdown, loading }: Props) {
  return (
    <div className="right-panel">
      <div className="summary-header">{"\u30b5\u30de\u30ea\u30fc"}</div>
      <div>
        {loading ? (
          <LoadingSpinner text={"\u8aad\u307f\u8fbc\u307f\u4e2d..."} />
        ) : markdown ? (
          <div className="summary-content">
            <Markdown>{markdown}</Markdown>
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
