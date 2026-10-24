import React from 'react';
import { Composition } from 'remotion';
import { LaunchVideo } from './LaunchVideo';
import { T } from './scenes';

export const RemotionRoot: React.FC = () => {
  return (
    <>
      <Composition
        id="LaunchVideo"
        component={LaunchVideo}
        durationInFrames={T.total}
        fps={30}
        width={1920}
        height={1080}
      />
    </>
  );
};
