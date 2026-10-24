import React from 'react';
import { AbsoluteFill, interpolate, spring, staticFile, useCurrentFrame, useVideoConfig } from 'remotion';
import { AppleTerminal, Caption, Headline, KilApp, Kbd, Line, Pill } from './parts';
import { C, MONO, SANS, ease } from './theme';

/** Scene start frames. */
export const T = { hook: 0, reveal: 62, ba: 146, feat: 376, size: 542, end: 634, total: 740 };

const GIT_LOG: Line[] = [
  { t: '~/code/kil ❯ git log --oneline --decorate -32 | cat' },
  { t: '* 621cec2 (HEAD -> main) Kil 0.0.16: The New Tab button lines up with the tabs' },
  { t: '* 8d6497d Kil 0.0.15: A /kil-config skill for coding agents working in the repo' },
  { t: '* 3e51de6 Kil 0.0.14: Clicking a notification goes to the pane that sent it' },
  { t: '* 93423aa Kil 0.0.13: Cmd-V with an image on the clipboard reaches coding agents', c: '#e6c07b' },
  { t: '* ef42f40 Kil 0.0.12: Restart to Update works from the menu, no downgrade on quit' },
  { t: '* 0e826f5 Kil 0.0.11: notifications from programs, with a click going to the pane' },
  { t: '* 3a08c9c Restart to Update works from the Kil menu, and quitting never installs', c: '#e6c07b' },
  { t: '* 6437485 Test that OSC 9 and 777 from a real program arrive as notifications', c: '#56b6c2' },
  { t: '* 8f405e3 Kil 0.0.4: reopened jobs by the line zsh ran' },
  { t: '* e7e6095 Reopen after any exit: crash-safe saving' },
  { t: '* 631b527 Restore tabs, splits and running jobs' },
  { t: '* 9b807aa Tab job status, exit and signal-mask hardening', c: '#98c379' },
  { t: '* f38579e Kil: native macOS terminal in Rust' },
  { t: '~/code/kil ❯ ' },
];

const CODE_PANE: Line[] = [
  { t: ' 85    if mode.sgr_mouse() {', c: '#abb2bf' },
  { t: ' 86        return Some(format!("\\x1b[<{button};{x};{y}M").into_bytes());' },
  { t: ' 87    }' },
  { t: ' 88    let mut bytes = b"\\x1b[M".to_vec();', c: '#61afef' },
  { t: ' 89    for value in [button, x, y] {', c: '#abb2bf' },
  { t: ' 90        let value = value + 32;' },
  { t: ' 91        if mode.utf8_mouse() {', c: '#c678dd' },
  { t: ' 92            let mut buf = [0; 4];', c: '#abb2bf' },
  { t: ' 93            bytes.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut buf).as_bytes());', c: '#e5c07b' },
  { t: ' 94        } else {', c: '#c678dd' },
  { t: ' 95            bytes.push(u8::try_from(value).ok()?);' },
  { t: ' 96        }' },
  { t: ' 97    }' },
  { t: ' 98    Some(bytes)' },
  { t: '"crates/kil/src/keys.rs" 359L, 12307B' },
];

const BUILD_LINES: Line[] = [
  { t: '~/code/kil ❯ cargo build --release' },
  { t: '   Compiling 412 crates ...' },
  { t: '   Building kil-term v0.0.16' },
  { t: '   Building kil v0.0.16' },
];

const PROGRESS_LINES: Line[] = [
  { t: '   Compiling 413 crates ...' },
  { t: '   Compiling 414 crates ...' },
];

const KILL_LINES: Line[] = [
  { t: '[1]  9873  cargo build --release', c: C.failure },
  { t: 'zsh: killed', c: C.failure },
  { t: 'error: build failed', c: C.failure },
];

const COMPILING: string[] = [
  '   Compiling proc-macro2 v1.0.107',
  '   Compiling unicode-ident v1.0.27',
  '   Compiling serde v1.0.229',
  '   Compiling alacritty_terminal v0.26.0',
  '   Compiling rustybuzz v0.20.1',
  '   Compiling gpui v0.2.2',
  '   Compiling vte v0.15.0',
  '   Compiling kil-term v0.0.16',
  '   Compiling kil v0.0.16',
];

const TABS = [
  { name: 'kil', dir: '~/code/kil' },
  { name: 'server', dir: '~/code/kil/crates/kil-term' },
  { name: 'code', dir: '~/code' },
];

