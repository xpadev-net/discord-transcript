import { forwardRef } from "react";

interface AudioPlayerProps {
  src: string;
}

export const AudioPlayer = forwardRef<HTMLAudioElement, AudioPlayerProps>(
  function AudioPlayer({ src }, ref) {
    return (
      <div className="audio-container">
        {/* biome-ignore lint/a11y/useMediaCaption: no caption track is available for this audio source */}
        <audio ref={ref} controls preload="metadata">
          <source src={src} type="audio/wav" />
        </audio>
      </div>
    );
  },
);
