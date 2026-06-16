import { forwardRef, useId, useState } from "react";

interface AudioPlayerProps {
  src: string;
  captionsSrc?: string;
  captionsLabel?: string;
  captionsLang?: string;
}

export const AudioPlayer = forwardRef<HTMLAudioElement, AudioPlayerProps>(
  function AudioPlayer(
    { src, captionsSrc, captionsLabel = "Captions", captionsLang = "ja" },
    ref,
  ) {
    const [failedSrc, setFailedSrc] = useState<string | null>(null);
    const errorId = useId();
    const loadError = failedSrc === src;

    return (
      <div className="audio-container">
        {/* biome-ignore lint/a11y/useMediaCaption: captions are optional and not available for every audio source */}
        <audio
          ref={ref}
          controls
          preload="metadata"
          aria-label="Meeting audio player"
          aria-describedby={loadError ? errorId : undefined}
          onLoadStart={() => setFailedSrc(null)}
          onLoadedMetadata={() => setFailedSrc(null)}
          onError={() => setFailedSrc(src)}
        >
          <source src={src} type="audio/wav" />
          {captionsSrc ? (
            <track
              kind="captions"
              src={captionsSrc}
              srcLang={captionsLang}
              label={captionsLabel}
            />
          ) : null}
        </audio>
        {loadError ? (
          <div id={errorId} className="audio-load-error" role="alert">
            音声の読み込みに失敗しました。音声ファイルにアクセスできないか、削除されている可能性があります。
          </div>
        ) : null}
      </div>
    );
  },
);