const BG: React.CSSProperties = {
  background: `radial-gradient(1200px 700px at 78% 10%, #16202e 0%, ${C.bg} 62%)`,
};

/* ---------------------------------------------------------------- 1. hook */

export const Hook: React.FC = () => {
  const frame = useCurrentFrame();
  const cardIn = ease(frame, 2, 14);
  const killIn = ease(frame, 20, 8);
  const headIn = ease(frame, 0, 12);
  const subIn = ease(frame, 34, 12);

  return (
    <AbsoluteFill
      style={{
        ...BG,
        alignItems: 'center',
        justifyContent: 'center',
        flexDirection: 'column',
        gap: 40,
      }}
    >
      <div style={{ opacity: headIn, transform: `translateY(${(1 - headIn) * 18}px)` }}>
        <Headline align="center" size={68} title="You closed the window." />
      </div>

      <div
        style={{
          width: 1000,
          borderRadius: 16,
          border: '1px solid #23272e',
          background: '#0e1013',
          padding: '30px 38px',
          fontFamily: MONO,
          fontSize: 26,
          lineHeight: 1.55,
          opacity: cardIn,
          transform: `translateY(${(1 - cardIn) * 22}px)`,
          boxShadow: '0 40px 120px rgba(0,0,0,0.5)',
        }}
      >
        {BUILD_LINES.map((l, i) => (
          <div key={i} style={{ color: l.c ?? C.text }}>
            {l.t}
          </div>
        ))}
        <div style={{ height: 18 }} />
        <div>
          {KILL_LINES.map((l, i) => (
            <div
              key={i}
              style={{
                color: l.c ?? C.text,
                opacity: ease(frame, 20 + i * 4, 8),
                transform: `translateY(${(1 - ease(frame, 20 + i * 4, 8)) * 8}px)`,
              }}
            >
              {l.t}
            </div>
          ))}
        </div>
      </div>

      <div
        style={{
          opacity: subIn,
          transform: `translateY(${(1 - subIn) * 14}px)`,
        }}
      >
        <Headline align="center" size={50} color={C.failure} title="So did the build." />
      </div>
    </AbsoluteFill>
  );
};

/* -------------------------------------------------------------- 2. reveal */

export const Reveal: React.FC = () => {
  const frame = useCurrentFrame() - T.reveal;
  const { fps } = useVideoConfig();
  const intro = spring({ frame: frame - 2, fps, config: { damping: 200, mass: 0.9 } });
  const headIn = ease(frame, 4, 12);

  return (
    <AbsoluteFill style={BG}>
      <div
        style={{
          position: 'absolute',
          inset: 0,
          transform: `scale(${0.965 + intro * 0.035})`,
          opacity: intro,
        }}
      >
        <KilApp
          width={1920}
          height={1080}
          tabs={TABS}
          active={0}
          panes={[GIT_LOG]}
          fontSize={27}
          paneTop={300}
          cursor
        />
      </div>

      <div
        style={{
          position: 'absolute',
          top: 58,
          left: 360,
          right: 0,
          height: 380,
          background:
            'linear-gradient(180deg, rgba(8,10,13,0.97) 0%, rgba(8,10,13,0.72) 45%, rgba(8,10,13,0) 100%)',
        }}
      />

      <div
        style={{
          position: 'absolute',
          top: 96,
          left: 390,
          right: 0,
          opacity: headIn,
          transform: `translateY(${(1 - headIn) * 20}px)`,
        }}
      >
        <div style={{ display: 'flex', alignItems: 'baseline', gap: 26 }}>
          <div
            style={{
              fontSize: 104,
              fontWeight: 800,
              letterSpacing: -3.5,
              color: C.text,
              fontFamily: SANS,
            }}
          >
            Kil
          </div>
          <div style={{ fontSize: 42, color: C.accent, fontWeight: 600, letterSpacing: -0.8 }}>
            A native macOS terminal in Rust
          </div>
        </div>
        <div style={{ fontSize: 30, color: C.muted, marginTop: 10, letterSpacing: -0.4 }}>
          Vertical tabs · Split panes · Jobs that outlive the window
        </div>
      </div>
    </AbsoluteFill>
  );
};

/* -------------------------------------------------------- 3. before/after */

