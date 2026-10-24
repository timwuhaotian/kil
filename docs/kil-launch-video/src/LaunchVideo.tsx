import React from 'react';
import { AbsoluteFill, useCurrentFrame } from 'remotion';
import { BeforeAfter, EndCard, Features, Hook, Reveal, Size, T } from './scenes';
import { C, ease } from './theme';

type SceneDef = { id: string; start: number; node: React.ReactNode };

const SCENES: SceneDef[] = [
  { id: 'hook', start: T.hook, node: <Hook /> },
  { id: 'reveal', start: T.reveal, node: <Reveal /> },
  { id: 'beforeafter', start: T.ba, node: <BeforeAfter /> },
  { id: 'features', start: T.feat, node: <Features /> },
  { id: 'size', start: T.size, node: <Size /> },
  { id: 'end', start: T.end, node: <EndCard /> },
];

/**
 * Transitions happen here, not inside the scenes: the outgoing scene is covered
 * by black before the incoming one is readable, so two busy screens never
 * dissolve into each other.
 */
export const LaunchVideo: React.FC = () => {
  const frame = useCurrentFrame();

  return (
    <AbsoluteFill style={{ background: C.bg }}>
      {SCENES.map((scene) => {
        if (frame < scene.start - 1) return null;
        const bgIn = ease(frame, scene.start, 6);
        const contentIn = ease(frame, scene.start, 12);
        return (
          <AbsoluteFill key={scene.id} style={{ background: C.bg, opacity: bgIn }}>
            <AbsoluteFill style={{ opacity: contentIn }}>{scene.node}</AbsoluteFill>
          </AbsoluteFill>
        );
      })}
    </AbsoluteFill>
  );
};
