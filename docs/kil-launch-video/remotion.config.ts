import { Config } from '@remotion/cli/config';
import { existsSync } from 'node:fs';

Config.setVideoImageFormat('jpeg');
Config.setOverwriteOutput(true);
Config.setCodec('h264');
Config.setCrf(18);

const localChrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
if (existsSync(localChrome)) {
  Config.setBrowserExecutable(localChrome);
}
