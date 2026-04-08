import { forwardRef } from "react";

interface AudioPlayerProps {
  src: string;
}

export const AudioPlayer = forwardRef<HTMLAudioElement, AudioPlayerProps>(
  ({ src }, ref) => {
    return (
      <div className="audio-container">
        <audio ref={ref} controls preload="metadata">
          <source src={src} type="audio/wav" />
        </audio>
      </div>
    );
  },
);