export const BeforeAfter: React.FC = () => {
  const frame = useCurrentFrame() - T.ba;
  const { fps } = useVideoConfig();
  const after = spring({ frame: frame - 6, fps, config: { damping: 16, mass: 0.7 } });
  const headIn = ease(frame, 2, 12);
  const phase2 = ease(frame, 100, 14);
  const killIn = ease(frame, 8, 8);

  if (phase2 > 0) {
    const restored = spring({ frame: frame - 100, fps, config: { damping: 200 } });
    const restoredPane: Line[] = [
      { t: '~/code/kil ❯ cargo build --release' },
      { t: '   Compiling 412 crates ...' },
      { t: '   Compiling 413 crates ...' },
      { t: '   Compiling 414 crates ...' },
      { t: '    Finished `release` profile in 4m 12s', c: '#98c379' },
      { t: '~/code/kil ❯ ' },
    ];
    return (
      <AbsoluteFill style={BG}>
        <div
          style={{
            position: 'absolute',
            inset: 0,
            transform: `scale(${0.97 + restored * 0.03})`,
            opacity: restored,
          }}
        >
          <KilApp
            width={1920}
            height={1080}
            tabs={[
              { name: 'kil', dir: '~/code/kil' },
              { name: 'server', dir: '~/code/kil/crates/kil-term', state: 'running' },
              { name: 'code', dir: '~/code' },
            ]}
            active={0}
            panes={[restoredPane, CODE_PANE]}
            split={0.5}
            fontSize={22}
            paneTop={0}
            paneAlign="center"
          />
        </div>
        <div
          style={{
            position: 'absolute',
            top: 58,
            left: 360,
            right: 0,
            height: 260,
            background:
              'linear-gradient(180deg, rgba(8,10,13,0.96) 0%, rgba(8,10,13,0.6) 50%, rgba(8,10,13,0) 100%)',
          }}
        />
        <Pill text="the server kept running" style={{ bottom: 92, right: 120 }} />
        <div style={{ position: 'absolute', top: 84, left: 390, right: 120, opacity: phase2 }}>
          <div style={{ fontSize: 54, fontWeight: 700, letterSpacing: -1.4, color: C.text }}>
            Reopen Kil. Same tabs, same panes, same processes.
          </div>
          <div style={{ fontSize: 30, color: C.muted, marginTop: 12 }}>
            The build finished while you were gone.
          </div>
        </div>
      </AbsoluteFill>
    );
  }

  const printed = Math.min(PROGRESS_LINES.length, Math.max(0, Math.floor((frame - 26) / 10)));
  const kilPane: Line[] = [BUILD_LINES[0], BUILD_LINES[1], BUILD_LINES[2], ...PROGRESS_LINES.slice(0, printed)];

  return (
    <AbsoluteFill
      style={{
        ...BG,
        alignItems: 'center',
        justifyContent: 'center',
        flexDirection: 'column',
        gap: 46,
      }}
    >
      <div style={{ opacity: headIn }}>
        <Headline
          align="center"
          size={58}
          title="Same build. Two terminals."
          sub="Only one of them keeps it."
        />
      </div>

      <div style={{ display: 'flex', gap: 70, alignItems: 'flex-start' }}>
        <div style={{ opacity: 1 - after * 0.25 }}>
          <AppleTerminal
            width={860}
            height={500}
            fontSize={24}
            lines={[...BUILD_LINES, ...(killIn > 0 ? KILL_LINES : [])]}
          />
          <div
            style={{
              marginTop: 26,
              textAlign: 'center',
              fontSize: 30,
              color: C.muted,
              opacity: killIn,
            }}
          >
            The window closed. The job died with it.
          </div>
        </div>

        <div
          style={{
            position: 'relative',
            opacity: ease(frame, 0, 10),
            transform: `translateY(${(1 - after) * -40}px) scale(${0.94 + after * 0.06})`,
          }}
        >
          <Pill text="still running" style={{ top: -66, right: 6 }} />
          <KilApp
            width={860}
            height={500}
            tabs={[{ name: 'kil', dir: '~/code/kil', state: 'running' }]}
            active={0}
            panes={[kilPane]}
            fontSize={22}
            radius={12}
            shadow
          />
          <div style={{ marginTop: 26, textAlign: 'center', fontSize: 30, color: C.muted }}>
            Kil quit. The job keeps going.
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};

/* ------------------------------------------------------------- 4. features */

const BEAT_START = [0, 78, 122];
const BEAT_LEN = [78, 44, 44];

export const Features: React.FC = () => {
  const frame = useCurrentFrame() - T.feat;
  const { fps } = useVideoConfig();

  const beat = frame < BEAT_START[1] ? 0 : frame < BEAT_START[2] ? 1 : 2;
  const beatStart = BEAT_START[beat];

  const openSpring = spring({
    frame: frame - (BEAT_START[1] + 4),
    fps,
    config: { damping: 16, mass: 0.8 },
  });
  const closeSpring = spring({
    frame: frame - (BEAT_START[2] + 4),
    fps,
    config: { damping: 16, mass: 0.8 },
  });
  const split =
    frame < BEAT_START[2]
      ? interpolate(openSpring, [0, 1], [1, 0.55])
      : interpolate(closeSpring, [0, 1], [0.55, 1]);

  const capIn = ease(frame, beatStart + (beat === 0 ? 22 : 6), 12);
  const capOut = 1 - ease(frame, beatStart + BEAT_LEN[beat] - 8, 8);

  const finished = beat === 0 && frame >= 36;
  const shown =
    beat === 0
      ? Math.max(0, Math.min(COMPILING.length, Math.floor((frame - 8) / 3)))
      : COMPILING.length;

  const tabs =
    beat === 0
      ? [
          { name: 'kil', dir: '~/code/kil' },
          { name: 'server', dir: '~/code/kil/crates/kil-term' },
          {
            name: 'code',
            dir: '~/code',
            state: finished ? ('done' as const) : ('running' as const),
          },
        ]
      : beat === 2
        ? [
            { name: 'kil', dir: '~/code/kil', mem: '142 MB' },
            { name: 'server', dir: '~/code/kil/crates/kil-term' },
            { name: 'code', dir: '~/code' },
          ]
        : TABS;

  const panes =
    beat === 0
      ? [
          [
            { t: '~/code ❯ cargo build --release' },
            ...COMPILING.slice(0, shown).map((t) => ({ t })),
            ...(finished
              ? [{ t: '    Finished `release` profile in 12.4s', c: '#98c379' } as Line]
              : []),
            ...(finished ? [{ t: '~/code ❯ ' }] : []),
          ],
        ]
      : [GIT_LOG, CODE_PANE];

  const caption =
    beat === 0 ? (
      'A dot marks the tab whose job finished while you were away.'
    ) : beat === 1 ? (
      <>
        Split panes that animate open and closed.   <Kbd>⌘⇧D</Kbd> right · <Kbd>⌘D</Kbd> down
      </>
    ) : (
      'See what each tab’s processes cost in RAM.'
    );

  return (
    <AbsoluteFill style={BG}>
      <KilApp
        width={1920}
        height={1080}
        tabs={tabs}
        active={beat === 0 ? 2 : 0}
        panes={panes}
        split={split}
        fontSize={beat === 1 ? 19 : 27}
        paneTop={beat === 0 ? 170 : 30}
        paneAlign={beat === 0 ? 'top' : 'center'}
        footer={beat === 2 ? 'Tabs 275 MB' : undefined}
      />
      <div
        style={{
          position: 'absolute',
          top: 0,
          right: 0,
          bottom: 0,
          left: 360,
          background:
            'linear-gradient(0deg, rgba(8,10,13,0.94) 0%, rgba(8,10,13,0.45) 20%, rgba(8,10,13,0) 34%)',
        }}
      />
      <div style={{ opacity: capIn * capOut }}>
        <Caption>{caption}</Caption>
      </div>
    </AbsoluteFill>
  );
};

/* ----------------------------------------------------------------- 5. size */

const SIZES: [string, number][] = [
  ['Kil', 8.6],
  ['Alacritty', 14.7],
  ['Ghostty', 65.0],
  ['iTerm2', 202.9],
  ['Hyper', 240.9],
  ['kitty', 253.0],
  ['WezTerm', 266.1],
  ['Tabby', 439.6],
  ['Wave', 556.1],
  ['Warp', 913.7],
];

export const Size: React.FC = () => {
  const frame = useCurrentFrame() - T.size;
  const headIn = ease(frame, 2, 14);

  return (
    <AbsoluteFill style={{ ...BG, padding: '80px 160px 70px', flexDirection: 'column' }}>
      <div style={{ opacity: headIn, fontSize: 54, fontWeight: 700, letterSpacing: -1.4, color: C.text }}>
        The smallest macOS terminal with tabs and split panes.
      </div>

      <div style={{ flex: 1, display: 'flex', alignItems: 'center', gap: 60 }}>
        <div style={{ width: 470, opacity: headIn }}>
          <div
            style={{
              fontSize: 132,
              fontWeight: 800,
              letterSpacing: -5,
              color: C.accent,
              lineHeight: 1,
              whiteSpace: 'nowrap',
              fontFamily: SANS,
            }}
          >
            8.6 MB
          </div>
          <div style={{ fontSize: 32, color: C.text, marginTop: 12, fontWeight: 600 }}>
            on disk, installed.
          </div>
          <div style={{ fontSize: 27, color: C.muted, marginTop: 16, lineHeight: 1.5 }}>
            106× smaller than Warp. Alacritty is the only other one close in size — and it has no
            tabs.
          </div>
        </div>

        <div style={{ flex: 1, opacity: headIn }}>
          <div style={{ fontSize: 24, color: C.muted, marginBottom: 14, paddingLeft: 183 }}>
            Installed size, MB
          </div>
          {SIZES.map(([name, mb], i) => {
            const w = ease(frame, 8 + i * 2, 20);
            const isKil = name === 'Kil';
            return (
              <div
                key={name}
                style={{ display: 'flex', alignItems: 'center', gap: 18, height: 62, marginBottom: 6 }}
              >
                <div
                  style={{
                    width: 165,
                    fontSize: 26,
                    fontFamily: SANS,
                    color: isKil ? C.accent : C.muted,
                    fontWeight: isKil ? 700 : 500,
                    textAlign: 'right',
                    whiteSpace: 'nowrap',
                  }}
                >
                  {name}
                </div>
                <div
                  style={{
                    height: isKil ? 32 : 22,
                    width: Math.max(8, (mb / 913.7) * 700 * w),
                    background: isKil ? C.accent : '#3a4049',
                    borderRadius: 5,
                    boxShadow: isKil ? `0 0 34px ${C.accent}66` : undefined,
                  }}
                />
                <div
                  style={{
                    fontSize: 26,
                    fontFamily: MONO,
                    color: isKil ? C.text : C.muted,
                    fontWeight: isKil ? 700 : 500,
                    whiteSpace: 'nowrap',
                    opacity: Math.min(1, w * 2),
                  }}
                >
                  {mb.toFixed(1)} MB
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </AbsoluteFill>
  );
};

/* ------------------------------------------------------------- 6. endcard */

export const EndCard: React.FC = () => {
  const frame = useCurrentFrame() - T.end;
  const inT = ease(frame, 4, 18);
  const cta = ease(frame, 24, 14);

  return (
    <AbsoluteFill style={{ ...BG, alignItems: 'center', justifyContent: 'center' }}>
      <div
        style={{
          position: 'absolute',
          width: 1100,
          height: 1100,
          borderRadius: 550,
          background: 'radial-gradient(circle, rgba(108,168,255,0.16) 0%, rgba(108,168,255,0) 65%)',
        }}
      />
      <div
        style={{
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          opacity: inT,
          transform: `translateY(${(1 - inT) * 22}px)`,
        }}
      >
        <img src={staticFile('icon.png')} style={{ width: 146, height: 146, borderRadius: 34 }} />
        <div
          style={{
            fontSize: 112,
            fontWeight: 800,
            letterSpacing: -4,
            marginTop: 22,
            color: C.text,
            fontFamily: SANS,
          }}
        >
          Kil
        </div>
        <div style={{ fontSize: 38, color: C.muted, marginTop: 18, letterSpacing: -0.8 }}>
          A native macOS terminal in Rust
        </div>
        <div
          style={{
            marginTop: 40,
            opacity: cta,
            transform: `translateY(${(1 - cta) * 14}px)`,
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'center',
            gap: 22,
          }}
        >
          <div
            style={{
              padding: '20px 44px',
              borderRadius: 14,
              background: C.accent,
              color: '#08111d',
              fontSize: 34,
              fontWeight: 700,
              letterSpacing: -0.6,
            }}
          >
            Download Kil
          </div>
          <div style={{ fontSize: 28, color: C.muted }}>
            Free · 8.6 MB · Apple silicon · macOS 14+
          </div>
          <div style={{ fontSize: 30, color: C.accent, fontFamily: SANS, letterSpacing: -0.4 }}>
            github.com/timwuhaotian/kil
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};
