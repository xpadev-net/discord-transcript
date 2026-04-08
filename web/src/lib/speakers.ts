const SPEAKER_COLORS = [
  "var(--speaker-1)",
  "var(--speaker-2)",
  "var(--speaker-3)",
  "var(--speaker-4)",
  "var(--speaker-5)",
  "var(--speaker-6)",
  "var(--speaker-7)",
  "var(--speaker-8)",
  "var(--speaker-9)",
  "var(--speaker-10)",
];

const colorMap = new Map<string, string>();

export function getSpeakerColor(speakerId: string): string {
  const cached = colorMap.get(speakerId);
  if (cached) return cached;

  let hash = 0;
  for (let i = 0; i < speakerId.length; i++) {
    hash = ((hash << 5) - hash) + speakerId.charCodeAt(i);
    hash = hash & hash; // Convert to 32-bit integer
  }
  const color = SPEAKER_COLORS[Math.abs(hash) % SPEAKER_COLORS.length];
  colorMap.set(speakerId, color);
  return color;
}
