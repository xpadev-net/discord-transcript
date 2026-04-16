import { forwardRef } from "react";

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
    return (
      <div className="audio-container">
        {/* biome-ignore lint/a11y/useMediaCaption: captions are optional and not available for every audio source */}
        <audio
          ref={ref}
          controls
          preload="metadata"
          aria-label="Meeting audio player"
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
      </div>
    );
  },
);
