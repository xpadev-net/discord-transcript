import type { DebugArtifact, DebugCategory } from "../lib/types";

interface DebugDownloadsProps {
  artifacts: DebugArtifact[] | null;
  loading: boolean;
  error: boolean;
}

const CATEGORY_ORDER: DebugCategory[] = [
  "audio",
  "whisper",
  "transcript",
  "prompt",
  "summary",
];

const CATEGORY_LABEL: Record<DebugCategory, string> = {
  audio: "音声",
  whisper: "Whisper レスポンス",
  transcript: "Transcript",
  prompt: "プロンプト",
  summary: "要約モデル出力",
};

export function DebugDownloads({
  artifacts,
  loading,
  error,
}: DebugDownloadsProps) {
  if (loading) {
    return (
      <div className="debug-downloads-section">
        <h3>デバッグデータ</h3>
        <div className="debug-downloads-loading" role="status">
          読み込み中...
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="debug-downloads-section">
        <h3>デバッグデータ</h3>
        <div className="debug-downloads-error" role="alert">
          取得に失敗しました
        </div>
      </div>
    );
  }

  if (!artifacts || artifacts.length === 0) {
    return null;
  }

  const grouped = new Map<DebugCategory, DebugArtifact[]>();
  for (const artifact of artifacts) {
    const list = grouped.get(artifact.category) ?? [];
    list.push(artifact);
    grouped.set(artifact.category, list);
  }

  return (
    <div className="debug-downloads-section">
      <h3>デバッグデータ</h3>
      <p className="debug-downloads-note">
        マスク前の音声・テキストを含む可能性があります。取り扱いに注意してください。
      </p>
      {CATEGORY_ORDER.filter((c) => grouped.has(c)).map((category) => {
        const items = grouped.get(category) ?? [];
        return (
          <div key={category} className="debug-downloads-group">
            <h4 className="debug-downloads-group-title">
              {CATEGORY_LABEL[category]}
            </h4>
            <ul className="debug-downloads-list">
              {items.map((artifact) => (
                <li
                  key={artifact.id}
                  className={
                    artifact.available
                      ? "debug-downloads-item"
                      : "debug-downloads-item is-unavailable"
                  }
                >
                  <span className="debug-downloads-name">{artifact.label}</span>
                  {artifact.available ? (
                    <a
                      href={artifact.download_url}
                      download={artifact.filename}
                      className="debug-downloads-download"
                      aria-label={`${artifact.label}をダウンロード`}
                    >
                      ダウンロード
                    </a>
                  ) : (
                    <span className="debug-downloads-unavailable">未生成</span>
                  )}
                </li>
              ))}
            </ul>
          </div>
        );
      })}
    </div>
  );
}
